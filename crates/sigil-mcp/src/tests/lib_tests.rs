use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use serde_json::{Value, json};
use sigil_kernel::{
    ApprovalMode, DurableEventType, ExtensionProcessLaunchPhase, ExtensionProcessLifecycleAudit,
    ExtensionProcessLifecycleStatus, JsonlSessionStore, McpServerConfig, McpServerStartup,
    McpServerTrustPolicy, McpTrustClass, MutationEventRecorder, ProviderCapabilities,
    ReasoningStreamSupport, SecretRedactor, SecretString, ToolAccess, ToolCategory, ToolContext,
    ToolErrorKind, ToolRegistry, ToolResultStatus, ToolSubject, ToolSubjectKind, ToolSubjectScope,
    WorkspaceMutationDetected, WorkspaceMutationDetectionReason,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{ChildStdout, Command},
};

use super::{
    ExtensionProcessNetworkAdmission, LocalMcpProcessLauncher, McpElicitationHandler,
    McpElicitationRequest, McpElicitationResponse, McpListChangedKind, McpListChangedNotification,
    McpProcessClass, McpProcessCoverage, McpProcessLaunchRequest, McpProcessLauncher,
    McpProgressNotification, McpPromptToolKind, McpRuntimeEventHandler, McpToolRegistrationOptions,
    activate_lazy_mcp_tools, register_mcp_tools, register_mcp_tools_with_options,
    unsupported_mcp_runtime_event_handler,
};

macro_rules! set_mcp_server_config_field {
    ($config:ident, command, $value:expr) => {
        let sigil_kernel::McpServerTransportConfig::Stdio { command, .. } = &mut $config.transport
        else {
            panic!("test MCP config must use stdio transport");
        };
        *command = $value;
    };
    ($config:ident, args, $value:expr) => {
        let sigil_kernel::McpServerTransportConfig::Stdio { args, .. } = &mut $config.transport
        else {
            panic!("test MCP config must use stdio transport");
        };
        *args = $value;
    };
    ($config:ident, inherit_env, $value:expr) => {
        let sigil_kernel::McpServerTransportConfig::Stdio { inherit_env, .. } =
            &mut $config.transport
        else {
            panic!("test MCP config must use stdio transport");
        };
        *inherit_env = $value;
    };
    ($config:ident, $field:ident, $value:expr) => {
        $config.$field = $value;
    };
}

macro_rules! mcp_server_config {
    (.. $base:expr $(,)?) => {{
        let base: McpServerConfig = $base;
        base
    }};
    ($field:ident: $value:expr, $($rest:tt)*) => {{
        let mut config = mcp_server_config!($($rest)*);
        set_mcp_server_config_field!(config, $field, $value);
        config
    }};
    ($field:ident, $($rest:tt)*) => {{
        let mut config = mcp_server_config!($($rest)*);
        set_mcp_server_config_field!(config, $field, $field);
        config
    }};
    ($field:ident: $value:expr $(,)?) => {{
        let mut config = McpServerConfig::default();
        set_mcp_server_config_field!(config, $field, $value);
        config
    }};
    ($field:ident $(,)?) => {{
        let mut config = McpServerConfig::default();
        set_mcp_server_config_field!(config, $field, $field);
        config
    }};
}

async fn register_mcp_tools_with_capabilities(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
) -> Result<()> {
    register_mcp_tools_with_options(
        registry,
        servers,
        McpToolRegistrationOptions::eager()?.with_capabilities(capabilities),
    )
    .await
}

async fn register_mcp_tools_with_capabilities_and_roots(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
    roots: Vec<std::path::PathBuf>,
) -> Result<()> {
    register_mcp_tools_with_options(
        registry,
        servers,
        McpToolRegistrationOptions::eager()?
            .with_capabilities(capabilities)
            .with_roots(roots),
    )
    .await
}

async fn register_mcp_tools_with_capabilities_roots_and_secrets(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
    roots: Vec<std::path::PathBuf>,
    secret_redactor: SecretRedactor,
) -> Result<()> {
    register_mcp_tools_with_options(
        registry,
        servers,
        McpToolRegistrationOptions::eager()?
            .with_capabilities(capabilities)
            .with_roots(roots)
            .with_secret_redactor(secret_redactor),
    )
    .await
}

async fn register_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
    roots: Vec<std::path::PathBuf>,
    secret_redactor: SecretRedactor,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<()> {
    register_mcp_tools_with_options(
        registry,
        servers,
        McpToolRegistrationOptions::eager()?
            .with_capabilities(capabilities)
            .with_roots(roots)
            .with_secret_redactor(secret_redactor)
            .with_elicitation_handler(elicitation_handler),
    )
    .await
}

async fn activate_lazy_mcp_tools_with_capabilities_roots_and_secrets(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
    roots: Vec<std::path::PathBuf>,
    secret_redactor: SecretRedactor,
) -> Result<()> {
    register_mcp_tools_with_options(
        registry,
        servers,
        McpToolRegistrationOptions::lazy()?
            .with_capabilities(capabilities)
            .with_roots(roots)
            .with_secret_redactor(secret_redactor),
    )
    .await
}

async fn activate_lazy_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
    roots: Vec<std::path::PathBuf>,
    secret_redactor: SecretRedactor,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<()> {
    register_mcp_tools_with_options(
        registry,
        servers,
        McpToolRegistrationOptions::lazy()?
            .with_capabilities(capabilities)
            .with_roots(roots)
            .with_secret_redactor(secret_redactor)
            .with_elicitation_handler(elicitation_handler),
    )
    .await
}

async fn register_mcp_tools_with_name_limit(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    provider_tool_name_max_chars: usize,
) -> Result<()> {
    let mut options = McpToolRegistrationOptions::eager()?;
    options.provider_tool_name_max_chars = provider_tool_name_max_chars;
    register_mcp_tools_with_options(registry, servers, options).await
}

async fn register_mcp_tools_with_name_limit_and_roots(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    provider_tool_name_max_chars: usize,
    roots: Vec<std::path::PathBuf>,
    secret_redactor: SecretRedactor,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<()> {
    let mut options = McpToolRegistrationOptions::eager()?
        .with_roots(roots)
        .with_secret_redactor(secret_redactor)
        .with_elicitation_handler(elicitation_handler);
    options.provider_tool_name_max_chars = provider_tool_name_max_chars;
    register_mcp_tools_with_options(registry, servers, options).await
}

fn write_fake_server_script(path: &std::path::Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    Ok(())
}

#[tokio::test]
async fn registration_rejects_duplicate_exact_server_names_before_launch() -> Result<()> {
    let duplicate = mcp_server_config! {
        name: "duplicate".to_owned(),
        command: "/must/not/be/launched".to_owned(),
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();

    let error = register_mcp_tools(&mut registry, &[duplicate.clone(), duplicate])
        .await
        .expect_err("duplicate exact MCP server names must fail before process launch");

    assert!(error.to_string().contains("duplicate MCP server name"));
    assert!(registry.specs().is_empty());
    Ok(())
}

fn write_identity_server_script(
    path: &std::path::Path,
    server_name: &str,
    server_version: &str,
) -> Result<()> {
    let body = format!(
        r#"#!/usr/bin/env python3
import json, sys

SERVER_NAME = {server_name:?}
SERVER_VERSION = {server_version:?}

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"protocolVersion":"2024-11-05","serverInfo":{{"name":SERVER_NAME,"version":SERVER_VERSION}},"capabilities":{{}}}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"echo","description":"Echo","inputSchema":{{"type":"object","properties":{{"value":{{"type":"string"}}}},"required":["value"]}}}}]}}}})
    elif method == "tools/call":
        value = message["params"]["arguments"]["value"]
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"content":[{{"type":"text","text":value}}]}}}})
"#
    );
    write_fake_server_script(path, &body)
}

fn test_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        reasoning_stream: ReasoningStreamSupport::Unsupported,
        supports_reasoning_effort: false,
        supports_tool_stream: false,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: false,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    }
}

#[tokio::test]
async fn local_mcp_process_launcher_marks_stdio_outside_sandbox() -> Result<()> {
    if Command::new("sh")
        .arg("-c")
        .arg("true")
        .output()
        .await
        .is_err()
    {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let launch = LocalMcpProcessLauncher.launch(McpProcessLaunchRequest {
        server_name: "local".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), "sleep 1".to_owned()],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:test-launch".to_owned(),
        startup_timeout_secs: 1,
        classification: McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::default(),
        declaration: None,
    })?;

    assert_eq!(
        launch.receipt.classification,
        McpProcessClass::LocalStdioConfigured
    );
    assert_eq!(
        launch.receipt.coverage,
        McpProcessCoverage::LocalStdioOutsideSandbox
    );
    assert_eq!(
        launch.receipt.backend,
        Some(sigil_kernel::ExecutionBackendKind::Local)
    );
    assert_eq!(
        launch.receipt.sandbox_profile,
        Some(sigil_kernel::ExecutionSandboxProfile::Unconfined)
    );

    let mut child = launch.child;
    let _ = child.kill().await;
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn mcp_process_network_ask_without_approval_is_zero_spawn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("ask-without-approval-spawned");
    let result = LocalMcpProcessLauncher.launch(McpProcessLaunchRequest {
        server_name: "ask-without-approval".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "printf spawned > \"$1\"".to_owned(),
            "sh".to_owned(),
            marker.to_string_lossy().into_owned(),
        ],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:ask-without-approval".to_owned(),
        startup_timeout_secs: 1,
        classification: McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::new(
            sigil_kernel::NetworkPolicy::Ask,
            false,
        ),
        declaration: None,
    });

    let Err(error) = result else {
        panic!("ask without explicit approval must fail before spawn");
    };
    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkApprovalRequired)
    );
    assert!(!marker.exists(), "network ask rejection must be zero-spawn");
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn mcp_process_network_ask_with_explicit_approval_spawns() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("ask-approved-spawned");
    let mut launch = LocalMcpProcessLauncher.launch(McpProcessLaunchRequest {
        server_name: "ask-approved".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "printf spawned > \"$1\"".to_owned(),
            "sh".to_owned(),
            marker.to_string_lossy().into_owned(),
        ],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:ask-approved".to_owned(),
        startup_timeout_secs: 1,
        classification: McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::new(
            sigil_kernel::NetworkPolicy::Ask,
            true,
        ),
        declaration: None,
    })?;

    assert!(launch.child.wait().await?.success());
    assert!(
        marker.exists(),
        "explicit network approval should admit spawn"
    );
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn mcp_process_network_deny_without_proven_isolation_is_zero_spawn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("deny-unproven-spawned");
    let result = LocalMcpProcessLauncher.launch(McpProcessLaunchRequest {
        server_name: "deny-unproven".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "printf spawned > \"$1\"".to_owned(),
            "sh".to_owned(),
            marker.to_string_lossy().into_owned(),
        ],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:deny-unproven".to_owned(),
        startup_timeout_secs: 1,
        classification: McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::new(
            sigil_kernel::NetworkPolicy::Deny,
            true,
        ),
        declaration: None,
    });

    let Err(error) = result else {
        panic!("deny without isolation proof must fail before spawn");
    };
    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkIsolationUnavailable)
    );
    assert!(
        !marker.exists(),
        "network deny rejection must be zero-spawn"
    );
    Ok(())
}

#[tokio::test]
async fn extension_process_environment_clears_ambient_and_injects_only_grants() -> Result<()> {
    let Some(home) = std::env::var("HOME").ok() else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let run = |grant_names: &[String]| -> Result<_> {
        LocalMcpProcessLauncher.launch(McpProcessLaunchRequest {
            server_name: "environment-test".to_owned(),
            command: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf '%s|%s' \"${HOME-unset}\" \"${PATH-unset}\"".to_owned(),
            ],
            working_dir: Some(temp.path().to_path_buf()),
            environment: sigil_kernel::resolve_extension_process_environment(grant_names)?,
            launch_static_fingerprint: "sha256:test-launch".to_owned(),
            startup_timeout_secs: 1,
            classification: McpProcessClass::LocalStdioConfigured,
            network_admission: ExtensionProcessNetworkAdmission::default(),
            declaration: None,
        })
    };

    let mut isolated = run(&[])?;
    let mut isolated_stdout = isolated
        .child
        .stdout
        .take()
        .expect("isolated stdout should be piped");
    let mut isolated_output = String::new();
    isolated_stdout.read_to_string(&mut isolated_output).await?;
    isolated.child.wait().await?;
    assert!(isolated_output.starts_with("unset|"));
    assert!(!isolated_output.ends_with("|unset"));

    let mut granted = run(&["HOME".to_owned()])?;
    let metadata = granted.receipt.audit_metadata();
    let metadata_text = format!("{metadata:?}");
    assert_eq!(granted.receipt.environment_grant_names, vec!["HOME"]);
    assert_eq!(
        granted.receipt.environment_policy,
        sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension
    );
    assert!(!metadata_text.contains(&home));
    assert_eq!(metadata["mcp_environment_grant_names"], "HOME");
    let mut granted_stdout = granted
        .child
        .stdout
        .take()
        .expect("granted stdout should be piped");
    let mut granted_output = String::new();
    granted_stdout.read_to_string(&mut granted_output).await?;
    granted.child.wait().await?;
    assert!(granted_output.starts_with(&format!("{home}|")));
    Ok(())
}

