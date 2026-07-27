use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sigil_kernel::{JsonlSessionStore, ModelMessage, RootConfig, Session};
use sigil_runtime::{provider_connections::resolve_default_model_route, resolve_sigil_paths};

fn test_workspace(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("sigil-intent-automation-{name}-"))
        .tempdir()
        .expect("test workspace should create")
}

fn write_config(workspace: &Path) -> PathBuf {
    let config_path = workspace.join("sigil.toml");
    let config = format!(
        r#"config_version = 2

[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
connection = "local-test"
model = "gpt-4.1"

[permission]
mode = "manual"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = {{ source = "none" }}
"#,
        workspace.join("state").display(),
        workspace.join("cache").display()
    );
    fs::write(&config_path, config).expect("test config should write");
    config_path
}

fn durable_session(config_path: &Path, workspace: &Path) -> String {
    let config = RootConfig::load(config_path).expect("config should load");
    let paths = resolve_sigil_paths(&config.storage, &config.session, workspace);
    fs::create_dir_all(&paths.session_log_dir).expect("session directory should create");
    let session_path = paths.session_log_dir.join("intent-automation.jsonl");
    let store = JsonlSessionStore::new(session_path).expect("session store should create");
    let (provider_name, route) =
        resolve_default_model_route(&config).expect("model route should resolve");
    let mut session = Session::load_from_store_with_route(
        provider_name,
        route.model_ref.model_id.clone(),
        Some(route),
        store,
    )
    .expect("session identity should initialize");
    session
        .append_user_message(ModelMessage::user("inspect durable Intent Stack"))
        .expect("session should initialize");
    session.session_scope_id().to_owned()
}

fn run_intent(config_path: &Path, workspace: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(workspace)
        .arg("--config")
        .arg(config_path)
        .arg("intent")
        .args(arguments)
        .output()
        .expect("sigil intent should execute")
}

#[test]
fn inspect_emits_one_typed_path_free_record_for_the_exact_durable_session() {
    let workspace = test_workspace("inspect");
    let config_path = write_config(workspace.path());
    let session_id = durable_session(&config_path, workspace.path());

    let output = run_intent(
        &config_path,
        workspace.path(),
        &["--session", &session_id, "inspect"],
    );

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(stdout.lines().count(), 1);
    let record: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be one JSON record");
    assert_eq!(record["record_type"], "result");
    assert_eq!(record["protocol_version"], 1);
    assert_eq!(record["session_id"], session_id);
    assert_eq!(record["output"]["result"], "projection");
    assert_eq!(record["output"]["state"]["status"], "history_unavailable");
    let encoded = record.to_string();
    let workspace_path = workspace.path().to_string_lossy();
    for forbidden in [
        workspace_path.as_ref(),
        "session_log_path",
        "workspace_root",
        "approval_authority",
        "permission_policy",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn unknown_session_emits_a_stable_typed_error_without_path_disclosure() {
    let workspace = test_workspace("missing");
    let config_path = write_config(workspace.path());

    let output = run_intent(
        &config_path,
        workspace.path(),
        &["--session", "missing-session", "inspect"],
    );

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(stdout.lines().count(), 1);
    let record: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be one JSON record");
    assert_eq!(record["record_type"], "error");
    assert_eq!(record["session_id"], "missing-session");
    assert_eq!(record["error"]["code"], "session_not_found");
    assert_eq!(record["error"]["retryable"], false);
    let workspace_path = workspace.path().to_string_lossy();
    assert!(!record.to_string().contains(workspace_path.as_ref()));
}