#[test]
fn mcp_environment_grant_is_orthogonal_to_payload_secret_policy() -> Result<()> {
    if std::env::var("HOME").is_err() {
        return Ok(());
    }
    for allow_secrets in [false, true] {
        let config = mcp_server_config! {
            name: "orthogonal".to_owned(),
            command: "sh".to_owned(),
            inherit_env: vec!["HOME".to_owned()],
            trust: McpServerTrustPolicy {
                allow_secrets,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        };
        let request = McpProcessLaunchRequest::from_config(&config, None)?;
        assert_eq!(request.environment.grant_names(), &["HOME"]);
    }
    Ok(())
}

#[test]
fn missing_mcp_environment_grant_is_typed_pre_spawn_configuration_error() {
    let config = mcp_server_config! {
        name: "missing-env".to_owned(),
        command: "definitely-must-not-spawn".to_owned(),
        inherit_env: vec!["SIGIL_E21_ENV_THAT_MUST_NOT_EXIST_7F33".to_owned()],
        ..McpServerConfig::default()
    };
    let error = McpProcessLaunchRequest::from_config(&config, None)
        .expect_err("missing environment grant should fail before launch");
    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::ConfigurationInvalid)
    );
}

#[tokio::test]
async fn pre_spawn_failure_records_lifecycle_without_launch_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let marker = temp.path().join("pre-spawn-marker");
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let error = register_mcp_tools_with_options(
        &mut ToolRegistry::new(),
        &[mcp_server_config! {
            name: "missing-env-audit".to_owned(),
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), format!("touch {}", marker.display())],
            inherit_env: vec!["SIGIL_E21_ENV_THAT_MUST_NOT_EXIST_41C2".to_owned()],
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store.clone())),
    )
    .await
    .expect_err("missing grant must fail before spawn");
    assert!(
        error
            .to_string()
            .contains("missing inherited environment variables")
    );
    assert!(!marker.exists());

    let lifecycle = JsonlSessionStore::read_event_records(session_store.path())?
        .into_iter()
        .find_map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ExtensionProcessLifecycleRecorded.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .expect("pre-spawn failure should be durably distinguished");
    let payload: ExtensionProcessLifecycleAudit = serde_json::from_value(lifecycle.payload)?;
    assert_eq!(payload.phase, ExtensionProcessLaunchPhase::PreSpawn);
    assert_eq!(
        payload.status,
        ExtensionProcessLifecycleStatus::StartupFailed
    );
    assert_eq!(payload.safe_metadata["mcp_process_coverage"], "unsupported");
    assert!(
        !payload
            .safe_metadata
            .contains_key("mcp_environment_live_fingerprint")
    );
    Ok(())
}

#[test]
fn changed_live_environment_fingerprint_invalidates_process_binding() -> Result<()> {
    let environment = sigil_kernel::resolve_extension_process_environment(&[])?;
    let request = McpProcessLaunchRequest {
        server_name: "binding".to_owned(),
        command: "sh".to_owned(),
        args: Vec::new(),
        working_dir: None,
        environment: environment.clone(),
        launch_static_fingerprint: "sha256:binding".to_owned(),
        startup_timeout_secs: 1,
        classification: McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::default(),
        declaration: None,
    };
    let mut receipt = super::McpProcessLaunchReceipt::local_outside_sandbox(&request);
    assert!(super::client::environment_binding_matches(
        &receipt,
        &environment
    ));
    receipt.environment_live_fingerprint = "hmac-sha256:changed".to_owned();
    assert!(!super::client::environment_binding_matches(
        &receipt,
        &environment
    ));
    Ok(())
}

#[tokio::test]
async fn credentialed_mcp_redacts_grant_across_identity_descriptors_results_and_args_gate()
-> Result<()> {
    let Ok(home) = std::env::var("HOME") else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("secret-argument-reached-server");
    let script = temp.path().join("credential_echo_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys

SECRET = os.environ["HOME"]
MARKER = pathlib.Path(sys.argv[1])

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":SECRET,"version":SECRET},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"leak","description":SECRET,"inputSchema":{"type":"object","properties":{SECRET:{"type":"string"},"value":{"type":"string","description":SECRET}}}}]}})
    elif method == "tools/call":
        value = message["params"]["arguments"].get("value", "")
        if value == SECRET:
            MARKER.write_text("leaked", encoding="utf-8")
        if value == "error":
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":SECRET}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":SECRET}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "credential-echo".to_owned(),
            command: "python3".to_owned(),
            args: vec![
                script.to_string_lossy().into_owned(),
                marker.to_string_lossy().into_owned(),
            ],
            inherit_env: vec!["HOME".to_owned()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                approval_default: ApprovalMode::Allow,
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let spec = registry
        .spec_for("mcp__credential_echo__leak")
        .expect("credentialed MCP tool should register");
    assert_eq!(spec.description, "[redacted]");
    assert!(!serde_json::to_string(&spec.input_schema)?.contains(&home));

    let egress = registry
        .egress_audit(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "identity".to_owned(),
                name: "mcp__credential_echo__leak".to_owned(),
                args_json: r#"{"value":"ordinary"}"#.to_owned(),
            },
        )?
        .expect("egress audit should exist");
    let egress_text = serde_json::to_string(&egress.payload)?;
    assert!(!egress_text.contains(&home));
    assert!(egress_text.contains("[redacted]"));

    let secret_key_args = Value::Object(
        [(home.clone(), Value::String("ordinary".to_owned()))]
            .into_iter()
            .collect(),
    );
    let secret_key_egress = registry
        .egress_audit(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "identity-secret-key".to_owned(),
                name: "mcp__credential_echo__leak".to_owned(),
                args_json: serde_json::to_string(&secret_key_args)?,
            },
        )?
        .expect("secret-key egress audit should exist");
    let secret_key_egress_text = serde_json::to_string(&secret_key_egress.payload)?;
    assert!(!secret_key_egress_text.contains(&home));
    assert!(secret_key_egress_text.contains("[redacted]"));

    let blocked = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "blocked-secret".to_owned(),
                name: "mcp__credential_echo__leak".to_owned(),
                args_json: serde_json::to_string(&json!({"value": home}))?,
            },
        )
        .await?;
    assert!(matches!(
        blocked.status,
        ToolResultStatus::Error(ref error) if error.kind == ToolErrorKind::PermissionDenied
    ));
    assert!(!marker.exists(), "blocked secret must not reach MCP bytes");

    let echoed = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "echoed-secret".to_owned(),
                name: "mcp__credential_echo__leak".to_owned(),
                args_json: r#"{"value":"echo"}"#.to_owned(),
            },
        )
        .await?;
    assert_eq!(echoed.content, "[redacted]");
    assert!(!format!("{echoed:?}").contains(&home));

    let protocol_error = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "protocol-secret".to_owned(),
                name: "mcp__credential_echo__leak".to_owned(),
                args_json: r#"{"value":"error"}"#.to_owned(),
            },
        )
        .await?;
    assert!(!format!("{protocol_error:?}").contains(&home));
    assert!(format!("{protocol_error:?}").contains("[redacted]"));
    Ok(())
}

#[tokio::test]
async fn credentialed_mcp_redacts_grant_from_registration_protocol_error() -> Result<()> {
    let Ok(home) = std::env::var("HOME") else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("credential_error_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, os, sys

SECRET = os.environ["HOME"]

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"version-fixture","version":"1.0.0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":SECRET}})
"#,
    )?;
    let error = register_mcp_tools(
        &mut ToolRegistry::new(),
        &[mcp_server_config! {
            name: "credential-error".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().into_owned()],
            inherit_env: vec!["HOME".to_owned()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("tools/list error should fail required registration");
    let error_text = format!("{error:#}");
    assert!(!error_text.contains(&home));
    assert!(error_text.contains("[redacted]"));
    Ok(())
}

#[tokio::test]
async fn stale_environment_binding_rejects_inbound_notification_before_handler() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("binding_mcp_server.py");
    write_identity_server_script(&script, "binding", "1.0.0")?;
    let handler = Arc::new(RecordingMcpRuntimeEventHandler::default());
    let runtime_handler: Arc<dyn McpRuntimeEventHandler> = handler.clone();
    let mut client = super::client::McpClient::spawn(
        mcp_server_config! {
            name: "binding".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().into_owned()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        },
        vec![temp.path().to_path_buf()],
        Some(temp.path().to_path_buf()),
        SecretRedactor::empty(),
        super::unsupported_mcp_elicitation_handler(),
        runtime_handler,
        Arc::new(LocalMcpProcessLauncher),
        None,
        ExtensionProcessNetworkAdmission::default(),
    )
    .await?;
    let monitor = client
        ._stderr_monitor_task
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .expect("spawn should start the stderr fault monitor");
    monitor.abort();
    let _ = monitor.await;
    Arc::get_mut(&mut client)
        .expect("aborted monitor should release its weak client reference")
        ._process_receipt
        .environment_live_fingerprint = "hmac-sha256:stale".to_owned();
    let mut state = client.connection.lock().await;
    let super::client::McpConnectionState::Ready(connection) = &mut *state else {
        panic!("newly spawned client should be ready");
    };
    let error = client
        .handle_inbound_message(
            connection,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {"progressToken": "stale", "message": "must-not-surface"}
            }),
        )
        .await
        .expect_err("stale binding must reject inbound messages");
    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::EnvironmentBindingChanged)
    );
    assert!(handler.progress.lock().expect("progress lock").is_empty());
    Ok(())
}

#[tokio::test]
async fn registers_and_calls_fake_stdio_tool() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        value = message["params"]["arguments"]["value"]
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":value}]}})
"#,
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                trust_class: McpTrustClass::ThirdParty,
                approval_default: ApprovalMode::Allow,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;
    let spec = registry
        .spec_for("mcp__fake__echo")
        .expect("expected provider-visible MCP tool");
    assert_eq!(spec.category, ToolCategory::Mcp);
    assert_eq!(spec.access, ToolAccess::Read);
    assert_eq!(
        spec.network_effect,
        Some(sigil_kernel::NetworkEffect::Unknown)
    );
    assert_eq!(
        registry.permission_operation(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "call-operation".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )?,
        sigil_kernel::ToolOperation::NetworkRequest
    );
    assert!(registry.spec_for("echo").is_none());
    assert!(registry.spec_for("mcp__fake__resources_list").is_none());
    assert!(registry.spec_for("mcp__fake__resources_read").is_none());

    let subjects = registry.permission_subjects(
        &ToolContext::new(temp.path().to_path_buf(), 5),
        &sigil_kernel::ToolCall {
            id: "call-subject".to_owned(),
            name: "mcp__fake__echo".to_owned(),
            args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
        },
    )?;
    assert_eq!(subjects.len(), 2);
    assert_eq!(subjects[0].kind, ToolSubjectKind::McpTool);
    assert_eq!(subjects[0].normalized, "mcp__fake__echo");
    assert_eq!(subjects[0].scope, ToolSubjectScope::Unknown);
    assert_eq!(subjects[1].kind, ToolSubjectKind::McpTrustClass);
    assert!(subjects[1].original.starts_with("fake:third_party:sha256:"));
    assert_eq!(subjects[1].normalized, "mcp_trust_class:third_party");
    assert_eq!(subjects[1].scope, ToolSubjectScope::Unknown);

    let default_mode = registry.permission_default_mode(
        &ToolContext::new(temp.path().to_path_buf(), 5),
        &sigil_kernel::ToolCall {
            id: "call-default".to_owned(),
            name: "mcp__fake__echo".to_owned(),
            args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
        },
    )?;
    assert_eq!(default_mode, Some(ApprovalMode::Allow));

    let egress = registry
        .egress_audit(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "call-egress".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )?
        .expect("mcp trust egress logging should produce an audit summary");
    assert_eq!(egress.destination, "mcp:fake");
    assert_eq!(egress.operation, "tools/call");
    assert!(!egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""server":"fake""#));
    assert!(payload.contains(r#""remote_tool":"echo""#));
    assert!(payload.contains(r#""top_level_keys":["value"]"#));
    assert!(!payload.contains("hello from mcp"));

    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "quiet".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                egress_logging: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;
    let quiet_egress = registry.egress_audit(
        &ToolContext::new(temp.path().to_path_buf(), 5),
        &sigil_kernel::ToolCall {
            id: "call-quiet-egress".to_owned(),
            name: "mcp__quiet__echo".to_owned(),
            args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
        },
    )?;
    assert!(quiet_egress.is_none());

    let result = registry
        .execute(
            sigil_kernel::ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )
        .await?;
    assert_eq!(result.content, "hello from mcp");
    Ok(())
}

#[tokio::test]
async fn registration_with_mutation_recorder_does_not_dirty_clean_startup() -> Result<()> {
    let Ok(home) = std::env::var("HOME") else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let script = temp.path().join("lifecycle_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_options(
        &mut registry,
        &[mcp_server_config! {
            name: "lifecycle".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            inherit_env: vec!["HOME".to_owned()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                trust_class: McpTrustClass::ThirdParty,
                approval_default: ApprovalMode::Allow,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store.clone())),
    )
    .await?;

    assert!(registry.spec_for("mcp__lifecycle__echo").is_some());
    let events = JsonlSessionStore::read_event_records(session_store.path())?
        .into_iter()
        .map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event) => event,
        })
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .all(|event| event.event_type != DurableEventType::WorkspaceMutationDetected.as_str()),
        "clean MCP startup must not make verification inconclusive"
    );
    let lifecycle = events
        .iter()
        .find(|event| {
            event.event_type == DurableEventType::ExtensionProcessLifecycleRecorded.as_str()
        })
        .expect("clean startup should record neutral durable lifecycle evidence");
    let payload: ExtensionProcessLifecycleAudit =
        serde_json::from_value(lifecycle.payload.clone())?;
    assert_eq!(payload.process_kind, "mcp_stdio");
    assert_eq!(payload.subject, "lifecycle");
    assert_eq!(payload.phase, ExtensionProcessLaunchPhase::PostSpawn);
    assert_eq!(payload.status, ExtensionProcessLifecycleStatus::Registered);
    assert_eq!(payload.safe_metadata["mcp_environment_grant_names"], "HOME");
    assert_eq!(
        payload.safe_metadata["mcp_process_declared_network_effect"],
        "unknown"
    );
    assert_eq!(
        payload.safe_metadata["mcp_process_effective_network_effect"],
        "unknown"
    );
    assert_eq!(
        payload.safe_metadata["mcp_process_network_isolation_proven"],
        "false"
    );
    assert_eq!(payload.safe_metadata["mcp_process_network_policy"], "allow");
    assert_eq!(
        payload.safe_metadata["mcp_process_explicit_network_approval"],
        "false"
    );
    assert!(!payload.safe_metadata["mcp_environment_live_fingerprint"].is_empty());
    assert!(!serde_json::to_string(&payload)?.contains(&home));
    Ok(())
}

#[tokio::test]
async fn strict_zero_surface_registration_records_failure_not_registered() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let script = temp.path().join("zero_surface_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

while True:
    line = sys.stdin.buffer.readline()
    if not line:
        break
    message = json.loads(line.decode())
    method = message.get("method")
    if method == "initialize":
        response = {"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}}
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        response = {"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}}
    else:
        continue
    sys.stdout.buffer.write(json.dumps(response).encode() + b"\n")
    sys.stdout.buffer.flush()
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_options(
        &mut registry,
        &[mcp_server_config! {
            name: "zero-surface".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().into_owned()],
            startup_timeout_secs: 5,
            required: false,
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_strict_registration()
            .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store.clone())),
    )
    .await
    .expect_err("strict registration must reject a server with no callable surfaces");

    assert!(registry.specs().is_empty());
    let lifecycle = JsonlSessionStore::read_event_records(session_store.path())?
        .into_iter()
        .map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event) => event,
        })
        .filter(|event| {
            event.event_type == DurableEventType::ExtensionProcessLifecycleRecorded.as_str()
        })
        .map(|event| serde_json::from_value::<ExtensionProcessLifecycleAudit>(event.payload))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(lifecycle.len(), 1);
    assert_eq!(lifecycle[0].subject, "zero-surface");
    assert_eq!(lifecycle[0].phase, ExtensionProcessLaunchPhase::PostSpawn);
    assert_eq!(
        lifecycle[0].status,
        ExtensionProcessLifecycleStatus::StartupFailed
    );
    assert_eq!(
        lifecycle[0].safe_metadata["mcp_startup_result"],
        "zero_surface"
    );
    Ok(())
}

#[tokio::test]
async fn mcp_lifecycle_mutation_failure_adds_server_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store_path = temp.path().join("session.jsonl");
    let session_store = JsonlSessionStore::new(&store_path)?;
    fs::create_dir(&store_path)?;
    let options = McpToolRegistrationOptions::eager()?
        .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store));

    let error = super::capture_mcp_server_lifecycle_scan(&options, "filesystem")
        .expect_err("directory-backed session path should fail mutation evidence append");

    assert!(
        format!("{error:#}")
            .contains("failed to record MCP server filesystem lifecycle mutation evidence")
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn post_spawn_lifecycle_append_failure_reaps_mcp_process_group() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store_path = temp.path().join("post-spawn-session.jsonl");
    let descendant_pid_path = temp.path().join("post-spawn-descendant.pid");
    let script = temp.path().join("post_spawn_lifecycle_failure.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, pathlib, subprocess, sys
STORE_PATH = pathlib.Path(sys.argv[1])
PID_PATH = pathlib.Path(sys.argv[2])
child = subprocess.Popen(["sh", "-c", "trap '' TERM; while :; do sleep 1; done"])
PID_PATH.write_text(str(child.pid))
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(message):
    sys.stdout.buffer.write(json.dumps(message).encode() + b"\n")
    sys.stdout.buffer.flush()
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        STORE_PATH.mkdir()
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;
    let session_store = JsonlSessionStore::new(&store_path)?;
    let options = McpToolRegistrationOptions::eager()?
        .with_working_dir(temp.path().to_path_buf())
        .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store.clone()));
    let error = register_mcp_tools_with_options(
        &mut ToolRegistry::new(),
        &[mcp_server_config! {
            name: "post-spawn-audit-failure".to_owned(),
            command: "python3".to_owned(),
            args: vec![
                script.to_string_lossy().into_owned(),
                store_path.to_string_lossy().into_owned(),
                descendant_pid_path.to_string_lossy().into_owned(),
            ],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        options,
    )
    .await
    .expect_err("post-spawn lifecycle append failure must fail registration");
    let error_text = format!("{error:#}");
    assert!(error_text.contains("lifecycle evidence failed after spawn"));
    assert!(error_text.contains("cleanup_completed=true"));

    let descendant_pid = fs::read_to_string(descendant_pid_path)?
        .trim()
        .parse::<u32>()?;
    assert!(
        !crate::process_group::process_has_live_effect(descendant_pid)?,
        "post-spawn lifecycle append failure must not orphan descendants"
    );
    Ok(())
}

#[tokio::test]
async fn mcp_initialize_uses_crate_version_for_client_info() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("version_mcp_server.py");
    let params_path = temp.path().join("initialize_params.json");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

PARAMS_PATH = sys.argv[1]

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        with open(PARAMS_PATH, "w", encoding="utf-8") as handle:
            json.dump(message["params"], handle, sort_keys=True)
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"probe","description":"Probe","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "version".to_owned(),
            command: "python3".to_owned(),
            args: vec![
                script.to_string_lossy().to_string(),
                params_path.to_string_lossy().to_string(),
            ],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let params: Value = serde_json::from_str(&fs::read_to_string(params_path)?)?;
    assert_eq!(params["clientInfo"]["name"], "sigil");
    assert_eq!(params["clientInfo"]["version"], env!("CARGO_PKG_VERSION"));
    Ok(())
}

#[tokio::test]
async fn mcp_startup_failure_without_workspace_change_does_not_dirty() -> Result<()> {
    let Ok(home) = std::env::var("HOME") else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let script = temp.path().join("crashing_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import sys

sys.exit(7)
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_options(
        &mut registry,
        &[mcp_server_config! {
            name: "crashy".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            inherit_env: vec!["HOME".to_owned()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                trust_class: McpTrustClass::ThirdParty,
                approval_default: ApprovalMode::Allow,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store.clone())),
    )
    .await
    .expect_err("startup failure should still surface");

    assert!(registry.specs().is_empty());
    let events = JsonlSessionStore::read_event_records(session_store.path())?
        .into_iter()
        .map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event) => event,
        })
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .all(|event| event.event_type != DurableEventType::WorkspaceMutationDetected.as_str()),
        "failed MCP startup without workspace writes must not stale verification"
    );
    let lifecycle = events
        .iter()
        .find(|event| {
            event.event_type == DurableEventType::ExtensionProcessLifecycleRecorded.as_str()
        })
        .expect("post-spawn failure should retain a neutral durable launch receipt");
    let payload: ExtensionProcessLifecycleAudit =
        serde_json::from_value(lifecycle.payload.clone())?;
    assert_eq!(payload.phase, ExtensionProcessLaunchPhase::PostSpawn);
    assert_eq!(
        payload.status,
        ExtensionProcessLifecycleStatus::StartupFailed
    );
    assert_eq!(payload.safe_metadata["mcp_environment_grant_names"], "HOME");
    assert!(!payload.safe_metadata["mcp_launch_static_fingerprint"].is_empty());
    assert!(!serde_json::to_string(&payload)?.contains(&home));
    Ok(())
}

#[tokio::test]
async fn mcp_startup_failure_with_side_effect_records_snapshot_changed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let side_effect = workspace.join("mcp-started.txt");
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let script = temp.path().join("crashing_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import pathlib, sys

pathlib.Path(sys.argv[1]).write_text("started", encoding="utf-8")
sys.exit(7)
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_options(
        &mut registry,
        &[mcp_server_config! {
            name: "crashy".to_owned(),
            command: "python3".to_owned(),
            args: vec![
                script.to_string_lossy().to_string(),
                side_effect.to_string_lossy().to_string(),
            ],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                trust_class: McpTrustClass::ThirdParty,
                approval_default: ApprovalMode::Allow,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_mutation_recorder(workspace, MutationEventRecorder::new(session_store.clone())),
    )
    .await
    .expect_err("startup failure should still surface");

    assert!(registry.specs().is_empty());
    assert!(side_effect.exists());
    let detected = JsonlSessionStore::read_event_records(session_store.path())?
        .into_iter()
        .filter_map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(detected.len(), 1);
    let payload: WorkspaceMutationDetected = serde_json::from_value(detected[0].payload.clone())?;
    assert_eq!(payload.tool_call_id, None);
    assert_eq!(payload.tool_name, "mcp_server:crashy");
    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::SnapshotChanged
    );
    assert!(!payload.unknown_dirty);
    assert_eq!(
        payload
            .metadata
            .get("mcp_startup_result")
            .map(String::as_str),
        Some("startup_failed")
    );
    assert_eq!(
        payload
            .metadata
            .get("mcp_process_coverage")
            .map(String::as_str),
        Some("local_stdio_outside_sandbox")
    );
    assert_eq!(
        payload
            .metadata
            .get("mcp_environment_policy")
            .map(String::as_str),
        Some("isolated_extension")
    );
    assert_eq!(
        payload
            .metadata
            .get("mcp_process_network")
            .map(String::as_str),
        Some("unknown")
    );
    Ok(())
}

#[tokio::test]
async fn registration_runs_mcp_process_from_configured_working_dir() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    let script = temp.path().join("cwd_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, os, sys

expected_cwd = os.path.realpath(sys.argv[1])
if os.path.realpath(os.getcwd()) != expected_cwd:
    sys.stderr.write(f"cwd mismatch: {os.getcwd()} != {expected_cwd}\n")
    sys.stderr.flush()
    sys.exit(2)

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"cwd","description":"cwd","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_options(
        &mut registry,
        &[mcp_server_config! {
            name: "cwd".to_owned(),
            command: "python3".to_owned(),
            args: vec![
                script.to_string_lossy().to_string(),
                workspace.to_string_lossy().to_string(),
            ],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_roots(vec![workspace.clone()])
            .with_working_dir(workspace),
    )
    .await?;

    assert!(registry.spec_for("mcp__cwd__cwd").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_resources_register_and_execute_when_server_declares_capability() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("resource_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{"resources":{"subscribe":False,"listChanged":True}},"serverInfo":{"name":"resource-test","version":"1.0.0"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == "resources/list":
        cursor = (message.get("params") or {}).get("cursor")
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"resources":[{"uri":"file:///workspace/notes.md","name":"notes","description":"Project notes","mimeType":"text/markdown"}],"nextCursor":"page-2","cursorSeen":cursor}})
    elif method == "resources/read":
        uri = message["params"]["uri"]
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"contents":[{"uri":uri,"mimeType":"text/markdown","text":"hello resource"}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "docs".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                trust_class: McpTrustClass::SelfHosted,
                approval_default: ApprovalMode::Allow,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let list_spec = registry
        .spec_for("mcp__docs__resources_list")
        .expect("expected MCP resources/list tool");
    assert_eq!(list_spec.category, ToolCategory::Mcp);
    assert_eq!(list_spec.access, ToolAccess::Read);
    assert_eq!(
        list_spec.network_effect,
        Some(sigil_kernel::NetworkEffect::Read)
    );
    let read_spec = registry
        .spec_for("mcp__docs__resources_read")
        .expect("expected MCP resources/read tool");
    assert_eq!(read_spec.category, ToolCategory::Mcp);
    assert_eq!(read_spec.access, ToolAccess::Read);
    assert_eq!(
        read_spec.network_effect,
        Some(sigil_kernel::NetworkEffect::Read)
    );

    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &sigil_kernel::ToolCall {
                id: "call-resource-operation".to_owned(),
                name: "mcp__docs__resources_read".to_owned(),
                args_json: r#"{"uri":"file:///workspace/notes.md"}"#.to_owned(),
            },
        )?,
        sigil_kernel::ToolOperation::NetworkRequest
    );
    let subjects = registry.permission_subjects(
        &ctx,
        &sigil_kernel::ToolCall {
            id: "call-resource-subjects".to_owned(),
            name: "mcp__docs__resources_read".to_owned(),
            args_json: r#"{"uri":"file:///workspace/notes.md"}"#.to_owned(),
        },
    )?;
    assert_eq!(subjects.len(), 2);
    assert_eq!(subjects[0].kind, ToolSubjectKind::McpTool);
    assert_eq!(subjects[0].normalized, "mcp__docs__resources_read");
    assert_eq!(subjects[1].kind, ToolSubjectKind::McpTrustClass);
    assert!(subjects[1].original.starts_with("docs:self_hosted:sha256:"));
    assert_eq!(subjects[1].normalized, "mcp_trust_class:self_hosted");

    let default_mode = registry.permission_default_mode(
        &ctx,
        &sigil_kernel::ToolCall {
            id: "call-resource-default".to_owned(),
            name: "mcp__docs__resources_read".to_owned(),
            args_json: r#"{"uri":"file:///workspace/notes.md"}"#.to_owned(),
        },
    )?;
    assert_eq!(default_mode, Some(ApprovalMode::Allow));

    let egress = registry
        .egress_audit(
            &ctx,
            &sigil_kernel::ToolCall {
                id: "call-resource-egress".to_owned(),
                name: "mcp__docs__resources_list".to_owned(),
                args_json: r#"{"cursor":"page-1"}"#.to_owned(),
            },
        )?
        .expect("resource tools should produce MCP egress audit summaries");
    assert_eq!(egress.destination, "mcp:docs");
    assert_eq!(egress.operation, "resources/list");
    assert!(!egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""resource_operation":"resources_list""#));
    assert!(payload.contains(r#""top_level_keys":["cursor"]"#));
    assert!(!payload.contains("page-1"));

    let list_result = registry
        .execute(
            ctx.clone(),
            sigil_kernel::ToolCall {
                id: "call-list".to_owned(),
                name: "mcp__docs__resources_list".to_owned(),
                args_json: r#"{"cursor":"page-1"}"#.to_owned(),
            },
        )
        .await?;
    assert!(matches!(list_result.status, ToolResultStatus::Ok));
    assert!(list_result.content.contains("file:///workspace/notes.md"));
    assert!(list_result.content.contains(r#""cursorSeen": "page-1""#));

    let read_result = registry
        .execute(
            ctx.clone(),
            sigil_kernel::ToolCall {
                id: "call-read".to_owned(),
                name: "mcp__docs__resources_read".to_owned(),
                args_json: r#"{"uri":"file:///workspace/notes.md"}"#.to_owned(),
            },
        )
        .await?;
    assert!(matches!(read_result.status, ToolResultStatus::Ok));
    assert!(read_result.content.contains("hello resource"));

    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "quiet-docs".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                egress_logging: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;
    let quiet_egress = registry.egress_audit(
        &ctx,
        &sigil_kernel::ToolCall {
            id: "call-quiet-resource-egress".to_owned(),
            name: "mcp__quiet_docs__resources_read".to_owned(),
            args_json: r#"{"uri":"file:///workspace/notes.md"}"#.to_owned(),
        },
    )?;
    assert!(quiet_egress.is_none());
    Ok(())
}

#[tokio::test]
async fn mcp_resource_tools_validate_arguments_and_missing_results() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("invalid_resource_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"resources":{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == "resources/list":
        write_message({"jsonrpc":"2.0","id":message["id"]})
    elif method == "resources/read":
        write_message({"jsonrpc":"2.0","id":message["id"]})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "invalid-docs".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    for (tool_name, args_json, expected_message) in [
        (
            "mcp__invalid_docs__resources_list",
            r#""not-object""#,
            "MCP resources/list arguments must be an object",
        ),
        (
            "mcp__invalid_docs__resources_list",
            r#"{"cursor":1}"#,
            "MCP resources/list cursor must be a string",
        ),
        (
            "mcp__invalid_docs__resources_read",
            r#""not-object""#,
            "MCP resources/read arguments must be an object",
        ),
        (
            "mcp__invalid_docs__resources_read",
            r#"{}"#,
            "MCP resources/read requires a uri string",
        ),
        (
            "mcp__invalid_docs__resources_read",
            r#"{"uri":"  "}"#,
            "MCP resources/read uri must not be empty",
        ),
    ] {
        let result = registry
            .execute(
                ctx.clone(),
                sigil_kernel::ToolCall {
                    id: format!("call-{tool_name}"),
                    name: tool_name.to_owned(),
                    args_json: args_json.to_owned(),
                },
            )
            .await?;
        match result.status {
            ToolResultStatus::Error(error) => {
                assert_eq!(error.kind, ToolErrorKind::InvalidInput);
                assert_eq!(error.message, expected_message);
            }
            ToolResultStatus::Ok => panic!("invalid resource arguments should fail"),
        }
    }

    let missing_result = registry
        .execute(
            ctx,
            sigil_kernel::ToolCall {
                id: "call-missing-resource-result".to_owned(),
                name: "mcp__invalid_docs__resources_read".to_owned(),
                args_json: r#"{"uri":"file:///workspace/notes.md"}"#.to_owned(),
            },
        )
        .await?;
    let ToolResultStatus::Error(error) = missing_result.status else {
        panic!("missing resource result must be a structured protocol error");
    };
    assert_eq!(error.kind, ToolErrorKind::Protocol);
    assert_eq!(error.details["mcp"]["code"], "invalid_jsonrpc_envelope");
    Ok(())
}

#[tokio::test]
async fn mcp_resource_read_blocks_secret_uri_when_trust_disallows_secrets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("secret_resource_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"resources":{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == "resources/read":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"contents":[{"uri":message["params"]["uri"],"text":"should not run"}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[mcp_server_config! {
            name: "docs".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
    )
    .await?;

    let egress = registry
        .egress_audit(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "call-secret-resource-egress".to_owned(),
                name: "mcp__docs__resources_read".to_owned(),
                args_json: r#"{"uri":"sigil://secret/sk-secret"}"#.to_owned(),
            },
        )?
        .expect("resource egress audit should summarize blocked attempts");
    assert_eq!(egress.operation, "resources/read");
    assert!(egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""secret_detected":true"#));
    assert!(!payload.contains("sk-secret"));

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-secret-resource".to_owned(),
                name: "mcp__docs__resources_read".to_owned(),
                args_json: r#"{"uri":"sigil://secret/sk-secret"}"#.to_owned(),
            },
        )
        .await?;
    match result.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::PermissionDenied);
        }
        ToolResultStatus::Ok => panic!("secret resource URI egress should be blocked"),
    }
    assert!(!result.content.contains("sk-secret"));
    Ok(())
}

#[tokio::test]
async fn registers_and_calls_mcp_prompt_surface_tools() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("prompt_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"prompt-server","version":"1.0.0"},"capabilities":{"prompts":{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == "prompts/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"prompts":[{"name":"story_seed","description":"Seed prompt","arguments":[{"name":"genre","required":False}]}]}})
    elif method == "prompts/get":
        genre = message["params"].get("arguments", {}).get("genre", "mystery")
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"description":"Seed prompt","messages":[{"role":"user","content":{"type":"text","text":"Write a " + genre + " opening."}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "prompts".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let list_spec = registry
        .spec_for("mcp__prompts__prompts_list")
        .expect("prompts/list tool should register");
    assert_eq!(list_spec.access, ToolAccess::Read);
    assert_eq!(
        list_spec.network_effect,
        Some(sigil_kernel::NetworkEffect::Read)
    );
    assert!(registry.spec_for("mcp__prompts__prompts_get").is_some());

    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &sigil_kernel::ToolCall {
                id: "call-prompt-operation".to_owned(),
                name: "mcp__prompts__prompts_get".to_owned(),
                args_json: r#"{"name":"story_seed"}"#.to_owned(),
            },
        )?,
        sigil_kernel::ToolOperation::NetworkRequest
    );
    let list = registry
        .execute(
            ctx.clone(),
            sigil_kernel::ToolCall {
                id: "call-prompts-list".to_owned(),
                name: "mcp__prompts__prompts_list".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;
    assert!(list.content.contains("story_seed"));
    assert_eq!(list.metadata.details["mcp"]["kind"], "prompt");
    assert_eq!(list.metadata.details["mcp"]["operation"], "prompts/list");

    let subjects = registry.permission_subjects(
        &ctx,
        &sigil_kernel::ToolCall {
            id: "call-prompt-subjects".to_owned(),
            name: "mcp__prompts__prompts_get".to_owned(),
            args_json: r#"{"name":"story_seed"}"#.to_owned(),
        },
    )?;
    assert_eq!(subjects.len(), 2);
    assert_eq!(subjects[0].kind, ToolSubjectKind::McpTool);
    assert_eq!(subjects[0].normalized, "mcp__prompts__prompts_get");
    assert_eq!(subjects[1].kind, ToolSubjectKind::McpTrustClass);
    assert!(
        subjects[1]
            .original
            .starts_with("prompts:self_hosted:sha256:")
    );

    let default_mode = registry.permission_default_mode(
        &ctx,
        &sigil_kernel::ToolCall {
            id: "call-prompt-default".to_owned(),
            name: "mcp__prompts__prompts_get".to_owned(),
            args_json: r#"{"name":"story_seed"}"#.to_owned(),
        },
    )?;
    assert_eq!(default_mode, Some(ApprovalMode::Ask));

    let egress = registry
        .egress_audit(
            &ctx,
            &sigil_kernel::ToolCall {
                id: "call-prompt-egress".to_owned(),
                name: "mcp__prompts__prompts_get".to_owned(),
                args_json: r#"{"name":"story_seed","arguments":{"genre":"fantasy"}}"#.to_owned(),
            },
        )?
        .expect("prompt egress audit should summarize prompt arguments");
    assert_eq!(egress.destination, "mcp:prompts");
    assert_eq!(egress.operation, "prompts/get");
    assert!(!egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""prompt_operation":"prompts_get""#));
    assert!(payload.contains(r#""top_level_keys":["arguments","name"]"#));
    assert!(!payload.contains("fantasy"));

    let prompt = registry
        .execute(
            ctx,
            sigil_kernel::ToolCall {
                id: "call-prompts-get".to_owned(),
                name: "mcp__prompts__prompts_get".to_owned(),
                args_json: r#"{"name":"story_seed","arguments":{"genre":"fantasy"}}"#.to_owned(),
            },
        )
        .await?;
    assert!(prompt.content.contains("fantasy opening"));

    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "quiet-prompts".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                egress_logging: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;
    let quiet_egress = registry.egress_audit(
        &ToolContext::new(temp.path().to_path_buf(), 5),
        &sigil_kernel::ToolCall {
            id: "call-quiet-prompt-egress".to_owned(),
            name: "mcp__quiet_prompts__prompts_list".to_owned(),
            args_json: "{}".to_owned(),
        },
    )?;
    assert!(quiet_egress.is_none());
    Ok(())
}

#[test]
fn mcp_prompt_tool_kind_validates_edge_arguments() {
    assert_eq!(McpPromptToolKind::all().len(), 2);
    assert_eq!(McpPromptToolKind::List.provider_suffix(), "prompts_list");
    assert_eq!(McpPromptToolKind::Get.provider_suffix(), "prompts_get");
    assert!(
        McpPromptToolKind::List
            .description()
            .contains("List MCP prompts")
    );
    assert_eq!(McpPromptToolKind::List.method(), "prompts/list");
    assert_eq!(McpPromptToolKind::Get.method(), "prompts/get");
    assert_eq!(
        McpPromptToolKind::List.request_params(&json!({"cursor": "page-2"})),
        Ok(json!({"cursor": "page-2"}))
    );
    assert_eq!(
        McpPromptToolKind::Get
            .request_params(&json!({"name": "story", "arguments": {"genre": "fantasy"}})),
        Ok(json!({"name": "story", "arguments": {"genre": "fantasy"}}))
    );
    assert_eq!(
        McpPromptToolKind::List
            .request_params(&json!("not-object"))
            .expect_err("list args must be object"),
        "MCP prompts/list arguments must be an object"
    );
    assert_eq!(
        McpPromptToolKind::List
            .request_params(&json!({"cursor": 1}))
            .expect_err("cursor must be string"),
        "MCP prompts/list cursor must be a string"
    );
    assert_eq!(
        McpPromptToolKind::Get
            .request_params(&json!("not-object"))
            .expect_err("get args must be object"),
        "MCP prompts/get arguments must be an object"
    );
    assert_eq!(
        McpPromptToolKind::Get
            .request_params(&json!({}))
            .expect_err("name is required"),
        "MCP prompts/get requires a name string"
    );
    assert_eq!(
        McpPromptToolKind::Get
            .request_params(&json!({"name": "  "}))
            .expect_err("name must not be blank"),
        "MCP prompts/get name must not be empty"
    );
    assert_eq!(
        McpPromptToolKind::Get
            .request_params(&json!({"name": "story", "arguments": 1}))
            .expect_err("arguments must be object"),
        "MCP prompts/get arguments must be an object"
    );
    assert_eq!(
        McpPromptToolKind::Get.input_schema()["required"],
        json!(["name"])
    );
}

#[tokio::test]
async fn mcp_prompt_tools_validate_arguments_and_block_secret_values() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("prompt_secret_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"prompts":{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == "prompts/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"prompts":[]}})
    elif method == "prompts/get":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"messages":[]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[mcp_server_config! {
            name: "prompt-secret".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
    )
    .await?;

    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let invalid = registry
        .execute(
            ctx.clone(),
            sigil_kernel::ToolCall {
                id: "call-invalid-prompt".to_owned(),
                name: "mcp__prompt_secret__prompts_get".to_owned(),
                args_json: r#"{"name":"story","arguments":1}"#.to_owned(),
            },
        )
        .await?;
    match invalid.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::InvalidInput);
            assert_eq!(error.message, "MCP prompts/get arguments must be an object");
        }
        ToolResultStatus::Ok => panic!("invalid prompt arguments should fail"),
    }

    let blocked = registry
        .execute(
            ctx,
            sigil_kernel::ToolCall {
                id: "call-secret-prompt".to_owned(),
                name: "mcp__prompt_secret__prompts_get".to_owned(),
                args_json: r#"{"name":"story","arguments":{"token":"sk-secret"}}"#.to_owned(),
            },
        )
        .await?;
    match blocked.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::PermissionDenied);
        }
        ToolResultStatus::Ok => panic!("secret prompt arguments should be blocked"),
    }
    assert!(!blocked.content.contains("sk-secret"));
    Ok(())
}

#[tokio::test]
async fn mcp_prompt_tools_surface_protocol_errors_and_missing_results() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("prompt_error_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"prompts":{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == "prompts/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"list failed"}})
    elif method == "prompts/get":
        name = message["params"].get("name")
        if name == "missing":
            write_message({"jsonrpc":"2.0","id":message["id"]})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32001,"message":"bad prompt"}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "prompt-errors".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let list_error = registry
        .execute(
            ctx.clone(),
            sigil_kernel::ToolCall {
                id: "call-prompt-list-error".to_owned(),
                name: "mcp__prompt_errors__prompts_list".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;
    match list_error.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::Protocol);
            assert!(error.message.contains("MCP prompts/list failed"));
        }
        ToolResultStatus::Ok => panic!("prompt list protocol error should fail"),
    }

    let get_error = registry
        .execute(
            ctx.clone(),
            sigil_kernel::ToolCall {
                id: "call-prompt-get-error".to_owned(),
                name: "mcp__prompt_errors__prompts_get".to_owned(),
                args_json: r#"{"name":"bad"}"#.to_owned(),
            },
        )
        .await?;
    match get_error.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::Protocol);
            assert!(error.message.contains("MCP prompts/get failed"));
        }
        ToolResultStatus::Ok => panic!("prompt get protocol error should fail"),
    }

    let missing_result = registry
        .execute(
            ctx,
            sigil_kernel::ToolCall {
                id: "call-prompt-missing-result".to_owned(),
                name: "mcp__prompt_errors__prompts_get".to_owned(),
                args_json: r#"{"name":"missing"}"#.to_owned(),
            },
        )
        .await?;
    let ToolResultStatus::Error(error) = missing_result.status else {
        panic!("missing prompt result must be a structured protocol error");
    };
    assert_eq!(error.kind, ToolErrorKind::Protocol);
    assert_eq!(error.details["mcp"]["code"], "invalid_jsonrpc_envelope");
    Ok(())
}

#[test]
fn mcp_text_budget_truncates_by_lines_and_utf8_boundaries() {
    let by_lines = super::truncate_text_budget("one\ntwo\nthree\n", 100, 2);
    assert!(by_lines.truncated);
    assert_eq!(by_lines.total_lines, 3);
    assert_eq!(by_lines.returned_lines, 2);
    assert_eq!(by_lines.content, "one\ntwo\n");
    assert_eq!(
        by_lines.returned_bytes + by_lines.omitted_bytes,
        by_lines.total_bytes
    );
    assert!(!by_lines.content.contains("MCP output truncated"));

    let by_bytes = super::truncate_text_budget("abcdef", 4, 10);
    assert_eq!(by_bytes.content, "abcd");
    assert_eq!(by_bytes.returned_bytes, 4);
    assert_eq!(by_bytes.omitted_bytes, 2);
    assert_eq!(by_bytes.returned_bytes + by_bytes.omitted_bytes, 6);

    let mut utf8 = String::new();
    super::append_utf8_prefix(&mut utf8, "éx", 1);
    assert!(utf8.is_empty());
    super::append_utf8_prefix(&mut utf8, "éx", 2);
    assert_eq!(utf8, "é");
    assert_eq!(super::to_u64(7), 7);
}

#[test]
fn bounded_mcp_json_streams_large_deep_values_and_preserves_small_pretty_output() -> Result<()> {
    let small = super::bounded_mcp_json(&SecretRedactor::empty(), &json!({"answer": 42}))?;
    assert_eq!(small.content, "{\n  \"answer\": 42\n}");

    let mut deep = json!({"payload": "x".repeat(128 * 1024)});
    for _ in 0..64 {
        deep = json!([deep]);
    }
    let compact_bytes = serde_json::to_vec(&deep)?.len();
    let bounded = super::bounded_mcp_json(&SecretRedactor::empty(), &deep)?;

    assert!(bounded.truncated);
    assert_eq!(bounded.total_bytes, compact_bytes);
    assert_eq!(
        bounded.returned_bytes + bounded.omitted_bytes,
        bounded.total_bytes
    );
    assert!(bounded.content.len() <= super::MCP_OUTPUT_LIMIT_BYTES);
    assert!(!bounded.content.contains("MCP output truncated"));
    Ok(())
}

#[test]
fn bounded_mcp_json_redacts_secrets_before_json_escaping() -> Result<()> {
    let secret = "line-one\n\"quoted-secret\"";
    let redactor = SecretRedactor::from_values([secret]);
    let escaped = serde_json::to_string(secret)?;

    let bounded = super::bounded_mcp_json(&redactor, &json!({"value": secret}))?;

    assert!(!bounded.content.contains(secret));
    assert!(!bounded.content.contains(&escaped[1..escaped.len() - 1]));
    assert!(bounded.content.contains("[redacted]"));
    Ok(())
}

#[test]
fn bounded_mcp_output_never_adds_a_secret_bearing_truncation_marker() {
    let mut redactor = SecretRedactor::empty();
    redactor.add_secret_carrier(SecretString::new("MCP"));

    let bounded = super::bounded_mcp_text(&redactor, &"x".repeat(64 * 1024));
    assert!(bounded.content.len() <= super::MCP_OUTPUT_LIMIT_BYTES);
    assert!(!bounded.content.contains("MCP"));

    let protocol = super::bounded_mcp_protocol_error(
        &redactor,
        &json!({"code": -32000, "message": "ordinary remote failure"}),
        "MCP tools/call failed",
    );
    assert!(!protocol.summary.contains("MCP"));
}

#[test]
fn mcp_output_metadata_bounds_remote_names_and_identity_arrays() {
    let redactor = SecretRedactor::empty();
    let long = "x".repeat(1024 * 1024);
    let identity = super::McpServerObservedIdentity {
        transport_fingerprint: long.clone(),
        process_authorization_fingerprint: long.clone(),
        declaration: None,
        environment_grant_names: (0..1_000)
            .map(|index| format!("{}-{index}", "G".repeat(256)))
            .collect(),
        environment_static_fingerprint: long.clone(),
        environment_live_fingerprint: long.clone(),
        protocol_version: long.clone(),
        server_name: long.clone(),
        server_version: long.clone(),
    };
    let tool_name = super::McpToolName {
        provider_name: "mcp__bounded__tool".to_owned(),
        server_name: long.clone(),
        original_name: long,
    };
    let budget = super::bounded_mcp_text(&redactor, "ok");
    let (_, metadata) = super::bounded_mcp_tool_result(
        &redactor,
        &tool_name,
        &McpServerTrustPolicy::default(),
        &identity,
        "tool",
        "tools/call",
        budget,
    );
    let encoded = serde_json::to_vec(&metadata.details).expect("metadata should serialize");

    assert!(encoded.len() < 32 * 1024);
    assert_eq!(metadata.details["mcp"]["server"], "");
    assert_eq!(metadata.details["mcp"]["tool"], "");
    assert_eq!(
        metadata.details["mcp"]["server_identity"]["environment_grant_name_count"],
        1_000
    );
    assert_eq!(
        metadata.details["mcp"]["server_identity"]["environment_grant_names_omitted"],
        1_000
    );
}

#[tokio::test]
async fn mcp_tool_output_is_bounded_and_reports_truncation_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("large_output_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"large","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"x" * 80000}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "large".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-large-output".to_owned(),
                name: "mcp__large__large".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;

    assert!(result.metadata.truncated);
    assert_eq!(
        result.metadata.limit_bytes,
        Some(super::MCP_OUTPUT_LIMIT_BYTES as u64)
    );
    assert_eq!(result.metadata.details["mcp"]["tool"], "large");
    assert_eq!(result.metadata.details["mcp"]["operation"], "tools/call");
    assert!(result.content.len() <= super::MCP_OUTPUT_LIMIT_BYTES);
    Ok(())
}

#[tokio::test]
async fn mcp_runtime_event_handler_receives_progress_and_list_changed_notifications() -> Result<()>
{
    let Ok(home) = std::env::var("HOME") else {
        return Ok(());
    };
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("event_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, os, sys

SECRET = os.environ["HOME"]

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"prompts":{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":SECRET,"progress":1,"total":2,"message":SECRET}})
        write_message({"jsonrpc":"2.0","method":"notifications/prompts/list_changed","params":{}})
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
"#,
    )?;

    let handler = Arc::new(RecordingMcpRuntimeEventHandler::default());
    let runtime_handler: Arc<dyn McpRuntimeEventHandler> = handler.clone();
    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_options(
        &mut registry,
        &[mcp_server_config! {
            name: "events".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            inherit_env: vec!["HOME".to_owned()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        McpToolRegistrationOptions::eager()?
            .with_roots(vec![temp.path().to_path_buf()])
            .with_runtime_event_handler(runtime_handler),
    )
    .await?;

    let progress = handler.progress.lock().expect("progress lock").clone();
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].server_name, "events");
    assert_eq!(progress[0].progress_token, "[redacted]");
    assert_eq!(progress[0].message.as_deref(), Some("[redacted]"));
    assert!(!format!("{progress:?}").contains(&home));

    let list_changed = handler.list_changed.lock().expect("list lock").clone();
    assert_eq!(list_changed.len(), 1);
    assert_eq!(list_changed[0].kind, super::McpListChangedKind::Prompts);
    Ok(())
}

#[tokio::test]
async fn unsupported_mcp_runtime_event_handler_accepts_events_and_kind_labels() -> Result<()> {
    assert_eq!(McpListChangedKind::Tools.as_str(), "tools");
    assert_eq!(McpListChangedKind::Resources.as_str(), "resources");
    assert_eq!(McpListChangedKind::Prompts.as_str(), "prompts");
    let handler = unsupported_mcp_runtime_event_handler();

    handler
        .progress(McpProgressNotification {
            server_name: "events".to_owned(),
            progress_token: "1".to_owned(),
            progress: None,
            total: None,
            message: None,
        })
        .await?;
    handler
        .list_changed(McpListChangedNotification {
            server_name: "events".to_owned(),
            kind: McpListChangedKind::Tools,
        })
        .await?;
    Ok(())
}

#[test]
fn mcp_runtime_notification_helpers_parse_edge_payloads() {
    let numeric = super::mcp_progress_notification(
        "server",
        &json!({
            "params": {
                "progressToken": 7,
                "progress": 2,
                "total": 4,
                "message": "Half"
            }
        }),
    )
    .expect("numeric token should parse");
    assert_eq!(numeric.progress_token, "7");
    assert_eq!(numeric.progress, Some(2.0));
    assert_eq!(numeric.total, Some(4.0));
    assert_eq!(numeric.message.as_deref(), Some("Half"));

    let structured = super::mcp_progress_notification(
        "server",
        &json!({"params": {"progressToken": {"id": "scan"}}}),
    )
    .expect("structured token should serialize");
    assert_eq!(structured.progress_token, r#"{"id":"scan"}"#);
    assert!(super::mcp_progress_notification("server", &json!({})).is_none());
    assert_eq!(
        super::mcp_list_changed_kind("notifications/tools/list_changed"),
        Some(McpListChangedKind::Tools)
    );
    assert_eq!(McpListChangedKind::Tools.as_str(), "tools");
    assert_eq!(McpListChangedKind::Resources.as_str(), "resources");
    assert_eq!(McpListChangedKind::Prompts.as_str(), "prompts");
    assert_eq!(
        super::mcp_list_changed_kind("notifications/resources/list_changed"),
        Some(McpListChangedKind::Resources)
    );
    assert_eq!(
        super::mcp_list_changed_kind("notifications/prompts/list_changed"),
        Some(McpListChangedKind::Prompts)
    );
    assert!(super::mcp_list_changed_kind("notifications/other").is_none());
}

#[test]
fn remote_transport_fingerprint_binds_source_metadata_without_resolving_secrets() -> Result<()> {
    let mut config = McpServerConfig {
        name: "remote".to_owned(),
        transport: sigil_kernel::McpServerTransportConfig::StreamableHttp(
            sigil_kernel::McpStreamableHttpConfig {
                url: "https://mcp.example.test/mcp".to_owned(),
                http_headers: BTreeMap::from([("X-Client".to_owned(), "sigil-alpha".to_owned())]),
                env_http_headers: BTreeMap::from([(
                    "X-Api-Key".to_owned(),
                    "SIGIL_TEST_MISSING_REMOTE_MCP_SECRET".to_owned(),
                )]),
                bearer_token_env_var: None,
                oauth: None,
                client_capabilities: BTreeSet::from([
                    sigil_kernel::McpRemoteClientCapability::Roots,
                ]),
            },
        ),
        ..McpServerConfig::default()
    };
    let first = super::mcp_transport_static_fingerprint(&config)?;
    assert!(first.starts_with("sha256:"));
    assert_eq!(first.len(), 71);

    let remote = match &mut config.transport {
        sigil_kernel::McpServerTransportConfig::StreamableHttp(remote) => remote,
        sigil_kernel::McpServerTransportConfig::Stdio { .. } => unreachable!(),
    };
    remote.env_http_headers.insert(
        "X-Api-Key".to_owned(),
        "SIGIL_TEST_MISSING_REMOTE_MCP_SECRET_ROTATED".to_owned(),
    );
    let changed_source = super::mcp_transport_static_fingerprint(&config)?;
    assert_ne!(changed_source, first);
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_server_registers_when_identity_matches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("pinned_mcp_server.py");
    write_identity_server_script(&script, "sigil-test-server", "1.2.3")?;
    let args = vec![script.to_string_lossy().to_string()];
    let transport_fingerprint = super::mcp_transport_fingerprint("python3", &args)?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "pinned".to_owned(),
            command: "python3".to_owned(),
            args,
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    transport_fingerprint,
                    protocol_version: "2024-11-05".to_owned(),
                    server_name: "sigil-test-server".to_owned(),
                    server_version: "1.2.3".to_owned(),
                }),
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.spec_for("mcp__pinned__echo").is_some());
    let egress = registry
        .egress_audit(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "call-pin-egress".to_owned(),
                name: "mcp__pinned__echo".to_owned(),
                args_json: r#"{"value":"hello"}"#.to_owned(),
            },
        )?
        .expect("egress audit should include pinned server identity");
    assert_eq!(
        egress.payload["server_identity"]["server_name"],
        "sigil-test-server"
    );
    assert_eq!(egress.payload["server_identity"]["server_version"], "1.2.3");
    Ok(())
}

#[test]
fn mcp_launch_static_fingerprint_preserves_empty_grant_compatibility_and_binds_names() -> Result<()>
{
    let args = vec!["server.py".to_owned()];
    let legacy = super::mcp_transport_fingerprint("python3", &args)?;
    let empty = sigil_mcp_launch_fingerprint(mcp_server_config! {
        command: "python3".to_owned(),
        args: args.clone(),
        ..McpServerConfig::default()
    })?;
    let granted = sigil_mcp_launch_fingerprint(mcp_server_config! {
        command: "python3".to_owned(),
        args,
        inherit_env: vec!["PATH".to_owned()],
        ..McpServerConfig::default()
    })?;

    assert_eq!(empty, legacy);
    assert_ne!(granted, legacy);
    Ok(())
}

#[test]
fn declaration_stable_pin_excludes_paths_but_binds_executable_content() -> Result<()> {
    let first_root = tempfile::tempdir()?;
    let second_root = tempfile::tempdir()?;
    let first_executable = first_root.path().join("bin/server");
    let second_executable = second_root.path().join("bin/server");
    fs::create_dir_all(
        first_executable
            .parent()
            .expect("first parent should exist"),
    )?;
    fs::create_dir_all(
        second_executable
            .parent()
            .expect("second parent should exist"),
    )?;
    fs::write(&first_executable, "same executable bytes")?;
    fs::write(&second_executable, "same executable bytes")?;
    let projection_fingerprint = format!("sha256:{:064x}", 7);

    let first = super::mcp_resolved_launch_static_fingerprint_at(
        &projection_fingerprint,
        &first_executable,
    )?;
    let second = super::mcp_resolved_launch_static_fingerprint_at(
        &projection_fingerprint,
        &second_executable,
    )?;

    assert_eq!(
        first, second,
        "persisted declaration pins must not disclose canonical root identity"
    );
    fs::write(&second_executable, "different executable bytes")?;
    let changed = super::mcp_resolved_launch_static_fingerprint_at(
        &projection_fingerprint,
        &second_executable,
    )?;
    assert_ne!(
        first, changed,
        "executable content remains statically pinned"
    );
    Ok(())
}

#[test]
fn process_launch_request_debug_hides_command_cwd_environment_and_args() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let command_secret = temp.path().join("private-command-secret");
    let argument_secret = "private-argument-secret";
    let request = McpProcessLaunchRequest::from_config(
        &mcp_server_config! {
            name: "debug-safe".to_owned(),
            command: command_secret.to_string_lossy().into_owned(),
            args: vec![argument_secret.to_owned()],
            ..McpServerConfig::default()
        },
        Some(temp.path().to_path_buf()),
    )?;

    let debug = format!("{request:?}");
    let static_fingerprint = super::mcp_transport_fingerprint(
        &command_secret.to_string_lossy(),
        &[argument_secret.to_owned()],
    )?;
    assert!(!debug.contains(&command_secret.to_string_lossy().into_owned()));
    assert!(!debug.contains(&temp.path().to_string_lossy().into_owned()));
    assert!(!debug.contains(argument_secret));
    assert!(!debug.contains(&static_fingerprint));
    assert!(debug.contains("command: \"[hidden]\""));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn credentialed_static_pin_rejects_replaced_executable_before_spawn() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var("HOME").is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("credentialed-mcp");
    let marker = temp.path().join("replacement-spawned");
    fs::write(&executable, "#!/bin/sh\nexit 0\n")?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions)?;

    let mut config = mcp_server_config! {
        name: "executable-pin".to_owned(),
        command: executable.to_string_lossy().into_owned(),
        inherit_env: vec!["HOME".to_owned()],
        startup_timeout_secs: 2,
        ..McpServerConfig::default()
    };
    let fingerprint = super::mcp_launch_static_fingerprint_at(&config, temp.path())?;
    config.trust = McpServerTrustPolicy {
        pin_version: true,
        pinned: Some(sigil_kernel::McpServerPinnedIdentity {
            transport_fingerprint: fingerprint,
            protocol_version: "2025-06-18".to_owned(),
            server_name: "expected".to_owned(),
            server_version: "1".to_owned(),
        }),
        ..McpServerTrustPolicy::default()
    };

    fs::write(
        &executable,
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )?;
    let error = register_mcp_tools_with_options(
        &mut ToolRegistry::new(),
        &[config],
        McpToolRegistrationOptions::eager()?.with_working_dir(temp.path().to_path_buf()),
    )
    .await
    .expect_err("same-path executable replacement must stale the pre-spawn pin");
    assert!(
        error
            .to_string()
            .contains("pre-spawn transport_fingerprint mismatch")
    );
    assert!(
        !marker.exists(),
        "replacement must receive zero process spawn"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn credentialed_launch_resolves_relative_executable_against_working_dir() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var("HOME").is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("server");
    fs::write(&executable, "#!/bin/sh\nexit 0\n")?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions)?;
    let config = mcp_server_config! {
        name: "relative".to_owned(),
        command: "./server".to_owned(),
        inherit_env: vec!["HOME".to_owned()],
        ..McpServerConfig::default()
    };
    let expected = super::mcp_launch_static_fingerprint_at(&config, temp.path())?;
    let request = McpProcessLaunchRequest::from_config(&config, Some(temp.path().to_path_buf()))?;

    assert_eq!(
        request.command,
        executable.canonicalize()?.to_string_lossy()
    );
    assert_eq!(request.working_dir, Some(temp.path().canonicalize()?));
    assert_eq!(request.launch_static_fingerprint, expected);
    Ok(())
}

#[test]
fn interpreter_launch_pin_binds_argument_text_but_not_script_contents() -> Result<()> {
    if std::env::var("HOME").is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("server.py");
    fs::write(&script, "print('first')\n")?;
    let config = mcp_server_config! {
        name: "interpreter-boundary".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().into_owned()],
        inherit_env: vec!["HOME".to_owned()],
        ..McpServerConfig::default()
    };
    let first = super::mcp_launch_static_fingerprint_at(&config, temp.path())?;
    fs::write(&script, "print('second')\n")?;
    let changed_script = super::mcp_launch_static_fingerprint_at(&config, temp.path())?;
    let changed_args = super::mcp_launch_static_fingerprint_at(
        &mcp_server_config! {
            args: vec!["different.py".to_owned()],
            ..config
        },
        temp.path(),
    )?;

    assert_eq!(
        first, changed_script,
        "the executable pin does not interpret or attest script argument contents"
    );
    assert_ne!(
        first, changed_args,
        "argument text remains statically bound"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn changed_approved_process_binding_rejects_before_spawn() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var("HOME").is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("must-not-spawn-after-binding-change");
    let executable = temp.path().join("binding-server");
    fs::write(
        &executable,
        format!("#!/bin/sh\ntouch {}\n", marker.display()),
    )?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions)?;
    let config = mcp_server_config! {
        name: "binding-change".to_owned(),
        command: executable.to_string_lossy().into_owned(),
        inherit_env: vec!["HOME".to_owned()],
        ..McpServerConfig::default()
    };
    let launch_fingerprint = super::mcp_launch_static_fingerprint_at(&config, temp.path())?;
    let stale_subject = ToolSubject::mcp_trust_class_with_process_binding(
        &config.name,
        config.trust.trust_class.as_str(),
        launch_fingerprint,
        "hmac-sha256:stale-approved-binding",
    );

    let error = register_mcp_tools_with_options(
        &mut ToolRegistry::new(),
        &[config],
        McpToolRegistrationOptions::eager()?
            .with_working_dir(temp.path().to_path_buf())
            .with_expected_process_subject(stale_subject),
    )
    .await
    .expect_err("changed approval binding must fail before launch");

    assert!(
        error
            .to_string()
            .contains("process binding changed after approval")
    );
    assert!(!marker.exists(), "stale approval must produce zero spawn");
    Ok(())
}

fn sigil_mcp_launch_fingerprint(config: McpServerConfig) -> Result<String> {
    super::mcp_launch_static_fingerprint(&config)
}

#[tokio::test]
async fn mismatched_static_pin_rejects_before_process_spawn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("must-not-exist");
    let config = mcp_server_config! {
        name: "pre-spawn-pin".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), format!("touch {}", marker.display())],
        trust: McpServerTrustPolicy {
            pin_version: true,
            pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                transport_fingerprint: "sha256:stale".to_owned(),
                protocol_version: "2024-11-05".to_owned(),
                server_name: "stale".to_owned(),
                server_version: "0".to_owned(),
            }),
            ..McpServerTrustPolicy::default()
        },
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(&mut registry, &[config])
        .await
        .expect_err("stale static pin should fail before spawn");

    assert!(
        error
            .to_string()
            .contains("pre-spawn transport_fingerprint mismatch")
    );
    assert!(!marker.exists());
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_server_errors_when_pin_is_missing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("unpinned_mcp_server.py");
    write_identity_server_script(&script, "sigil-test-server", "1.2.3")?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "unpinned".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                pin_version: true,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("pin_version without pinned identity should fail");

    let message = error.to_string();
    assert!(message.contains("pin_version = true but no pinned identity"));
    assert!(message.contains("transport_fingerprint"));
    assert!(message.contains("MCP server unpinned"));
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_server_errors_when_identity_mismatches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("mismatched_mcp_server.py");
    write_identity_server_script(&script, "sigil-test-server", "1.2.3")?;
    let args = vec![script.to_string_lossy().to_string()];
    let transport_fingerprint = super::mcp_transport_fingerprint("python3", &args)?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "mismatched".to_owned(),
            command: "python3".to_owned(),
            args,
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    transport_fingerprint,
                    protocol_version: "2024-11-05".to_owned(),
                    server_name: "other-server".to_owned(),
                    server_version: "1.2.3".to_owned(),
                }),
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("mismatched pinned identity should fail");

    let message = error.to_string();
    assert!(message.contains("pinned identity mismatch"));
    assert!(message.contains("server_name expected other-server observed sigil-test-server"));
    Ok(())
}

#[tokio::test]
async fn mcp_tool_blocks_secret_args_when_trust_disallows_secrets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"should not run"}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[mcp_server_config! {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-secret".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"sk-secret"}"#.to_owned(),
            },
        )
        .await?;

    let egress = registry
        .egress_audit(
            &ToolContext::new(temp.path().to_path_buf(), 5),
            &sigil_kernel::ToolCall {
                id: "call-secret-egress".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"sk-secret"}"#.to_owned(),
            },
        )?
        .expect("mcp trust egress logging should summarize blocked attempts");
    assert!(egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""secret_detected":true"#));
    assert!(payload.contains(r#""top_level_keys":["value"]"#));
    assert!(!payload.contains("sk-secret"));

    match result.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::PermissionDenied);
        }
        ToolResultStatus::Ok => panic!("secret egress should be blocked"),
    }
    assert!(!result.content.contains("sk-secret"));
    Ok(())
}

#[test]
fn mcp_egress_json_summary_does_not_include_values() {
    let summary = super::summarize_egress_json(&serde_json::json!({
        "path": "src/main.rs",
        "api_key": "sk-secret",
        "count": 3
    }));
    let rendered = serde_json::to_string(&summary).expect("summary should serialize");

    assert!(rendered.contains(r#""top_level_keys":["api_key","count","path"]"#));
    assert!(rendered.contains(r#""api_key":"string""#));
    assert!(!rendered.contains("sk-secret"));
    assert!(!rendered.contains("src/main.rs"));
}

#[test]
fn mcp_egress_json_summary_bounds_field_names_and_counts_without_copying_values() {
    let mut object = serde_json::Map::new();
    for index in 0..1_000 {
        object.insert(format!("field_{index:04}"), json!("v".repeat(128)));
    }
    object.insert("L".repeat(1024 * 1024), json!("must-not-be-copied"));

    let summary = super::summarize_egress_json(&Value::Object(object));
    let encoded = serde_json::to_vec(&summary).expect("summary should serialize");

    assert_eq!(summary["top_level_key_count"], 1_001);
    assert_eq!(summary["top_level_keys"].as_array().map_or(0, Vec::len), 64);
    assert_eq!(summary["omitted_top_level_keys"], 937);
    assert_eq!(summary["truncated"], true);
    assert!(encoded.len() < 32 * 1024);
    assert!(!String::from_utf8_lossy(&encoded).contains("must-not-be-copied"));
}

#[tokio::test]
async fn mcp_tool_redacts_secret_echo_when_trust_allows_secrets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        value = message["params"]["arguments"]["value"]
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":value}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[mcp_server_config! {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: true,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-secret".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"sk-secret"}"#.to_owned(),
            },
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.content, sigil_kernel::REDACTED_SECRET);
    Ok(())
}

#[tokio::test]
async fn register_mcp_tools_errors_when_initialize_times_out() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("slow_init_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys, time

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

message = read_message()
time.sleep(2)
if message and message.get("method") == "initialize":
    sys.exit(0)
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "slow".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 1,
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("expected MCP initialize timeout");

    assert!(
        error
            .to_string()
            .contains("MCP operation initialize timed out")
    );
    Ok(())
}

#[tokio::test]
async fn required_lazy_mcp_server_is_deferred_until_activation() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "required-lazy".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            startup: McpServerStartup::Lazy,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());

    let error = activate_lazy_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "required-lazy".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            startup: McpServerStartup::Lazy,
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("required lazy activation should surface startup failure");

    assert!(
        error
            .to_string()
            .contains("failed to spawn MCP server required-lazy")
    );
    Ok(())
}

#[tokio::test]
async fn lazy_mcp_servers_are_not_started_or_registered() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "lazy".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            startup: McpServerStartup::Lazy,
            required: false,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn lazy_mcp_server_activation_registers_and_calls_real_tools() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("lazy_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        value = message["params"]["arguments"]["value"]
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"lazy:" + value}]}})
"#,
    )?;

    let server = mcp_server_config! {
        name: "lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().to_string()],
        startup: McpServerStartup::Lazy,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, std::slice::from_ref(&server)).await?;
    assert!(
        registry.spec_for("mcp__lazy__echo").is_none(),
        "lazy registration should not expose pseudo tools"
    );

    activate_lazy_mcp_tools(&mut registry, &[server]).await?;
    assert!(registry.spec_for("mcp__lazy__echo").is_some());

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-lazy".to_owned(),
                name: "mcp__lazy__echo".to_owned(),
                args_json: r#"{"value":"ok"}"#.to_owned(),
            },
        )
        .await?;

    assert_eq!(result.content, "lazy:ok");
    Ok(())
}

#[tokio::test]
async fn optional_eager_mcp_server_start_failure_is_skipped() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "optional".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            required: false,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn optional_eager_mcp_server_start_failure_does_not_block_other_servers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("healthy_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[
            mcp_server_config! {
                name: "optional".to_owned(),
                command: "/definitely/missing/sigil-mcp-server".to_owned(),
                required: false,
                ..McpServerConfig::default()
            },
            mcp_server_config! {
                name: "healthy".to_owned(),
                command: "python3".to_owned(),
                args: vec![script.to_string_lossy().to_string()],
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            },
        ],
    )
    .await?;

    assert!(registry.spec_for("mcp__healthy__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_server_stderr_is_drained_without_blocking_registration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("stderr_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

sys.stderr.write("Secure MCP Filesystem Server running on stdio\n")
sys.stderr.write("x" * (256 * 1024))
sys.stderr.flush()

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let server = mcp_server_config! {
        name: "stderr".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().to_string()],
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };

    tokio::time::timeout(
        Duration::from_secs(5),
        register_mcp_tools(&mut registry, &[server]),
    )
    .await??;

    assert!(registry.spec_for("mcp__stderr__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn register_mcp_tools_errors_when_tools_list_payload_is_invalid() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("invalid_tools_list_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "invalid-tools-list".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("expected tools/list payload validation error");

    assert!(
        error
            .to_string()
            .contains("MCP tools/list missing tools array")
    );
    Ok(())
}

#[tokio::test]
async fn mcp_tool_execute_surfaces_remote_call_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("erroring_call_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"remote boom"}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "error-call".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__error_call__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )
        .await?;

    let ToolResultStatus::Error(error) = result.status else {
        panic!("expected remote tools/call error result");
    };
    assert_eq!(error.kind, ToolErrorKind::Protocol);
    assert!(error.message.contains("remote boom"));
    Ok(())
}

#[tokio::test]
async fn mcp_tool_execute_falls_back_for_non_text_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("non_text_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"image","description":"Image","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"image","data":"abc","mimeType":"image/png"}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "non-text".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__non_text__image".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;

    assert!(result.content.contains(r#""type": "image""#));
    assert!(result.content.contains(r#""mimeType": "image/png""#));
    Ok(())
}

#[tokio::test]
async fn mcp_client_answers_roots_list_while_waiting_for_tools_list() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("sigil mcp root");
    fs::create_dir_all(&workspace_root)?;
    let script = temp.path().join("roots_request_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        capabilities = message.get("params", {}).get("capabilities", {})
        if "roots" not in capabilities:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32602,"message":"missing roots capability"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"p1","progress":1}})
        write_message({"jsonrpc":"2.0","id":"server-roots-1","method":"roots/list","params":{}})
        roots_response = read_message()
        roots = roots_response.get("result", {}).get("roots", [])
        uri = roots[0].get("uri", "") if roots else ""
        if roots_response.get("id") != "server-roots-1" or not uri.startswith("file://") or "sigil%20mcp%20root" not in uri:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid roots response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_name_limit_and_roots(
        &mut registry,
        &[mcp_server_config! {
            name: "roots".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        64,
        vec![workspace_root],
        SecretRedactor::empty(),
        super::unsupported_mcp_elicitation_handler(),
    )
    .await?;

    assert!(registry.spec_for("mcp__roots__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_roots_list_blocks_secret_when_trust_disallows_secrets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("project-sk-secret");
    fs::create_dir_all(&workspace_root)?;
    let script = temp.path().join("secret_roots_request_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-roots-secret","method":"roots/list","params":{}})
        roots_response = read_message()
        error_message = roots_response.get("error", {}).get("message", "missing roots error")
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":error_message}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools_with_name_limit_and_roots(
        &mut registry,
        &[mcp_server_config! {
            name: "secret_roots".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        64,
        vec![workspace_root],
        SecretRedactor::from_values(["sk-secret"]),
        super::unsupported_mcp_elicitation_handler(),
    )
    .await
    .expect_err("secret-bearing roots/list should fail registration");

    let message = error.to_string();
    assert!(message.contains("roots/list would expose a secret"));
    assert!(message.contains("allow_secrets = false"));
    Ok(())
}

#[tokio::test]
async fn mcp_client_rejects_elicitation_requests_without_hanging() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("elicitation_request_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-elicitation-1","method":"elicitation/create","params":{"message":"Need input"}})
        elicitation_response = read_message()
        error = elicitation_response.get("error", {})
        if elicitation_response.get("id") != "server-elicitation-1" or error.get("code") != -32601:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid elicitation response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "elicitation".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.spec_for("mcp__elicitation__echo").is_some());
    Ok(())
}

#[derive(Debug)]
struct AcceptingElicitationHandler;

#[async_trait::async_trait]
impl McpElicitationHandler for AcceptingElicitationHandler {
    fn supports_elicitation(&self) -> bool {
        true
    }

    async fn elicit(&self, request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        assert_eq!(request.server_name, "elicitation");
        assert_eq!(request.message, "[redacted]");
        assert!(request.requested_schema.get("properties").is_some());
        assert!(
            serde_json::to_string(&request.requested_schema)
                .expect("schema should serialize")
                .contains("[redacted]")
        );
        Ok(McpElicitationResponse::accept(serde_json::json!({
            "value": "accepted from test"
        })))
    }
}

#[tokio::test]
async fn mcp_client_answers_elicitation_requests_with_handler() -> Result<()> {
    if std::env::var("HOME").is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("elicitation_supported_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, os, sys

SECRET = os.environ["HOME"]

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        params = message.get("params", {})
        capabilities = params.get("capabilities", {})
        if params.get("protocolVersion") != "2025-06-18" or "elicitation" not in capabilities:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"elicitation capability missing"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"elicitation","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-elicitation-1","method":"elicitation/create","params":{"message":SECRET,"requestedSchema":{"type":"object","properties":{"value":{"type":"string","title":SECRET}},"required":["value"]}}})
        elicitation_response = read_message()
        result = elicitation_response.get("result", {})
        content = result.get("content", {})
        if elicitation_response.get("id") != "server-elicitation-1" or result.get("action") != "accept" or content.get("value") != "accepted from test":
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid elicitation response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        &mut registry,
        &[mcp_server_config! {
            name: "elicitation".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            inherit_env: vec!["HOME".to_owned()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::empty(),
        std::sync::Arc::new(AcceptingElicitationHandler),
    )
    .await?;

    assert!(registry.spec_for("mcp__elicitation__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_provider_visible_names_are_sanitized_truncated_and_unique() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("many_tools_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

tools = [
    {"name":"alpha tool","description":"Alpha","inputSchema":{"type":"object"}},
    {"name":"alpha-tool","description":"Alpha collision","inputSchema":{"type":"object"}},
    {"name":"very-long-tool-name-" + "x" * 80,"description":"Long","inputSchema":{"type":"object"}}
]

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":tools}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_name_limit(
        &mut registry,
        &[mcp_server_config! {
            name: "bad server!".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        48,
    )
    .await?;

    let names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(names.iter().all(|name| name.len() <= 48));
    assert!(names.iter().all(|name| {
        name.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }));
    assert!(
        names
            .iter()
            .any(|name| name == "mcp__bad_server__alpha_tool")
    );
    assert_eq!(
        names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        names.len()
    );
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("mcp__bad_server__very_long") && name.contains("__"))
    );
    Ok(())
}

#[tokio::test]
async fn same_tool_names_from_different_mcp_servers_stay_isolated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("same_tool_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_name_limit(
        &mut registry,
        &[
            mcp_server_config! {
                name: "server-a".to_owned(),
                command: "python3".to_owned(),
                args: vec![script.to_string_lossy().to_string()],
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            },
            mcp_server_config! {
                name: "server-b".to_owned(),
                command: "python3".to_owned(),
                args: vec![script.to_string_lossy().to_string()],
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            },
        ],
        64,
    )
    .await?;

    assert!(registry.spec_for("mcp__server_a__echo").is_some());
    assert!(registry.spec_for("mcp__server_b__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn capability_and_lazy_wrapper_apis_register_expected_tools() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("wrapper_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let eager = mcp_server_config! {
        name: "wrapper-eager".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().to_string()],
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    let lazy = mcp_server_config! {
        name: "wrapper-lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().to_string()],
        startup_timeout_secs: 5,
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_and_roots(
        &mut registry,
        std::slice::from_ref(&eager),
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
    )
    .await?;
    assert!(registry.spec_for("mcp__wrapper_eager__echo").is_some());

    activate_lazy_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[lazy],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::empty(),
    )
    .await?;
    assert!(registry.spec_for("mcp__wrapper_lazy__echo").is_some());
    Ok(())
}

#[test]
fn helper_types_cover_defaults_summaries_and_paths() {
    assert_eq!(
        McpElicitationResponse::accept(json!({"name":"sigil"})).into_result(),
        json!({ "action": "accept", "content": { "name": "sigil" } })
    );
    assert_eq!(
        McpElicitationResponse {
            action: super::McpElicitationAction::Accept,
            content: None,
        }
        .into_result(),
        json!({ "action": "accept", "content": {} })
    );
    assert_eq!(
        McpElicitationResponse::decline().into_result(),
        json!({ "action": "decline" })
    );
    assert_eq!(
        McpElicitationResponse::cancel().into_result(),
        json!({ "action": "cancel" })
    );

    let handler = super::unsupported_mcp_elicitation_handler();
    assert!(!handler.supports_elicitation());
    let request = super::mcp_elicitation_request("demo", &json!({ "params": {} }))
        .expect("default elicitation request should build");
    assert_eq!(request.server_name, "demo");
    assert_eq!(request.message, "MCP server requested input");
    assert_eq!(
        request.requested_schema,
        json!({ "type": "object", "properties": {} })
    );
    assert!(super::mcp_elicitation_request("demo", &json!({})).is_err());

    assert_eq!(
        super::summarize_egress_json(&json!(["a", "b"]))["item_count"],
        2
    );
    assert_eq!(super::summarize_egress_json(&json!(true))["type"], "bool");
    assert_eq!(super::json_type_label(&Value::Null), "null");
    assert_eq!(super::sanitize_provider_name_part("!!"), "tool");
    let hashed = super::provider_name_with_hash("abcdef", "identity", 6);
    assert!(hashed.len() > 6);
    assert!(hashed.contains("__"));
    assert_eq!(super::stable_hash("abc"), super::stable_hash("abc"));
    assert_eq!(super::root_name(std::path::Path::new("/")), "workspace");
    assert!(super::file_uri(std::path::Path::new("relative dir/file.rs")).starts_with("file://"));
}

#[test]
fn mcp_egress_json_summary_handles_arrays_and_scalars() {
    let array = super::summarize_egress_json(&json!(["a", 1, true]));
    assert_eq!(array["type"], "array");
    assert_eq!(array["item_count"], 3);

    let scalar = super::summarize_egress_json(&json!(true));
    assert_eq!(scalar["type"], "bool");
}

#[test]
fn mcp_elicitation_request_and_response_defaults_are_stable() -> Result<()> {
    let request = super::mcp_elicitation_request(
        "server",
        &json!({
            "params": {}
        }),
    )?;
    assert_eq!(request.server_name, "server");
    assert_eq!(request.message, "MCP server requested input");
    assert_eq!(request.requested_schema["type"], "object");

    let accept_without_content = McpElicitationResponse {
        action: super::McpElicitationAction::Accept,
        content: None,
    }
    .into_result();
    assert_eq!(accept_without_content["action"], "accept");
    assert_eq!(accept_without_content["content"], json!({}));

    assert_eq!(
        McpElicitationResponse::decline().into_result(),
        json!({ "action": "decline" })
    );
    assert_eq!(
        McpElicitationResponse::cancel().into_result(),
        json!({ "action": "cancel" })
    );

    let error = super::mcp_elicitation_request("server", &json!({}))
        .expect_err("missing params should be rejected");
    assert!(
        error
            .to_string()
            .contains("MCP elicitation/create missing params object")
    );
    Ok(())
}

#[test]
fn mcp_name_and_uri_helpers_sanitize_and_encode() {
    assert_eq!(
        super::sanitize_provider_name_part("bad name///tool"),
        "bad_name_tool"
    );
    assert_eq!(super::sanitize_provider_name_part("!!!"), "tool");
    assert_eq!(
        super::root_name(std::path::Path::new("/tmp/test-root")),
        "test-root"
    );
    let file_uri = super::file_uri(std::path::Path::new("/tmp/space name.txt"));
    assert!(file_uri.starts_with("file:///"));
    assert!(file_uri.ends_with("/tmp/space%20name.txt"));
    assert!(!file_uri.contains("%3A"));
}

#[test]
fn mcp_name_hashing_handles_extremely_short_provider_limits() {
    assert_eq!(
        super::fit_provider_name_with_hash("short", "identity", 16),
        "short"
    );
    let fitted = super::fit_provider_name_with_hash("very_long_provider_tool_name", "identity", 6);
    assert!(fitted.contains("__"));
    assert!(fitted.len() > 6);
}

#[test]
fn mcp_private_helpers_cover_pin_json_and_name_collision_edges() -> Result<()> {
    assert_eq!(
        super::mcp_provider_tool_name_prefix("bad server!"),
        "mcp__bad_server__"
    );
    assert_eq!(super::json_type_label(&Value::Null), "null");
    assert_eq!(super::json_type_label(&json!(false)), "bool");
    assert_eq!(super::json_type_label(&json!(1)), "number");
    assert_eq!(super::json_type_label(&json!("value")), "string");
    assert_eq!(super::json_type_label(&json!([])), "array");
    assert_eq!(super::json_type_label(&json!({})), "object");
    assert_eq!(super::summarize_egress_json(&Value::Null)["type"], "null");

    let observed = super::McpServerObservedIdentity {
        transport_fingerprint: "sha256:observed".to_owned(),
        process_authorization_fingerprint: "hmac-sha256:observed".to_owned(),
        declaration: None,
        environment_grant_names: Vec::new(),
        environment_static_fingerprint: "sha256:environment".to_owned(),
        environment_live_fingerprint: "hmac-sha256:environment".to_owned(),
        protocol_version: "2025-06-18".to_owned(),
        server_name: "observed-server".to_owned(),
        server_version: "2.0.0".to_owned(),
    };
    let disabled = mcp_server_config! {
        name: "unmatched".to_owned(),
        trust: McpServerTrustPolicy {
            pin_version: false,
            ..McpServerTrustPolicy::default()
        },
        ..McpServerConfig::default()
    };
    super::validate_mcp_pin(&disabled, &observed)?;

    let matching = mcp_server_config! {
        name: "matched".to_owned(),
        trust: McpServerTrustPolicy {
            pin_version: true,
            pinned: Some(observed.as_pinned_identity()),
            ..McpServerTrustPolicy::default()
        },
        ..McpServerConfig::default()
    };
    super::validate_mcp_pin(&matching, &observed)?;

    let mismatched = mcp_server_config! {
        name: "mismatched".to_owned(),
        trust: McpServerTrustPolicy {
            pin_version: true,
            pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                transport_fingerprint: "sha256:expected".to_owned(),
                protocol_version: "2024-11-05".to_owned(),
                server_name: "expected-server".to_owned(),
                server_version: "1.0.0".to_owned(),
            }),
            ..McpServerTrustPolicy::default()
        },
        ..McpServerConfig::default()
    };
    let error = super::validate_mcp_pin(&mismatched, &observed)
        .expect_err("pin mismatch should include every mismatched field");
    let message = error.to_string();
    assert!(
        message.contains("transport_fingerprint expected sha256:expected observed sha256:observed")
    );
    assert!(message.contains("protocol_version expected 2024-11-05 observed 2025-06-18"));
    assert!(message.contains("server_name expected expected-server observed observed-server"));
    assert!(message.contains("server_version expected 1.0.0 observed 2.0.0"));

    let mut used = std::collections::BTreeSet::new();
    let first = super::McpToolName::new("server", "same tool", 24, &mut used);
    let second = super::McpToolName::new("server", "same-tool", 24, &mut used);
    assert_ne!(first.provider_name, second.provider_name);
    assert_eq!(first.server_name, "server");
    assert_eq!(first.original_name, "same tool");
    Ok(())
}

#[tokio::test]
async fn mcp_public_capability_wrappers_handle_empty_server_lists() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let capabilities = ProviderCapabilities {
        tool_name_max_chars: 32,
        ..test_provider_capabilities()
    };
    let mut registry = ToolRegistry::new();

    register_mcp_tools_with_capabilities(&mut registry, &[], &capabilities).await?;
    register_mcp_tools_with_capabilities_and_roots(
        &mut registry,
        &[],
        &capabilities,
        vec![temp.path().to_path_buf()],
    )
    .await?;
    activate_lazy_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[],
        &capabilities,
        vec![temp.path().to_path_buf()],
        SecretRedactor::empty(),
    )
    .await?;
    activate_lazy_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        &mut registry,
        &[],
        &capabilities,
        vec![temp.path().to_path_buf()],
        SecretRedactor::empty(),
        super::unsupported_mcp_elicitation_handler(),
    )
    .await?;

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn mcp_tool_without_description_uses_default_description() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("default_description_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"bare","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "bare".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let spec = registry
        .spec_for("mcp__bare__bare")
        .expect("bare mcp tool should register");
    assert_eq!(spec.description, "MCP tool");
    Ok(())
}

#[tokio::test]
async fn optional_mcp_server_tools_list_failure_is_skipped() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("tools_list_error_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"tools are unavailable"}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "optional-tools".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            required: false,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn unsupported_elicitation_handler_rejects_requests() {
    let handler = super::unsupported_mcp_elicitation_handler();
    let error = handler
        .elicit(McpElicitationRequest {
            server_name: "fake".to_owned(),
            message: "need input".to_owned(),
            requested_schema: json!({"type":"object"}),
        })
        .await
        .expect_err("unsupported handler should fail");
    assert!(error.to_string().contains("not supported"));
}

#[test]
fn unsupported_elicitation_handler_reports_capability() {
    let handler = super::unsupported_mcp_elicitation_handler();
    assert!(!handler.supports_elicitation());
}

#[tokio::test]
async fn mcp_tool_execute_errors_when_call_result_is_missing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("missing_result_call_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"]})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "missing-result".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-missing-result".to_owned(),
                name: "mcp__missing_result__echo".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;
    let ToolResultStatus::Error(error) = result.status else {
        panic!("missing result payload must be a structured protocol error");
    };
    assert_eq!(error.kind, ToolErrorKind::Protocol);
    assert_eq!(error.details["mcp"]["code"], "invalid_jsonrpc_envelope");
    Ok(())
}

#[tokio::test]
async fn mcp_tool_execute_supports_string_content_results() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("string_content_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":"plain text"}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "string-content".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            sigil_kernel::ToolCall {
                id: "call-string-content".to_owned(),
                name: "mcp__string_content__echo".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;

    assert_eq!(result.content, "plain text");
    Ok(())
}

#[tokio::test]
async fn mcp_client_returns_unknown_method_errors_to_server() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("unknown_method_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-unsupported-1","method":"client/ping","params":{}})
        unsupported_response = read_message()
        error = unsupported_response.get("error", {})
        if unsupported_response.get("id") != "server-unsupported-1" or error.get("code") != -32601:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid unsupported response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[mcp_server_config! {
            name: "unsupported".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.spec_for("mcp__unsupported__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_client_returns_handler_errors_to_server() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("elicitation_error_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"error","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-elicitation-error-1","method":"elicitation/create","params":{"message":"Need input","requestedSchema":{"type":"object"}}})
        error_response = read_message()
        error = error_response.get("error", {})
        if error_response.get("id") != "server-elicitation-error-1" or error.get("code") != -32000 or "handler boom" not in error.get("message", ""):
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid handler error response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        &mut registry,
        &[mcp_server_config! {
            name: "error".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::empty(),
        std::sync::Arc::new(ErroringElicitationHandler),
    )
    .await?;

    assert!(registry.spec_for("mcp__error__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_client_blocks_secret_elicitation_response_when_trust_disallows_secrets() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("elicitation_secret_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"secret","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-elicitation-secret-1","method":"elicitation/create","params":{"message":"Need input","requestedSchema":{"type":"object"}}})
        error_response = read_message()
        error = error_response.get("error", {})
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":error.get("message","missing secret error")}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        &mut registry,
        &[mcp_server_config! {
            name: "secret".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
        std::sync::Arc::new(SecretElicitationHandler),
    )
    .await
    .expect_err("secret-bearing elicitation response should fail");

    assert!(
        error
            .to_string()
            .contains("elicitation response contains a secret")
    );
    Ok(())
}

#[test]
fn validate_mcp_pin_reports_all_supported_mismatch_fields() {
    let error = super::validate_mcp_pin(
        &mcp_server_config! {
            name: "pin".to_owned(),
            trust: McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    transport_fingerprint: "expected-fingerprint".to_owned(),
                    protocol_version: "expected-protocol".to_owned(),
                    server_name: "expected-name".to_owned(),
                    server_version: "expected-version".to_owned(),
                }),
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        },
        &super::McpServerObservedIdentity {
            transport_fingerprint: "observed-fingerprint".to_owned(),
            process_authorization_fingerprint: "observed-authorization".to_owned(),
            declaration: None,
            environment_grant_names: Vec::new(),
            environment_static_fingerprint: "sha256:environment".to_owned(),
            environment_live_fingerprint: "hmac-sha256:environment".to_owned(),
            protocol_version: "observed-protocol".to_owned(),
            server_name: "expected-name".to_owned(),
            server_version: "observed-version".to_owned(),
        },
    )
    .expect_err("mismatch should fail pin validation");

    let message = error.to_string();
    assert!(message.contains(
        "transport_fingerprint expected expected-fingerprint observed observed-fingerprint"
    ));
    assert!(
        message.contains("protocol_version expected expected-protocol observed observed-protocol")
    );
    assert!(message.contains("server_version expected expected-version observed observed-version"));
}

#[tokio::test]
async fn read_message_reports_stream_close_and_invalid_frames() -> Result<()> {
    let mut closed = python_stdout_reader("import sys")?;
    let closed_error = super::read_message(&mut closed)
        .await
        .expect_err("closed stdout should fail");
    assert!(closed_error.to_string().contains("stream closed"));

    let mut missing_newline =
        python_stdout_reader("import sys; sys.stdout.write('{}'); sys.stdout.flush()")?;
    let missing_newline_error = super::read_message(&mut missing_newline)
        .await
        .expect_err("missing newline should fail");
    assert!(
        missing_newline_error
            .to_string()
            .contains("newline delimiter")
    );

    let mut invalid_json =
        python_stdout_reader("import sys; sys.stdout.write('not-json\\n'); sys.stdout.flush()")?;
    let invalid_json_error = super::read_message(&mut invalid_json)
        .await
        .expect_err("invalid JSON should fail");
    assert!(invalid_json_error.to_string().contains("not valid JSON"));
    Ok(())
}

#[derive(Debug)]
struct ErroringElicitationHandler;

#[async_trait::async_trait]
impl McpElicitationHandler for ErroringElicitationHandler {
    fn supports_elicitation(&self) -> bool {
        true
    }

    async fn elicit(&self, _request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        anyhow::bail!("handler boom")
    }
}

#[derive(Debug)]
struct SecretElicitationHandler;

#[async_trait::async_trait]
impl McpElicitationHandler for SecretElicitationHandler {
    fn supports_elicitation(&self) -> bool {
        true
    }

    async fn elicit(&self, _request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        Ok(McpElicitationResponse::accept(json!({"value":"sk-secret"})))
    }
}

#[derive(Debug, Default)]
struct RecordingMcpRuntimeEventHandler {
    progress: Mutex<Vec<McpProgressNotification>>,
    list_changed: Mutex<Vec<McpListChangedNotification>>,
}

#[async_trait::async_trait]
impl McpRuntimeEventHandler for RecordingMcpRuntimeEventHandler {
    async fn progress(&self, notification: McpProgressNotification) -> Result<()> {
        self.progress
            .lock()
            .expect("progress lock should not be poisoned")
            .push(notification);
        Ok(())
    }

    async fn list_changed(&self, notification: McpListChangedNotification) -> Result<()> {
        self.list_changed
            .lock()
            .expect("list_changed lock should not be poisoned")
            .push(notification);
        Ok(())
    }
}

fn python_stdout_reader(script: &str) -> Result<BufReader<ChildStdout>> {
    let mut child = Command::new("python3")
        .args(["-c", script])
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .expect("python child stdout should exist");
    std::mem::forget(child);
    Ok(BufReader::new(stdout))
}
