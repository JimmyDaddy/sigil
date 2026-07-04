use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use serde_json::json;
use sigil_kernel::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk, DurableEventType,
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupStatus,
    ExecutionConfig, ExecutionNetworkPolicy, ExecutionReceipt, ExecutionRequest,
    ExecutionResourceLimitKind, ExecutionSandboxFallback, ExecutionSandboxProfile,
    ExecutionSandboxStrategyConfig, ExecutionTimeoutSource, JsonlSessionStore,
    MutationEventRecorder, PathTrustZone, PermissionRisk, SessionStreamRecord,
    TerminalExecutionBackendCapabilities, TerminalExecutionBackendKind, TerminalTaskEntry,
    TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus, Tool, ToolAccess, ToolCall,
    ToolContext, ToolErrorKind, ToolOperation, ToolPreviewCapability, ToolProgressEvent,
    ToolProgressSink, ToolRegistry, ToolResultStatus, ToolSubjectKind, ToolSubjectScope,
};
use tokio::time::{Duration, sleep};

use super::{
    ApplyChangeSetTool, BashTool, BuiltinToolPaths, ChangeSetArtifactStore, DeleteFileTool,
    DockerExecutionBackend, EditFileTool, GlobTool, GrepTool, LinuxBubblewrapExecutionBackend,
    ListTool, LocalExecutionBackend, MacosSeatbeltExecutionBackend, ReadFileTool,
    TerminalInputTool, TerminalProcessManagers, TerminalStartRequest, TerminalStartTool,
    WriteFileTool, register_builtin_tools, register_builtin_tools_with_paths,
};

use serial_test::serial;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

fn bash_tool(test_root: &Path) -> BashTool {
    BashTool {
        scratch_root: test_root.join("scratch-cache").join("tmp"),
        scratch_label: "cache/tmp".to_owned(),
        backend: Arc::new(LocalExecutionBackend),
    }
}

fn sandbox_execution_config(
    backend: ExecutionBackendKind,
    profile: ExecutionSandboxProfile,
    fallback: ExecutionSandboxFallback,
    container_image: Option<String>,
) -> ExecutionConfig {
    let mut sandbox = ExecutionSandboxStrategyConfig::new(backend);
    sandbox.profile = profile;
    sandbox.fallback = fallback;
    sandbox.container_image = container_image;
    ExecutionConfig::sandbox(sandbox)
}

fn tool_context_with_mutation_recorder(workspace: &Path, timeout_secs: u64) -> Result<ToolContext> {
    let store = JsonlSessionStore::new(workspace.join("session.jsonl"))?;
    Ok(ToolContext::new(workspace.to_path_buf(), timeout_secs)
        .with_mutation_recorder(MutationEventRecorder::new(store)))
}

struct RecordingProgressSink {
    events: Arc<Mutex<Vec<ToolProgressEvent>>>,
}

impl ToolProgressSink for RecordingProgressSink {
    fn emit(&self, event: ToolProgressEvent) -> Result<()> {
        self.events
            .lock()
            .expect("progress event lock should not be poisoned")
            .push(event);
        Ok(())
    }
}

#[test]
fn module_split_facade_registers_tools_paths_and_backend_contracts() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    let names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    for expected in [
        "read_file",
        "write_file",
        "edit_file",
        "delete_file",
        "apply_changeset",
        "bash",
        "terminal_start",
        "terminal_read",
        "terminal_input",
        "terminal_cancel",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing builtin tool from split facade: {expected}"
        );
    }

    let paths = BuiltinToolPaths::workspace_defaults(Path::new("/workspace"));
    assert_eq!(paths.scratch_label, "cache/tmp");
    assert!(paths.scratch_root.ends_with("cache/tmp"));

    let backend = super::build_execution_backend(&ExecutionConfig::default())?;
    assert_eq!(backend.kind(), ExecutionBackendKind::Local);
    Ok(())
}

#[test]
fn local_execution_backend_policy_fails_closed_when_sandbox_required() -> Result<()> {
    let backend = super::build_execution_backend(&ExecutionConfig::default())?;
    assert_eq!(backend.kind(), ExecutionBackendKind::Local);

    let result = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::Local,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    ));
    let Err(error) = result else {
        panic!("local backend cannot satisfy required sandbox policy");
    };
    assert!(
        error
            .to_string()
            .contains("execution profile WorkspaceWrite requires filesystem and process isolation")
    );
    Ok(())
}

#[test]
fn long_lived_stdio_process_plan_local_unconfined_is_outside_sandbox() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let plan = super::long_lived_stdio_process_plan(
        &ExecutionConfig::default(),
        "sh",
        &["-c".to_owned(), "true".to_owned()],
        temp.path(),
        &BTreeMap::new(),
    )?;

    assert_eq!(plan.backend, ExecutionBackendKind::Local);
    assert_eq!(plan.sandbox_profile, ExecutionSandboxProfile::Unconfined);
    assert!(!plan.sandboxed);
    assert_eq!(plan.program, PathBuf::from("sh"));
    Ok(())
}

#[test]
fn long_lived_stdio_process_plan_local_required_sandbox_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = super::long_lived_stdio_process_plan(
        &sandbox_execution_config(
            ExecutionBackendKind::Local,
            ExecutionSandboxProfile::WorkspaceWrite,
            ExecutionSandboxFallback::Deny,
            None,
        ),
        "sh",
        &["-c".to_owned(), "true".to_owned()],
        temp.path(),
        &BTreeMap::new(),
    );

    let Err(error) = result else {
        panic!("local stdio MCP process must fail closed when sandbox is required");
    };
    assert!(
        error
            .to_string()
            .contains("local execution backend cannot enforce local stdio sandbox")
    );
    Ok(())
}

#[test]
fn long_lived_stdio_process_plan_docker_fails_closed_for_stdio_mcp() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = super::long_lived_stdio_process_plan(
        &sandbox_execution_config(
            ExecutionBackendKind::Docker,
            ExecutionSandboxProfile::WorkspaceWrite,
            ExecutionSandboxFallback::Deny,
            Some("redis:8-alpine".to_owned()),
        ),
        "sh",
        &["-c".to_owned(), "true".to_owned()],
        temp.path(),
        &BTreeMap::new(),
    );

    let Err(error) = result else {
        panic!("docker stdio MCP process must fail closed until container lifecycle is supported");
    };
    assert!(
        error
            .to_string()
            .contains("docker execution backend does not support long-lived stdio MCP processes")
    );
    Ok(())
}

#[test]
fn terminal_entry_details_serializes_execution_backend_metadata() -> Result<()> {
    let entry = TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new("terminal-details")?,
            command: "cargo test".to_owned(),
            cwd: ".".into(),
            shell: "zsh".to_owned(),
            log_path: "state/artifacts/tasks/terminal-details/output.log".into(),
            created_at_ms: 100,
            execution_backend: Some(TerminalExecutionBackendKind::LocalPty),
            execution_backend_capabilities: Some(TerminalExecutionBackendCapabilities::local_pty()),
            enforcement_backend: Some(sigil_kernel::ExecutionBackendKind::Local),
            enforcement_backend_capabilities: Some(
                sigil_kernel::ExecutionBackendCapabilities::default(),
            ),
            sandbox_profile: Some(sigil_kernel::ExecutionSandboxProfile::Unconfined),
        },
        status: TerminalTaskStatus::Running,
        output_preview: Some("tail".to_owned()),
        output_hash: Some("sha256:terminal".to_owned()),
        output_truncated: false,
        cleanup: None,
        updated_at_ms: 120,
    };

    let details = super::terminal_entry_details(&entry, None);
    let workspace = tempfile::tempdir()?;
    let analysis = super::analyze_shell_command(workspace.path(), "cargo check 2>&1 | tail -20")?;
    let shell_details = super::terminal_entry_details(&entry, Some(&analysis));

    assert_eq!(details["execution_backend"], json!("local_pty"));
    assert_eq!(details["enforcement_backend"], json!("local"));
    assert_eq!(details["sandbox_profile"], json!("unconfined"));
    assert_eq!(
        details["execution_backend_capabilities"]["persistent_pty"],
        json!(true)
    );
    assert_eq!(
        details["execution_backend_capabilities"]["input"],
        json!(true)
    );
    assert_eq!(
        shell_details["shell_analysis"]["command_family"],
        json!("cargo_check")
    );
    assert_eq!(
        shell_details["shell_analysis"]["grant_scope"],
        json!("workspace_check_family")
    );
    assert_eq!(shell_details["shell_analysis"]["verdict"], json!("running"));
    Ok(())
}

#[test]
fn macos_seatbelt_backend_default_and_custom_paths_are_stable() {
    let default_backend = MacosSeatbeltExecutionBackend::default();
    assert_eq!(default_backend.kind(), ExecutionBackendKind::MacosSeatbelt);

    let custom_path = PathBuf::from("/tmp/custom-sandbox-exec");
    let custom_backend = MacosSeatbeltExecutionBackend::new(custom_path.clone());
    assert_eq!(custom_backend.kind(), ExecutionBackendKind::MacosSeatbelt);
    assert!(!custom_backend.is_available());
}

#[test]
#[cfg(target_os = "macos")]
fn macos_seatbelt_backend_satisfies_required_sandbox_policy() -> Result<()> {
    let backend = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::MacosSeatbelt,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    ))?;

    assert_eq!(backend.kind(), ExecutionBackendKind::MacosSeatbelt);
    let capabilities = backend.capabilities();
    assert!(capabilities.filesystem_isolation);
    assert!(!capabilities.network_isolation);
    assert!(capabilities.process_isolation);
    assert!(capabilities.persistent_pty);
    assert!(!capabilities.workspace_snapshot);
    Ok(())
}

#[test]
fn macos_seatbelt_backend_does_not_satisfy_offline_build_profile() {
    let backend = MacosSeatbeltExecutionBackend::default();
    let config = sandbox_execution_config(
        ExecutionBackendKind::MacosSeatbelt,
        sigil_kernel::ExecutionSandboxProfile::BuildOffline,
        ExecutionSandboxFallback::Deny,
        None,
    );

    let error = config
        .validate_profile_capabilities(backend.capabilities())
        .expect_err("build_offline requires proven network isolation");

    assert!(error.contains("network isolation"));
}

#[test]
fn linux_bubblewrap_backend_declares_enforced_mvp_capabilities() {
    let backend = LinuxBubblewrapExecutionBackend::new(PathBuf::from("/usr/bin/bwrap"), false);
    let capabilities = backend.capabilities();

    assert_eq!(backend.kind(), ExecutionBackendKind::LinuxBubblewrap);
    assert!(capabilities.filesystem_isolation);
    assert!(capabilities.network_isolation);
    assert!(capabilities.process_isolation);
    assert!(!capabilities.resource_limits);
    assert!(capabilities.persistent_pty);
    assert!(!capabilities.workspace_snapshot);
}

#[test]
#[cfg(not(target_os = "linux"))]
fn linux_bubblewrap_backend_fails_closed_on_non_linux() {
    let result = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::LinuxBubblewrap,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    ));
    let Err(error) = result else {
        panic!("linux_bubblewrap backend must fail closed on non-Linux");
    };
    assert!(
        error
            .to_string()
            .contains("linux_bubblewrap execution backend requires bwrap on PATH")
            || error
                .to_string()
                .contains("linux_bubblewrap execution backend is only available on Linux")
    );
}

#[test]
fn linux_bubblewrap_args_mount_workspace_scratch_and_disable_network_by_default() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let scratch = temp.path().join("scratch");
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&scratch)?;
    let canonical_workspace = fs::canonicalize(&workspace)?;
    let canonical_scratch = fs::canonicalize(&scratch)?;
    let request = ExecutionRequest {
        program: "sh".to_owned(),
        args: vec!["-c".to_owned(), "true".to_owned()],
        cwd: canonical_workspace.clone(),
        env: BTreeMap::from([(
            "SIGIL_SCRATCH_DIR".to_owned(),
            canonical_scratch.to_string_lossy().into_owned(),
        )]),
        timeout_ms: None,
        timeout_secs: 5,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    };

    let args = super::linux_bubblewrap_args(&canonical_workspace, &request, false)
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let workspace_text = canonical_workspace.to_string_lossy();
    let scratch_text = canonical_scratch.to_string_lossy();
    assert!(args.iter().any(|arg| arg == "--unshare-net"));
    assert!(args.windows(3).any(|window| {
        window[0] == "--bind"
            && window[1] == workspace_text.as_ref()
            && window[2] == workspace_text.as_ref()
    }));
    assert!(args.windows(3).any(|window| {
        window[0] == "--bind"
            && window[1] == scratch_text.as_ref()
            && window[2] == scratch_text.as_ref()
    }));
    assert!(
        args.windows(2)
            .any(|window| window[0] == "--chdir" && window[1] == workspace_text.as_ref())
    );

    let networked_args = super::linux_bubblewrap_args(&canonical_workspace, &request, true)
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(!networked_args.contains(&"--unshare-net".to_owned()));
    Ok(())
}

#[test]
fn linux_bubblewrap_args_keep_tmp_workspace_visible_after_tmpfs() {
    let canonical_workspace = PathBuf::from("/tmp/sigil-bwrap-test/workspace");
    let request = ExecutionRequest {
        program: "sh".to_owned(),
        args: vec!["-c".to_owned(), "true".to_owned()],
        cwd: canonical_workspace.clone(),
        env: BTreeMap::new(),
        timeout_ms: None,
        timeout_secs: 5,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    };

    let args = super::linux_bubblewrap_args(&canonical_workspace, &request, false)
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let workspace_text = canonical_workspace.to_string_lossy();
    let Some(parent) = canonical_workspace.parent() else {
        panic!("test workspace path should have a parent");
    };
    let parent_text = parent.to_string_lossy();
    let tmpfs_index = args
        .windows(2)
        .position(|window| window[0] == "--tmpfs" && window[1] == "/tmp")
        .expect("bubblewrap args should mount tmpfs /tmp");
    let dir_index = args
        .windows(2)
        .position(|window| window[0] == "--dir" && window[1] == parent_text.as_ref())
        .expect("bubblewrap args should recreate tmp workspace parent");
    let bind_index = args
        .windows(3)
        .position(|window| {
            window[0] == "--bind"
                && window[1] == workspace_text.as_ref()
                && window[2] == workspace_text.as_ref()
        })
        .expect("bubblewrap args should bind tmp workspace after tmpfs");

    assert!(
        tmpfs_index < dir_index && dir_index < bind_index,
        "tmpfs /tmp must be mounted before recreating and binding tmp workspace"
    );
}

#[tokio::test]
#[ignore = "requires Linux host with bubblewrap user/mount namespaces and wget"]
#[cfg(target_os = "linux")]
async fn linux_bubblewrap_execution_backend_real_conformance() -> Result<()> {
    let backend = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::LinuxBubblewrap,
        sigil_kernel::ExecutionSandboxProfile::BuildOffline,
        ExecutionSandboxFallback::Deny,
        None,
    ))?;
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("input.txt"), "from-host")?;
    let external_temp = tempfile::tempdir_in("/var/tmp")?;
    let external_path = external_temp.path().join("outside.txt");

    let receipt = backend
        .execute(ExecutionRequest {
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                concat!(
                    "command -v wget >/dev/null || { echo missing-wget >&2; exit 8; }; ",
                    "cat input.txt; ",
                    "printf from-bwrap > output.txt; ",
                    "if printf external > \"$OUTSIDE_PATH\" 2>/dev/null; ",
                    "then echo external-write-unexpected; exit 7; ",
                    "else echo external-write-blocked; fi; ",
                    "if wget -q -T 2 -O - https://example.com >/dev/null 2>&1; ",
                    "then echo network-unexpected; exit 9; ",
                    "else echo network-blocked; fi"
                )
                .to_owned(),
            ],
            cwd: workspace.clone(),
            env: BTreeMap::from([(
                "OUTSIDE_PATH".to_owned(),
                external_path.to_string_lossy().into_owned(),
            )]),
            timeout_ms: Some(10_000),
            timeout_secs: 10,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.backend, ExecutionBackendKind::LinuxBubblewrap);
    assert_eq!(receipt.network.policy, ExecutionNetworkPolicy::Denied);
    assert_eq!(receipt.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&receipt.stdout);
    assert!(stdout.contains("from-host"));
    assert!(stdout.contains("external-write-blocked"));
    assert!(stdout.contains("network-blocked"));
    assert_eq!(
        fs::read_to_string(workspace.join("output.txt"))?,
        "from-bwrap"
    );
    assert!(!external_path.exists());
    Ok(())
}

#[test]
fn docker_backend_requires_explicit_container_image() {
    let result = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::Docker,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    ));

    let Err(error) = result else {
        panic!("docker backend must fail closed without explicit container image");
    };

    assert!(
        error
            .to_string()
            .contains("docker execution backend requires execution.sandbox.container_image")
    );
}

#[test]
fn backend_selection_only_unconfined_fallback_relaxes_to_local() -> Result<()> {
    let prompt_result = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::Docker,
        ExecutionSandboxProfile::WorkspaceWrite,
        sigil_kernel::ExecutionSandboxFallback::Prompt,
        None,
    ));
    let Err(error) = prompt_result else {
        panic!("prompt fallback should not relax inside non-interactive backend builder");
    };
    assert!(error.to_string().contains("fallback requires user prompt"));

    let backend = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::Docker,
        ExecutionSandboxProfile::WorkspaceWrite,
        sigil_kernel::ExecutionSandboxFallback::Unconfined,
        None,
    ))?;

    assert_eq!(backend.kind(), ExecutionBackendKind::Local);
    Ok(())
}

#[test]
fn docker_backend_declares_only_enforced_mvp_capabilities() {
    let backend = DockerExecutionBackend::new(
        PathBuf::from("/usr/bin/docker"),
        "rust:1.94.1".to_owned(),
        false,
    );
    let capabilities = backend.capabilities();

    assert_eq!(backend.kind(), ExecutionBackendKind::Docker);
    assert_eq!(backend.image(), "rust:1.94.1");
    assert!(capabilities.filesystem_isolation);
    assert!(capabilities.network_isolation);
    assert!(capabilities.process_isolation);
    assert!(!capabilities.resource_limits);
    assert!(!capabilities.persistent_pty);
    assert!(!capabilities.workspace_snapshot);
}

#[test]
#[cfg(unix)]
fn docker_backend_checks_daemon_and_configured_image_before_selection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let docker = temp.path().join("docker");
    let calls_path = temp.path().join("calls.txt");
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n---\\n' \"$@\" >> {}\ncase \"$1 $2\" in\n  'version --format') printf '29.3.0\\n' ;;\n  'image inspect') printf '{{}}\\n' ;;\n  *) printf 'unexpected docker check' >&2; exit 9 ;;\nesac\n",
            calls_path.display()
        ),
    )?;
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755))?;
    let backend = DockerExecutionBackend::new(docker, "rust:1.94.1".to_owned(), false);

    super::ensure_docker_available(&backend)?;

    let calls = fs::read_to_string(calls_path)?;
    assert!(calls.contains("version\n---\n--format\n---\n{{.Server.Version}}\n---\n"));
    assert!(calls.contains("image\n---\ninspect\n---\nrust:1.94.1\n---\n"));
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn docker_execution_backend_builds_offline_container_command() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let docker = temp.path().join("docker");
    let args_path = temp.path().join("args.txt");
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf fake-docker-ok\n",
            args_path.display()
        ),
    )?;
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755))?;
    let backend = DockerExecutionBackend::new(docker, "rust:1.94.1".to_owned(), false);

    let receipt = backend
        .execute(ExecutionRequest {
            program: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
            ],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::from([("RUST_LOG".to_owned(), "debug".to_owned())]),
            timeout_ms: None,
            timeout_secs: 5,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.backend, ExecutionBackendKind::Docker);
    assert_eq!(receipt.network.policy, ExecutionNetworkPolicy::Denied);
    assert!(
        receipt
            .network
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("--network none")
    );
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(String::from_utf8_lossy(&receipt.stdout), "fake-docker-ok");
    let args = fs::read_to_string(args_path)?;
    assert!(args.contains("run\n"));
    assert!(args.contains("--rm\n"));
    assert!(args.contains("--workdir\n"));
    assert!(args.contains("--mount\n"));
    assert!(args.contains("--network\nnone\n"));
    assert!(args.contains("--env\nRUST_LOG=debug\n"));
    assert!(args.contains("rust:1.94.1\ncargo\ntest\n-p\nsigil-kernel\n"));
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn docker_execution_backend_networked_receipt_allows_network() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let docker = temp.path().join("docker");
    let args_path = temp.path().join("args.txt");
    fs::write(
        &docker,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf fake-docker-ok\n",
            args_path.display()
        ),
    )?;
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755))?;
    let backend = DockerExecutionBackend::new(docker, "rust:1.94.1".to_owned(), true);

    let receipt = backend
        .execute(ExecutionRequest {
            program: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 5,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.network.policy, ExecutionNetworkPolicy::Allowed);
    let args = fs::read_to_string(args_path)?;
    assert!(!args.contains("--network\nnone\n"));
    Ok(())
}

#[tokio::test]
#[ignore = "requires a healthy local Docker daemon and SIGIL_DOCKER_CONFORMANCE_IMAGE with sh+wget"]
#[cfg(unix)]
async fn docker_execution_backend_real_daemon_conformance() -> Result<()> {
    let image = std::env::var("SIGIL_DOCKER_CONFORMANCE_IMAGE")
        .context("set SIGIL_DOCKER_CONFORMANCE_IMAGE to a local image with sh and wget")?;
    let backend = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::Docker,
        sigil_kernel::ExecutionSandboxProfile::BuildOffline,
        ExecutionSandboxFallback::Deny,
        Some(image),
    ))?;
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("input.txt"), "from-host")?;

    let receipt = backend
        .execute(ExecutionRequest {
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                concat!(
                    "command -v wget >/dev/null || { echo missing-wget >&2; exit 8; }; ",
                    "cat input.txt; ",
                    "printf from-container > output.txt; ",
                    "if wget -q -T 2 -O - https://example.com >/dev/null 2>&1; ",
                    "then echo network-unexpected; exit 7; ",
                    "else echo network-blocked; fi"
                )
                .to_owned(),
            ],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: Some(10_000),
            timeout_secs: 10,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.backend, ExecutionBackendKind::Docker);
    assert_eq!(receipt.exit_code, Some(0));
    let stdout = String::from_utf8_lossy(&receipt.stdout);
    assert!(stdout.contains("from-host"));
    assert!(stdout.contains("network-blocked"));
    assert_eq!(
        fs::read_to_string(temp.path().join("output.txt"))?,
        "from-container"
    );
    let metadata = fs::metadata(temp.path().join("output.txt"))?;
    let expected_user = super::current_user_group_flag()
        .await?
        .expect("unix backend should report uid:gid");
    let expected_parts: Vec<_> = expected_user.split(':').collect();
    assert_eq!(metadata.uid().to_string(), expected_parts[0]);
    assert_eq!(metadata.gid().to_string(), expected_parts[1]);
    Ok(())
}

#[test]
#[cfg(target_os = "macos")]
fn macos_seatbelt_backend_missing_binary_fails_closed_during_validation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let backend = MacosSeatbeltExecutionBackend::new(temp.path().join("missing-sandbox-exec"));

    let error = super::ensure_macos_seatbelt_available(&backend)
        .expect_err("missing sandbox-exec should fail closed during validation");

    assert!(
        error
            .to_string()
            .contains("macos_seatbelt execution backend requires")
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn macos_seatbelt_backend_fails_closed_on_non_macos() {
    let result = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::MacosSeatbelt,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    ));

    let Err(error) = result else {
        panic!("macos_seatbelt backend must fail closed on non-macOS");
    };
    assert!(
        error
            .to_string()
            .contains("macos_seatbelt execution backend is only available on macOS")
    );
}

#[tokio::test]
async fn local_execution_backend_runs_command_without_sandbox_claims() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = LocalExecutionBackend;
    let capabilities = backend.capabilities();
    assert!(!capabilities.filesystem_isolation);
    assert!(!capabilities.network_isolation);
    assert!(!capabilities.process_isolation);

    let receipt = backend
        .execute(ExecutionRequest {
            program: "sh".to_owned(),
            args: vec!["-lc".to_owned(), "printf backend-ok".to_owned()],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 5,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.backend, ExecutionBackendKind::Local);
    assert_eq!(receipt.network.policy, ExecutionNetworkPolicy::Unknown);
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(String::from_utf8_lossy(&receipt.stdout), "backend-ok");
    assert!(receipt.stderr.is_empty());
    assert!(!receipt.timed_out);
    Ok(())
}

#[tokio::test]
async fn execution_backend_records_timeout_cleanup_and_unsupported_limits() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = LocalExecutionBackend;

    let receipt = backend
        .execute(ExecutionRequest {
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), "sleep 5".to_owned()],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: Some(20),
            timeout_secs: 1,
            cpu_time_ms: Some(100),
            memory_limit_bytes: Some(1024),
            process_count_limit: Some(2),
        })
        .await?;

    assert!(receipt.timed_out);
    assert_eq!(
        receipt.resources.timeout_source,
        ExecutionTimeoutSource::WallClock
    );
    assert_eq!(
        receipt.resources.cleanup.status,
        ExecutionCleanupStatus::Completed
    );
    assert!(receipt.resources.applied_limits.iter().any(|limit| {
        limit.kind == ExecutionResourceLimitKind::WallClockTimeout && limit.value == "20ms"
    }));
    assert_eq!(receipt.resources.unsupported_limits.len(), 3);
    assert!(receipt.resources.unsupported_limits.iter().any(|limit| {
        limit.kind == ExecutionResourceLimitKind::CpuTime && limit.value == "100ms"
    }));
    assert!(receipt.resources.unsupported_limits.iter().any(|limit| {
        limit.kind == ExecutionResourceLimitKind::Memory && limit.value == "1024 bytes"
    }));
    assert!(receipt.resources.unsupported_limits.iter().any(|limit| {
        limit.kind == ExecutionResourceLimitKind::ProcessCount && limit.value == "2 processes"
    }));
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn execution_backend_timeout_cleans_process_group_children() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let pid_file = temp.path().join("child.pid");
    let backend = LocalExecutionBackend;
    let script = format!("sleep 30 & echo $! > {}; wait", pid_file.display());

    let receipt = backend
        .execute(ExecutionRequest {
            program: "sh".to_owned(),
            args: vec!["-c".to_owned(), script],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: Some(100),
            timeout_secs: 1,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert!(receipt.timed_out);
    assert_eq!(
        receipt.resources.cleanup.status,
        ExecutionCleanupStatus::Completed
    );
    let pid = fs::read_to_string(pid_file)?.trim().to_owned();
    for _ in 0..20 {
        if !process_is_running(&pid) {
            return Ok(());
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("child process {pid} should have been cleaned up after timeout");
}

#[cfg(unix)]
fn process_is_running(pid: &str) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid)
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn macos_seatbelt_execution_backend_allows_workspace_write_and_denies_external_write()
-> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let workspace_root = fs::canonicalize(workspace.path())?;
    let outside_root = fs::canonicalize(outside.path())?;
    let backend = MacosSeatbeltExecutionBackend::default();

    let receipt = backend
        .execute(ExecutionRequest {
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf ok > allowed.txt; printf nope > \"$1/denied.txt\"".to_owned(),
                "sh".to_owned(),
                outside_root.to_string_lossy().into_owned(),
            ],
            cwd: workspace_root.clone(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 5,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.backend, ExecutionBackendKind::MacosSeatbelt);
    assert_eq!(receipt.network.policy, ExecutionNetworkPolicy::Unsupported);
    assert_eq!(receipt.exit_code, Some(1));
    assert_eq!(
        fs::read_to_string(workspace_root.join("allowed.txt"))?,
        "ok"
    );
    assert!(!outside_root.join("denied.txt").exists());
    assert!(
        String::from_utf8_lossy(&receipt.stderr).contains("Operation not permitted"),
        "stderr should explain the sandbox denial: {}",
        String::from_utf8_lossy(&receipt.stderr)
    );
    Ok(())
}

#[test]
fn macos_seatbelt_profile_escapes_workspace_path() {
    let profile = super::macos_seatbelt_workspace_write_profile(Path::new(
        r#"/tmp/sigil "quoted"\workspace"#,
    ));
    assert!(
        profile.contains(r#"(allow file-write* (subpath "/tmp/sigil \"quoted\"\\workspace"))"#)
    );
}

#[test]
fn sandbox_conformance_local_backend_does_not_claim_sandbox_capabilities() {
    let backend = LocalExecutionBackend;
    let capabilities = backend.capabilities();

    assert!(!capabilities.filesystem_isolation);
    assert!(!capabilities.network_isolation);
    assert!(!capabilities.process_isolation);
    assert!(!capabilities.resource_limits);
    assert!(!capabilities.persistent_pty);
    assert!(!capabilities.workspace_snapshot);
    assert!(!capabilities.supports_required_sandbox());
}

#[test]
fn sandbox_conformance_local_backend_fails_closed_for_required_sandbox() {
    let result = super::build_execution_backend(&sandbox_execution_config(
        ExecutionBackendKind::Local,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    ));

    let Err(error) = result else {
        panic!("local backend must not satisfy required sandbox policy");
    };
    assert!(
        error
            .to_string()
            .contains("execution profile WorkspaceWrite requires filesystem and process isolation")
    );
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn sandbox_conformance_macos_seatbelt_enforces_filesystem_write_claim() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let workspace_root = fs::canonicalize(workspace.path())?;
    let outside_root = fs::canonicalize(outside.path())?;
    let backend = MacosSeatbeltExecutionBackend::default();
    let capabilities = backend.capabilities();

    assert!(capabilities.filesystem_isolation);
    assert!(capabilities.process_isolation);
    assert!(capabilities.supports_required_sandbox());

    let receipt = backend
        .execute(ExecutionRequest {
            program: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "mkdir -p build && printf ok > build/artifact.txt; printf nope > \"$1/denied.txt\""
                    .to_owned(),
                "sh".to_owned(),
                outside_root.to_string_lossy().into_owned(),
            ],
            cwd: workspace_root.clone(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 5,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.backend, ExecutionBackendKind::MacosSeatbelt);
    assert_eq!(receipt.exit_code, Some(1));
    assert_eq!(
        fs::read_to_string(workspace_root.join("build").join("artifact.txt"))?,
        "ok"
    );
    assert!(!outside_root.join("denied.txt").exists());
    Ok(())
}

#[test]
fn sandbox_conformance_macos_seatbelt_does_not_claim_network_isolation() {
    let backend = MacosSeatbeltExecutionBackend::default();

    assert!(!backend.capabilities().network_isolation);
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn sandbox_conformance_macos_seatbelt_missing_binary_fails_closed() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let backend = MacosSeatbeltExecutionBackend::new(workspace.path().join("missing-sandbox-exec"));

    let error = backend
        .execute(ExecutionRequest {
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), "printf should-not-run".to_owned()],
            cwd: workspace.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 5,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await
        .expect_err("missing sandbox-exec should fail closed before command execution");

    assert!(
        error
            .to_string()
            .contains("macos_seatbelt execution backend requires")
    );
    Ok(())
}

#[test]
#[cfg(not(target_os = "macos"))]
fn sandbox_conformance_macos_seatbelt_is_skipped_with_reason_on_unsupported_platform() {
    eprintln!("skipping macos_seatbelt conformance: backend is macOS-only");
}

#[tokio::test]
async fn local_execution_backend_allows_explicit_no_timeout() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = LocalExecutionBackend;

    let receipt = backend
        .execute(ExecutionRequest {
            program: "printf".to_owned(),
            args: vec!["no-timeout".to_owned()],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 0,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;

    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(String::from_utf8_lossy(&receipt.stdout), "no-timeout");
    assert!(!receipt.timed_out);
    Ok(())
}

#[tokio::test]
async fn local_execution_backend_reports_timeout_and_spawn_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = LocalExecutionBackend;

    let timed_out = backend
        .execute(ExecutionRequest {
            program: "sh".to_owned(),
            args: vec!["-lc".to_owned(), "sleep 2".to_owned()],
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: Some(1),
            timeout_secs: 1,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await?;
    assert!(timed_out.timed_out);
    assert_eq!(timed_out.exit_code, None);
    assert!(timed_out.stdout.is_empty());
    assert!(timed_out.stderr.is_empty());

    let spawn_error = backend
        .execute(ExecutionRequest {
            program: "sigil-missing-local-backend-command".to_owned(),
            args: Vec::new(),
            cwd: temp.path().to_path_buf(),
            env: BTreeMap::new(),
            timeout_ms: None,
            timeout_secs: 1,
            cpu_time_ms: None,
            memory_limit_bytes: None,
            process_count_limit: None,
        })
        .await
        .expect_err("missing program should surface spawn error");
    assert!(!spawn_error.to_string().is_empty());
    Ok(())
}

#[test]
fn bash_execution_request_and_receipt_mapping_are_stable() -> Result<()> {
    let workspace = PathBuf::from("/workspace");
    let scratch = PathBuf::from("/scratch");
    let request = super::bash_execution_request("printf ok", &workspace, &scratch, 9);
    assert_eq!(request.program, "sh");
    assert_eq!(request.args, vec!["-c".to_owned(), "printf ok".to_owned()]);
    assert_eq!(request.cwd, workspace);
    assert_eq!(
        request
            .env
            .get(super::SIGIL_SCRATCH_DIR_ENV)
            .map(String::as_str),
        Some("/scratch")
    );
    assert_eq!(request.timeout_secs, 9);

    let timeout = super::bash_tool_result_from_execution_receipt(
        "call-timeout".to_owned(),
        "bash".to_owned(),
        ExecutionReceipt {
            backend: ExecutionBackendKind::Local,
            capabilities: ExecutionBackendCapabilities::default(),
            network: Default::default(),
            resources: Default::default(),
            exit_code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: true,
        },
    )?;
    let ToolResultStatus::Error(timeout_error) = timeout.status else {
        panic!("expected timeout error result");
    };
    assert_eq!(timeout_error.kind, ToolErrorKind::Timeout);

    let success = super::bash_tool_result_from_execution_receipt(
        "call-ok".to_owned(),
        "bash".to_owned(),
        ExecutionReceipt {
            backend: ExecutionBackendKind::Local,
            capabilities: ExecutionBackendCapabilities::default(),
            network: Default::default(),
            resources: Default::default(),
            exit_code: Some(0),
            stdout: b"stdout".to_vec(),
            stderr: b"stderr".to_vec(),
            timed_out: false,
        },
    )?;
    assert!(matches!(success.status, ToolResultStatus::Ok));
    assert_eq!(success.content, "stdout\nstderr");
    assert_eq!(success.metadata.exit_code, Some(0));
    assert_eq!(success.metadata.stdout_bytes, Some(6));
    assert_eq!(success.metadata.stderr_bytes, Some(6));

    let failed = super::bash_tool_result_from_execution_receipt(
        "call-failed".to_owned(),
        "bash".to_owned(),
        ExecutionReceipt {
            backend: ExecutionBackendKind::Local,
            capabilities: ExecutionBackendCapabilities::default(),
            network: Default::default(),
            resources: Default::default(),
            exit_code: Some(7),
            stdout: Vec::new(),
            stderr: b"bad".to_vec(),
            timed_out: false,
        },
    )?;
    let ToolResultStatus::Error(error) = &failed.status else {
        panic!("expected non-zero exit error result");
    };
    assert_eq!(error.kind, ToolErrorKind::ExitStatus);
    assert_eq!(failed.metadata.exit_code, Some(7));
    assert_eq!(failed.content, "bad");
    Ok(())
}

fn register_builtin_tools_with_test_paths(
    registry: &mut ToolRegistry,
    workspace_root: &Path,
    scratch_root: PathBuf,
) {
    register_builtin_tools_with_paths(
        registry,
        BuiltinToolPaths {
            changesets_root: workspace_root
                .join("state")
                .join("artifacts")
                .join("changesets"),
            changesets_label_root: PathBuf::from("state/artifacts/changesets"),
            terminal_tasks_root: workspace_root.join("state").join("artifacts").join("tasks"),
            terminal_tasks_label_root: PathBuf::from("state/artifacts/tasks"),
            scratch_root,
            scratch_label: "cache/tmp".to_owned(),
        },
    );
}

fn apply_changeset_tool() -> ApplyChangeSetTool {
    ApplyChangeSetTool {
        artifact_root: PathBuf::from("state/artifacts/changesets"),
        artifact_label_root: PathBuf::from("state/artifacts/changesets"),
    }
}

fn stored_event_types(store: &JsonlSessionStore) -> Result<Vec<String>> {
    let mut event_types = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let SessionStreamRecord::Stored(event) = record;
        event_types.push(event.event_type);
    }
    Ok(event_types)
}

#[test]
fn builtin_tool_paths_workspace_defaults_are_stable() {
    let root = Path::new("/workspace/project");
    let paths = BuiltinToolPaths::workspace_defaults(root);

    assert_eq!(
        paths.changesets_root,
        root.join("state/artifacts/changesets")
    );
    assert_eq!(
        paths.terminal_tasks_root,
        root.join("state/artifacts/tasks")
    );
    assert_eq!(paths.scratch_root, root.join("cache/tmp"));
    assert_eq!(paths.scratch_label, "cache/tmp");
}

#[test]
fn temporary_file_guidance_is_model_visible() {
    let scratch_root = PathBuf::from("/tmp/sigil-scratch-test");
    for spec in [
        WriteFileTool.spec(),
        BashTool {
            scratch_root: scratch_root.clone(),
            scratch_label: "cache/tmp".to_owned(),
            backend: Arc::new(LocalExecutionBackend),
        }
        .spec(),
        super::TerminalStartTool {
            managers: Default::default(),
            artifact_root: PathBuf::from("state/artifacts/tasks"),
            artifact_label_root: PathBuf::from("state/artifacts/tasks"),
            scratch_root,
            scratch_label: "cache/tmp".to_owned(),
        }
        .spec(),
    ] {
        assert!(spec.description.contains("$SIGIL_SCRATCH_DIR"));
        assert!(spec.description.contains("cache/tmp"));
        assert!(spec.description.contains("permission.external_directory"));
    }
}

#[test]
fn changeset_artifact_store_uses_injected_root_and_verifies_hashes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let temp_root = fs::canonicalize(temp.path())?;
    let workspace = temp_root.join("workspace");
    let artifact_root = temp_root.join("state").join("artifacts").join("changesets");
    fs::create_dir_all(&workspace)?;
    let store = ChangeSetArtifactStore::new_with_artifact_root(
        &workspace,
        &artifact_root,
        PathBuf::from("state/artifacts/changesets"),
    )?
    .with_summary_limit_bytes(8);

    let record = store.write_diff_artifacts(
        ChangeSetId::new("changeset_1")?,
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
        "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-new\n+old\n",
    )?;

    assert_eq!(
        record.artifact_dir,
        "state/artifacts/changesets/changeset_1"
    );
    assert!(record.summary.truncated);
    assert!(store.verify_diff_artifact(&record.preview)?);

    let mut mismatched = record.preview.clone();
    mismatched.sha256 = "sha256:bad".to_owned();
    assert!(!store.verify_diff_artifact(&mismatched)?);

    let mut absolute = record.preview.clone();
    absolute.path = artifact_root.join("preview.diff").display().to_string();
    assert!(store.verify_diff_artifact(&absolute).is_err());

    let mut unknown_label = record.preview.clone();
    unknown_label.path = "other/preview.diff".to_owned();
    assert!(store.verify_diff_artifact(&unknown_label).is_err());

    #[cfg(unix)]
    {
        let outside = tempfile::tempdir()?;
        symlink(outside.path(), artifact_root.join("leak"))?;
        let mut escaped = record.preview;
        escaped.path = "state/artifacts/changesets/leak/preview.diff".to_owned();
        let error = store
            .verify_diff_artifact(&escaped)
            .expect_err("symlink escape should be rejected");
        assert!(error.to_string().contains("outside artifact root"));
    }
    Ok(())
}

#[test]
fn terminal_process_managers_reuse_relative_artifact_roots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let managers = TerminalProcessManagers::default();
    let first = managers.manager_for(
        temp.path(),
        Path::new("state/artifacts/tasks"),
        Path::new("state/artifacts/tasks"),
    )?;
    let second = managers.manager_for(
        temp.path(),
        Path::new("state/artifacts/tasks"),
        Path::new("state/artifacts/tasks"),
    )?;

    assert!(Arc::ptr_eq(&first, &second));
    assert!(
        first
            .artifacts_for(&TerminalTaskId::new("terminal-relative-root")?)?
            .absolute_dir
            .starts_with(temp.path().canonicalize()?.join("state/artifacts/tasks"))
    );
    Ok(())
}

#[test]
fn write_file_permission_operation_classifies_create_overwrite_and_external() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("existing.txt"), "old")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    assert_eq!(
        WriteFileTool.permission_operation(&ctx, &json!({"path":"existing.txt"}))?,
        ToolOperation::OverwriteFile
    );
    assert_eq!(
        WriteFileTool.permission_operation(&ctx, &json!({"path":"new.txt"}))?,
        ToolOperation::CreateFile
    );
    assert_eq!(
        WriteFileTool
            .permission_operation(&ctx, &json!({"path": temp.path().join("abs-new.txt")}))?,
        ToolOperation::CreateFile
    );
    assert!(
        WriteFileTool
            .permission_operation(&ctx, &json!({"path":"../outside.txt"}))
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn read_and_edit_file_tool_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "hello old")?;
    let ctx = tool_context_with_mutation_recorder(temp.path(), 5)?;
    let read = ReadFileTool
        .execute(ctx.clone(), "1".to_owned(), json!({ "path": "note.txt" }))
        .await?;
    assert_eq!(read.content, "hello old");
    EditFileTool
        .execute(
            ctx.clone(),
            "2".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await?;
    assert_eq!(fs::read_to_string(file)?, "hello new");
    Ok(())
}

#[tokio::test]
async fn write_file_records_controlled_mutation_events_when_session_store_is_available()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5)
        .with_mutation_recorder(MutationEventRecorder::new(store.clone()));

    let result = WriteFileTool
        .execute(
            ctx,
            "write-call".to_owned(),
            json!({ "path": "note.txt", "content": "hello\n" }),
        )
        .await?;

    assert!(!result.is_error());
    assert_eq!(fs::read_to_string(temp.path().join("note.txt"))?, "hello\n");
    assert_eq!(
        stored_event_types(&store)?,
        vec![
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn edit_and_delete_file_record_controlled_mutation_events_when_session_store_is_available()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.txt"), "hello old\n")?;
    fs::write(temp.path().join("doomed.txt"), "delete me\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5)
        .with_mutation_recorder(MutationEventRecorder::new(store.clone()));

    let edit = EditFileTool
        .execute(
            ctx.clone(),
            "edit-call".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await?;
    let delete = DeleteFileTool
        .execute(
            ctx,
            "delete-call".to_owned(),
            json!({ "path": "doomed.txt" }),
        )
        .await?;

    assert!(!edit.is_error());
    assert!(!delete.is_error());
    assert_eq!(
        fs::read_to_string(temp.path().join("note.txt"))?,
        "hello new\n"
    );
    assert!(!temp.path().join("doomed.txt").exists());
    assert_eq!(
        stored_event_types(&store)?
            .into_iter()
            .filter(|event_type| event_type == DurableEventType::WriteCommitted.as_str())
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn write_file_preview_contains_diff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "alpha\nbeta\n")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let preview = WriteFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "content": "alpha\nbeta\ngamma\n" }),
        )
        .await?
        .expect("expected preview");
    assert!(preview.body.contains("--- current/note.txt"));
    assert!(preview.body.contains("+++ proposed/note.txt"));
    assert!(preview.body.contains("+gamma"));
    assert_eq!(preview.changed_files, vec!["note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "note.txt");
    assert!(preview.file_diffs[0].diff.contains("+gamma"));
    Ok(())
}

#[tokio::test]
async fn write_file_preview_for_new_file_contains_create_diff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let preview = WriteFileTool
        .preview(ctx, json!({ "path": "new-note.txt", "content": "hello\n" }))
        .await?
        .expect("expected preview");

    assert_eq!(preview.changed_files, vec!["new-note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "new-note.txt");
    assert!(
        preview.file_diffs[0]
            .diff
            .contains("--- current/new-note.txt")
    );
    assert!(
        preview.file_diffs[0]
            .diff
            .contains("+++ proposed/new-note.txt")
    );
    assert!(preview.file_diffs[0].diff.contains("+hello"));
    Ok(())
}

#[tokio::test]
async fn write_file_preview_errors_for_unreadable_existing_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, [0xff_u8, 0xfe, 0xfd])?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let error = WriteFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "content": "hello\nworld\n" }),
        )
        .await
        .expect_err("expected preview generation to surface the read failure");
    assert!(error.to_string().contains("failed to read"));
    Ok(())
}

#[tokio::test]
async fn edit_file_preview_contains_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "hello old\n")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let preview = EditFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await?
        .expect("expected preview");
    assert!(preview.body.contains("-hello old"));
    assert!(preview.body.contains("+hello new"));
    assert_eq!(preview.changed_files, vec!["note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "note.txt");
    assert!(preview.file_diffs[0].diff.contains("+hello new"));
    Ok(())
}

#[tokio::test]
async fn delete_file_preview_contains_delete_diff() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.txt"), "alpha\nbeta\n")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let preview = DeleteFileTool
        .preview(ctx, json!({ "path": "note.txt" }))
        .await?
        .expect("expected preview");

    assert_eq!(preview.title, "Delete note.txt");
    assert_eq!(preview.changed_files, vec!["note.txt"]);
    assert_eq!(preview.file_diffs.len(), 1);
    assert_eq!(preview.file_diffs[0].path, "note.txt");
    assert!(preview.file_diffs[0].diff.contains("--- current/note.txt"));
    assert!(preview.file_diffs[0].diff.contains("+++ proposed/note.txt"));
    assert!(preview.file_diffs[0].diff.contains("-alpha"));
    assert!(preview.file_diffs[0].diff.contains("-beta"));
    Ok(())
}

#[tokio::test]
async fn delete_file_execute_deletes_regular_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("note.txt");
    fs::write(&file, "alpha\nbeta\n")?;
    let ctx = tool_context_with_mutation_recorder(temp.path(), 5)?;

    let result = DeleteFileTool
        .execute(ctx, "delete".to_owned(), json!({ "path": "note.txt" }))
        .await?;

    assert!(!file.exists());
    assert_eq!(result.tool_name, "delete_file");
    assert_eq!(result.metadata.changed_files, vec!["note.txt"]);
    assert_eq!(result.metadata.bytes, Some("alpha\nbeta\n".len() as u64));
    assert_eq!(result.metadata.details["action"], "delete");
    let model_content = result.to_model_content();
    assert!(model_content.contains("deleted"));
    assert!(!model_content.contains("-alpha"));
    assert!(!model_content.contains("file_diffs"));
    Ok(())
}

#[tokio::test]
async fn delete_file_errors_for_missing_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let error = DeleteFileTool
        .execute(ctx, "delete".to_owned(), json!({ "path": "missing.txt" }))
        .await
        .expect_err("expected missing file to fail");

    assert!(error.to_string().contains("failed to inspect"));
    Ok(())
}

#[tokio::test]
async fn delete_file_errors_for_directory_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir(temp.path().join("dir"))?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let error = DeleteFileTool
        .execute(ctx, "delete".to_owned(), json!({ "path": "dir" }))
        .await
        .expect_err("expected directory delete to fail");

    assert!(
        error
            .to_string()
            .contains("delete_file only supports regular files")
    );
    Ok(())
}

#[test]
fn register_builtin_tools_registers_multiple_tools() {
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);
    assert!(registry.specs().len() >= 13);
    let spec = registry
        .spec_for("delete_file")
        .expect("delete_file should be registered");
    assert_eq!(spec.access, ToolAccess::Write);
    assert_eq!(spec.preview, ToolPreviewCapability::Required);
    let apply_spec = registry
        .spec_for("apply_changeset")
        .expect("apply_changeset should be registered");
    assert_eq!(apply_spec.access, ToolAccess::Write);
    assert_eq!(apply_spec.preview, ToolPreviewCapability::Required);
    assert_eq!(
        registry
            .spec_for("terminal_start")
            .expect("terminal_start should be registered")
            .access,
        ToolAccess::Execute
    );
    assert_eq!(
        registry
            .spec_for("terminal_read")
            .expect("terminal_read should be registered")
            .access,
        ToolAccess::Read
    );
    assert_eq!(
        registry
            .spec_for("terminal_input")
            .expect("terminal_input should be registered")
            .access,
        ToolAccess::Execute
    );
    assert_eq!(
        registry
            .spec_for("terminal_input")
            .expect("terminal_input should be registered")
            .input_schema["properties"]["input"]["maxLength"],
        super::MAX_TERMINAL_INPUT_BYTES
    );
    assert_eq!(
        registry
            .spec_for("terminal_resize")
            .expect("terminal_resize should be registered")
            .access,
        ToolAccess::Execute
    );
    assert_eq!(
        registry
            .spec_for("terminal_cancel")
            .expect("terminal_cancel should be registered")
            .access,
        ToolAccess::Execute
    );
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[test]
fn terminal_tools_permission_subjects_and_access_are_conservative() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir(temp.path().join("logs"))?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let start_call = tool_call(
        "terminal_start",
        json!({
            "command": "cat input.txt > out.txt",
            "cwd": "logs",
            "shell": "/bin/sh"
        }),
    );
    assert_eq!(
        registry.permission_access(&ctx, &start_call)?,
        ToolAccess::Execute
    );
    let start_subjects = registry.permission_subjects(&ctx, &start_call)?;
    assert!(start_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Command && subject.original == "cat input.txt > out.txt"
    }));
    assert!(start_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Command && subject.original == "/bin/sh"
    }));
    assert!(start_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path
            && subject.normalized == "logs"
            && subject.scope == ToolSubjectScope::Workspace
    }));
    assert!(start_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path
            && subject.normalized == "logs/input.txt"
            && subject.scope == ToolSubjectScope::Workspace
    }));
    assert!(start_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path
            && subject.normalized == "logs/out.txt"
            && subject.scope == ToolSubjectScope::Workspace
    }));

    let cargo_check_call = tool_call(
        "terminal_start",
        json!({ "command": "cd . && cargo check 2>&1 | tail -20" }),
    );
    assert_eq!(
        registry.permission_access(&ctx, &cargo_check_call)?,
        ToolAccess::Execute
    );
    assert_eq!(
        registry.permission_operation(&ctx, &cargo_check_call)?,
        ToolOperation::ExecuteWorkspaceCheckCommand
    );
    let cargo_check_subjects = registry.permission_subjects(&ctx, &cargo_check_call)?;
    assert!(cargo_check_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Command && subject.normalized == "family:cargo_check"
    }));

    let read_call = tool_call("terminal_read", json!({ "task_id": "terminal-perm" }));
    let input_call = tool_call(
        "terminal_input",
        json!({ "task_id": "terminal-perm", "input": "echo hello\n" }),
    );
    let resize_call = tool_call(
        "terminal_resize",
        json!({ "task_id": "terminal-perm", "rows": 30, "cols": 100 }),
    );
    let cancel_call = tool_call("terminal_cancel", json!({ "task_id": "terminal-perm" }));
    assert_eq!(
        registry.permission_access(&ctx, &read_call)?,
        ToolAccess::Read
    );
    assert_eq!(
        registry.permission_access(&ctx, &input_call)?,
        ToolAccess::Execute
    );
    assert_eq!(
        registry.permission_access(&ctx, &resize_call)?,
        ToolAccess::Execute
    );
    assert_eq!(
        registry.permission_access(&ctx, &cancel_call)?,
        ToolAccess::Execute
    );
    let missing_context = registry
        .permission_subjects(&ctx, &input_call)
        .expect_err("terminal_input without a live task context should fail closed");
    assert!(
        missing_context
            .to_string()
            .contains("permission context is unavailable")
    );
    assert!(
        registry
            .permission_subjects(&ctx, &resize_call)?
            .iter()
            .any(|subject| subject.kind == ToolSubjectKind::Command
                && subject.original == "terminal_task:terminal-perm")
    );
    Ok(())
}

#[test]
fn builtin_tools_expose_fine_grained_permission_operations() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("existing.txt"), "old")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    assert_eq!(
        registry.permission_operation(
            &ctx,
            &tool_call("write_file", json!({ "path": "new.txt", "content": "new" }))
        )?,
        ToolOperation::CreateFile
    );
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &tool_call(
                "write_file",
                json!({ "path": "existing.txt", "content": "new" })
            )
        )?,
        ToolOperation::OverwriteFile
    );
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &tool_call("delete_file", json!({ "path": "existing.txt" }))
        )?,
        ToolOperation::DeleteFile
    );
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &tool_call(
                "apply_changeset",
                json!({
                    "id": "change-1",
                    "files": [
                        {"path": "existing.txt", "action": "delete"}
                    ]
                })
            )
        )?,
        ToolOperation::ApplyChangeSet
    );
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &tool_call("bash", json!({ "command": "rm -rf .sigil" }))
        )?,
        ToolOperation::ExecuteDestructiveCommand
    );
    assert_eq!(
        registry.permission_operation(
            &ctx,
            &tool_call("terminal_start", json!({ "command": "git clean -fdx" }))
        )?,
        ToolOperation::ExecuteDestructiveCommand
    );
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_tools_start_read_cancel_share_manager_and_bound_results() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let start = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-tool-read",
                    "command": "printf 0123456789",
                    "mode": "background",
                    "shell": shell
                }),
            ),
        )
        .await?;
    assert!(matches!(start.status, ToolResultStatus::Ok));
    assert!(start.content.contains("terminal-tool-read"));
    assert_eq!(start.metadata.details["task_id"], "terminal-tool-read");

    let read = wait_for_terminal_read(&registry, ctx.clone(), "terminal-tool-read", 3).await?;
    assert!(matches!(read.status, ToolResultStatus::Ok));
    assert_eq!(read.metadata.returned_bytes, Some(3));
    assert_eq!(read.metadata.limit_bytes, Some(3));
    assert!(read.metadata.truncated);
    assert_eq!(read.metadata.details["next_offset"], 3);
    assert_eq!(read.content, "012");
    assert_eq!(read.metadata.details["content_returned"], true);

    let summarized_read = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_read",
                json!({ "task_id": "terminal-tool-read", "limit_bytes": 3 }),
            ),
        )
        .await?;
    assert!(matches!(summarized_read.status, ToolResultStatus::Ok));
    assert!(!summarized_read.content.contains("012"));
    assert!(summarized_read.content.contains("read omitted"));
    assert_eq!(summarized_read.metadata.returned_bytes, Some(3));
    assert_eq!(summarized_read.metadata.omitted_bytes, Some(3));
    assert_eq!(summarized_read.metadata.returned_lines, Some(0));
    assert_eq!(summarized_read.metadata.details["content_returned"], false);
    assert_eq!(summarized_read.metadata.details["content_omitted"], true);

    let shell = test_shell(temp.path())?;
    registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-tool-cancel",
                    "command": "sleep 5",
                    "mode": "background",
                    "shell": shell
                }),
            ),
        )
        .await?;
    let cancel = registry
        .execute(
            ctx,
            tool_call(
                "terminal_cancel",
                json!({ "task_id": "terminal-tool-cancel" }),
            ),
        )
        .await?;
    assert!(matches!(cancel.status, ToolResultStatus::Ok));
    assert_eq!(cancel.metadata.details["status"], "cancelled");
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_tool_reports_status_in_read_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-read-status",
                    "command": "printf 0123456789",
                    "shell": shell
                }),
            ),
        )
        .await?;

    let mut latest = None;
    for _ in 0..250 {
        let read = registry
            .execute(
                ctx.clone(),
                tool_call(
                    "terminal_read",
                    json!({ "task_id": "terminal-read-status", "limit_bytes": 10 }),
                ),
            )
            .await?;
        if read.metadata.details["terminal_task"]["status"] == "exited" {
            latest = Some(read);
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let read = latest.expect("terminal_read should eventually report terminal task status");

    assert!(read.content.contains("read omitted from model context"));
    assert!(!read.content.contains("0123456789"));
    assert_eq!(read.metadata.omitted_bytes, Some(10));
    assert_eq!(read.metadata.details["content_returned"], false);
    assert_eq!(read.metadata.details["content_omitted"], true);
    assert_eq!(
        read.metadata.details["terminal_task"]["task_id"],
        "terminal-read-status"
    );
    assert_eq!(read.metadata.details["terminal_task"]["status"], "exited");
    assert_eq!(
        read.metadata.details["terminal_task"]["status_detail"]["exit_code"],
        0
    );

    let raw_read = registry
        .execute(
            ctx,
            tool_call(
                "terminal_read",
                json!({
                    "task_id": "terminal-read-status",
                    "limit_bytes": 10,
                    "include_content": true
                }),
            ),
        )
        .await?;
    assert_eq!(raw_read.content, "0123456789");
    assert_eq!(raw_read.metadata.omitted_bytes, None);
    assert_eq!(raw_read.metadata.details["content_returned"], true);
    assert_eq!(raw_read.metadata.details["content_omitted"], false);
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_foreground_waits_and_returns_final_facts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let progress_events = Arc::new(Mutex::new(Vec::new()));
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5).with_progress_sink(Arc::new(
        RecordingProgressSink {
            events: Arc::clone(&progress_events),
        },
    ));
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let result = registry
        .execute(
            ctx,
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-foreground",
                    "command": "printf foreground-ok",
                    "shell": shell,
                    "mode": "foreground"
                }),
            ),
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.metadata.exit_code, Some(0));
    assert_eq!(result.metadata.details["task_id"], "terminal-foreground");
    assert_eq!(result.metadata.details["status"], "exited");
    assert_eq!(result.metadata.details["execution_mode"], "foreground");
    assert_eq!(result.metadata.details["verdict"], "passed");
    assert_eq!(result.metadata.details["rerun_not_needed"], true);
    assert_eq!(
        result.metadata.details["shell_analysis"]["verdict"],
        "passed"
    );
    assert!(
        result
            .metadata
            .details
            .get("output_preview")
            .is_some_and(|preview| preview.as_str() == Some("foreground-ok"))
    );
    assert!(!result.content.contains("foreground-ok"));
    let model_content: serde_json::Value = serde_json::from_str(&result.to_model_content())?;
    assert_eq!(
        model_content["meta"]["details"]["output_preview"]["omitted"],
        true
    );
    let progress_events = progress_events
        .lock()
        .expect("progress event lock should not be poisoned");
    assert!(!progress_events.is_empty());
    assert!(progress_events.iter().all(|event| {
        event.execution_id.as_str() == "terminal-foreground" && event.tool_name == "terminal_start"
    }));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_defaults_check_touched_to_foreground() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let scripts = temp.path().join("scripts");
    fs::create_dir_all(&scripts)?;
    let check_touched = scripts.join("check-touched.sh");
    fs::write(&check_touched, "#!/bin/sh\necho check-touched-ok\n")?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&check_touched)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&check_touched, permissions)?;
    }
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let result = registry
        .execute(
            ctx,
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-check-touched-default",
                    "command": "./scripts/check-touched.sh --tier quick 2>&1",
                    "shell": shell
                }),
            ),
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.metadata.exit_code, Some(0));
    assert_eq!(result.metadata.details["execution_mode"], "foreground");
    assert_eq!(result.metadata.details["verdict"], "passed");
    assert_eq!(result.metadata.details["rerun_not_needed"], true);
    assert_eq!(
        result.metadata.details["shell_analysis"]["command_family"],
        "check_touched"
    );
    assert!(!result.content.contains("check-touched-ok"));
    Ok(())
}

#[test]
fn terminal_start_defaults_unknown_one_shot_to_foreground() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let one_shot = super::analyze_shell_command(workspace.path(), "printf unknown-ok")?;
    let dev_server = super::analyze_shell_command(workspace.path(), "npm run dev")?;
    let tail_follow = super::analyze_shell_command(workspace.path(), "tail -f logs/app.log")?;

    assert_eq!(
        super::resolve_terminal_start_execution_mode(None, false, &one_shot)?,
        super::TerminalStartExecutionMode::Foreground
    );
    assert_eq!(
        super::resolve_terminal_start_execution_mode(None, false, &dev_server)?,
        super::TerminalStartExecutionMode::Background
    );
    assert_eq!(
        super::resolve_terminal_start_execution_mode(None, false, &tail_follow)?,
        super::TerminalStartExecutionMode::Background
    );
    assert_eq!(
        super::resolve_terminal_start_execution_mode(
            Some(super::TerminalStartExecutionMode::Background),
            false,
            &one_shot
        )?,
        super::TerminalStartExecutionMode::Background
    );
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_unknown_one_shot_without_mode_returns_final_facts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let result = registry
        .execute(
            ctx,
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-unknown-one-shot-default",
                    "command": "printf unknown-one-shot-ok",
                    "shell": shell
                }),
            ),
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.metadata.exit_code, Some(0));
    assert_eq!(result.metadata.details["execution_mode"], "foreground");
    assert_eq!(result.metadata.details["verdict"], "passed");
    assert_eq!(result.metadata.details["rerun_not_needed"], true);
    assert_eq!(result.metadata.details["status"], "exited");
    assert!(!result.content.contains("unknown-one-shot-ok"));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_foreground_uses_long_task_timeout_contract() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 1);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let result = registry
        .execute(
            ctx,
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-foreground-long-contract",
                    "command": "sleep 2; printf foreground-late-ok",
                    "shell": shell,
                    "mode": "foreground"
                }),
            ),
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.metadata.exit_code, Some(0));
    assert_eq!(result.metadata.details["verdict"], "passed");
    assert_eq!(result.metadata.details["rerun_not_needed"], true);
    assert_eq!(result.metadata.details["foreground_timeout_secs"], 1800);
    assert_eq!(
        result.metadata.details["foreground_inactivity_timeout_secs"],
        300
    );
    assert!(
        result
            .metadata
            .details
            .get("output_preview")
            .is_some_and(|preview| preview.as_str() == Some("foreground-late-ok"))
    );
    assert!(!result.content.contains("foreground-late-ok"));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_foreground_explicit_total_timeout_cancels_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 30);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let result = registry
        .execute(
            ctx,
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-foreground-total-timeout",
                    "command": "sleep 5; printf never",
                    "shell": shell,
                    "mode": "foreground",
                    "foreground_timeout_secs": 1,
                    "foreground_inactivity_timeout_secs": 10
                }),
            ),
        )
        .await?;

    let ToolResultStatus::Error(error) = &result.status else {
        panic!("expected foreground timeout to surface as an error result");
    };
    assert_eq!(error.kind, ToolErrorKind::Timeout);
    assert_eq!(result.metadata.details["verdict"], "timed_out");
    assert_eq!(result.metadata.details["timeout_kind"], "total");
    assert_eq!(result.metadata.details["foreground_timeout_secs"], 1);
    assert_eq!(result.metadata.details["rerun_not_needed"], false);
    assert!(result.content.contains("timeout_kind: total"));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_foreground_explicit_inactivity_timeout_cancels_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 30);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let result = registry
        .execute(
            ctx,
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-foreground-inactivity-timeout",
                    "command": "sleep 5; printf never",
                    "shell": shell,
                    "mode": "foreground",
                    "foreground_timeout_secs": 10,
                    "foreground_inactivity_timeout_secs": 1
                }),
            ),
        )
        .await?;

    let ToolResultStatus::Error(error) = &result.status else {
        panic!("expected foreground inactivity timeout to surface as an error result");
    };
    assert_eq!(error.kind, ToolErrorKind::Timeout);
    assert_eq!(result.metadata.details["verdict"], "inactive_timeout");
    assert_eq!(result.metadata.details["timeout_kind"], "inactivity");
    assert_eq!(
        result.metadata.details["shell_analysis"]["timeout_kind"],
        "inactivity"
    );
    assert_eq!(
        result.metadata.details["foreground_inactivity_timeout_secs"],
        1
    );
    assert!(result.content.contains("timeout_kind: inactivity"));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_start_injects_scratch_dir_env() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let scratch_root = temp.path().join("cache").join("tmp");
    let shell = test_shell(&workspace)?;
    let ctx = ToolContext::new(workspace.clone(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools_with_test_paths(&mut registry, &workspace, scratch_root.clone());

    let start = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-scratch-env",
                    "command": "test -d \"$SIGIL_SCRATCH_DIR\" && printf terminal-ok > \"$SIGIL_SCRATCH_DIR/probe\" && printf done",
                    "shell": shell
                }),
            ),
        )
        .await?;
    assert!(matches!(start.status, ToolResultStatus::Ok));

    let read = wait_for_terminal_read(&registry, ctx, "terminal-scratch-env", 64).await?;
    assert!(matches!(read.status, ToolResultStatus::Ok));
    assert_eq!(read.content, "done");
    assert_eq!(
        fs::read_to_string(scratch_root.join("probe"))?,
        "terminal-ok"
    );
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_input_returns_structured_unsupported_without_echoing_input() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-input",
                    "command": "sleep 5",
                    "mode": "background",
                    "shell": shell
                }),
            ),
        )
        .await?;

    let destructive_input = tool_call(
        "terminal_input",
        json!({
            "task_id": "terminal-input",
            "input": "rm -rf .sigil\n"
        }),
    );
    assert_eq!(
        registry.permission_operation(&ctx, &destructive_input)?,
        ToolOperation::ExecuteDestructiveCommand
    );
    let destructive_subjects = registry.permission_subjects(&ctx, &destructive_input)?;
    assert!(destructive_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path && subject.normalized == ".sigil"
    }));

    let result = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_input",
                json!({
                    "task_id": "terminal-input",
                    "input": "secret-token-should-not-appear\n"
                }),
            ),
        )
        .await?;

    let ToolResultStatus::Error(error) = &result.status else {
        panic!("terminal_input should return unsupported error");
    };
    assert_eq!(error.kind, ToolErrorKind::Unsupported);
    assert!(!result.content.contains("secret-token"));
    assert_eq!(result.metadata.details["supported"], false);
    assert_eq!(result.metadata.details["input_bytes"], 31);
    let resize = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_resize",
                json!({ "task_id": "terminal-input", "rows": 24, "cols": 80 }),
            ),
        )
        .await?;
    let ToolResultStatus::Error(error) = &resize.status else {
        panic!("terminal_resize should return unsupported error");
    };
    assert_eq!(error.kind, ToolErrorKind::Unsupported);
    assert_eq!(resize.metadata.details["supported"], false);
    assert_eq!(resize.metadata.details["backend"], "process");
    let oversize = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_input",
                json!({
                    "task_id": "terminal-input",
                    "input": "x".repeat(super::MAX_TERMINAL_INPUT_BYTES + 1)
                }),
            ),
        )
        .await?;
    let ToolResultStatus::Error(error) = &oversize.status else {
        panic!("oversized terminal_input should return invalid input");
    };
    assert_eq!(error.kind, ToolErrorKind::InvalidInput);
    assert!(!oversize.to_model_content().contains("secret-token"));
    assert_eq!(
        oversize.metadata.limit_bytes,
        Some(super::MAX_TERMINAL_INPUT_BYTES as u64)
    );
    registry
        .execute(
            ctx,
            tool_call("terminal_cancel", json!({ "task_id": "terminal-input" })),
        )
        .await?;
    Ok(())
}

#[serial]
#[tokio::test]
async fn terminal_input_permission_hooks_use_live_process_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::create_dir(workspace.join("logs"))?;
    let shell = test_shell(&workspace)?;
    let ctx = ToolContext::new(workspace.clone(), 5);
    let managers = Arc::new(TerminalProcessManagers::default());
    let manager = managers.manager_for(
        &workspace,
        Path::new("state/artifacts/tasks"),
        Path::new("state/artifacts/tasks"),
    )?;
    let task_id = TerminalTaskId::new("terminal-input-permission")?;
    manager
        .start(TerminalStartRequest {
            task_id: Some(task_id.clone()),
            command: "sleep 5".to_owned(),
            cwd: Some(PathBuf::from("logs")),
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;
    let tool = TerminalInputTool {
        managers,
        artifact_root: PathBuf::from("state/artifacts/tasks"),
        artifact_label_root: PathBuf::from("state/artifacts/tasks"),
    };

    let input_args = json!({
        "task_id": task_id.as_str(),
        "input": "cat input.txt > out.txt\n"
    });
    assert_eq!(
        tool.permission_operation(&ctx, &input_args)?,
        ToolOperation::ExecuteDestructiveCommand
    );
    let subjects = tool.permission_subjects(&ctx, &input_args)?;
    assert!(subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Command && subject.original == "terminal_input bytes=24"
    }));
    assert!(subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path && subject.normalized == "logs/input.txt"
    }));
    assert!(subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path && subject.normalized == "logs/out.txt"
    }));

    assert_eq!(
        tool.permission_operation(
            &ctx,
            &json!({ "task_id": task_id.as_str(), "input": "echo hello\n" }),
        )?,
        ToolOperation::SendTerminalInput
    );
    manager.cancel(&task_id).await?;
    Ok(())
}

#[cfg(unix)]
#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_pty_tools_accept_input_resize_and_read_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let mut registry = ToolRegistry::new();
    register_builtin_tools(&mut registry);

    let start = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "terminal-pty-tool",
                    "command": "trap '' WINCH; IFS= read -r line; printf 'got:%s\\n' \"$line\"",
                    "shell": shell,
                    "pty": true,
                    "rows": 12,
                    "cols": 50
                }),
            ),
        )
        .await?;
    assert!(matches!(start.status, ToolResultStatus::Ok));

    let resize = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_resize",
                json!({ "task_id": "terminal-pty-tool", "rows": 18, "cols": 70 }),
            ),
        )
        .await?;
    assert!(matches!(resize.status, ToolResultStatus::Ok));
    assert_eq!(resize.metadata.details["backend"], "pty");

    let input = registry
        .execute(
            ctx.clone(),
            tool_call(
                "terminal_input",
                json!({ "task_id": "terminal-pty-tool", "input": "hello-from-pty\n" }),
            ),
        )
        .await?;
    assert!(matches!(input.status, ToolResultStatus::Ok));
    assert!(!input.content.contains("hello-from-pty"));
    assert_eq!(input.metadata.details["backend"], "pty");
    assert_eq!(input.metadata.details["input_bytes"], 15);

    let read =
        wait_for_terminal_read_contains(&registry, ctx, "terminal-pty-tool", "got:hello-from-pty")
            .await?;
    assert!(read.content.contains("got:hello-from-pty"));
    Ok(())
}

#[tokio::test]
async fn read_file_supports_offset_limit_and_truncation_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("big.txt"), "one\ntwo\nthree\nfour\n")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let result = ReadFileTool
        .execute(
            ctx,
            "read".to_owned(),
            json!({ "path": "big.txt", "offset": 1, "limit": 2 }),
        )
        .await?;

    assert!(result.content.starts_with("two\nthree"));
    assert!(result.content.contains("output truncated"));
    assert!(result.metadata.truncated);
    assert_eq!(result.metadata.returned_lines, Some(2));
    assert_eq!(result.metadata.total_lines, Some(4));
    assert_eq!(result.metadata.details["path"], "big.txt");
    assert_eq!(result.metadata.details["offset"], 1);
    assert_eq!(result.metadata.details["next_offset"], 3);
    Ok(())
}

#[tokio::test]
async fn read_file_reports_code_preview_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("lib.rs"), "fn main() {}\n")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let result = ReadFileTool
        .execute(ctx, "read".to_owned(), json!({ "path": "lib.rs" }))
        .await?;

    assert_eq!(result.metadata.details["path"], "lib.rs");
    assert_eq!(result.metadata.details["language"], "rust");
    Ok(())
}

#[tokio::test]
async fn list_glob_and_grep_report_limit_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for index in 0..5 {
        fs::write(temp.path().join(format!("file-{index}.txt")), "needle\n")?;
    }
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let list = ListTool
        .execute(ctx.clone(), "ls".to_owned(), json!({ "limit": 2 }))
        .await?;
    let glob = GlobTool
        .execute(
            ctx.clone(),
            "glob".to_owned(),
            json!({ "pattern": "*.txt", "limit": 2 }),
        )
        .await?;
    let grep = GrepTool
        .execute(
            ctx,
            "grep".to_owned(),
            json!({ "pattern": "needle", "limit": 2 }),
        )
        .await?;

    assert!(list.metadata.truncated);
    assert_eq!(list.metadata.returned_entries, Some(2));
    assert_eq!(list.metadata.total_entries, Some(5));
    assert!(glob.metadata.truncated);
    assert_eq!(glob.metadata.details["returned_paths"], 2);
    assert_eq!(glob.metadata.details["total_paths"], 5);
    assert!(grep.metadata.truncated);
    assert_eq!(grep.metadata.returned_matches, Some(2));
    assert_eq!(grep.metadata.total_matches, Some(5));
    Ok(())
}

#[tokio::test]
async fn bash_large_output_is_truncated_with_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let result = bash_tool(temp.path())
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "yes x | head -n 70000" }),
        )
        .await?;

    assert!(result.metadata.truncated);
    assert!(result.content.contains("output truncated"));
    assert!(result.metadata.stdout_bytes.unwrap_or_default() > 64 * 1024);
    Ok(())
}

#[tokio::test]
async fn bash_tool_injects_scratch_dir_env() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let tool = BashTool {
        scratch_root: temp.path().join("cache").join("tmp"),
        scratch_label: "cache/tmp".to_owned(),
        backend: Arc::new(LocalExecutionBackend),
    };
    let ctx = ToolContext::new(workspace, 5);

    let result = tool
        .execute(
            ctx,
            "bash".to_owned(),
            json!({
                "command": "test -d \"$SIGIL_SCRATCH_DIR\" && printf bash-ok > \"$SIGIL_SCRATCH_DIR/probe\" && printf ok"
            }),
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.content, "ok");
    assert_eq!(
        result.metadata.details["execution"]["network"]["policy"],
        json!("unknown")
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("cache/tmp/probe"))?,
        "bash-ok"
    );
    Ok(())
}

#[tokio::test]
async fn bash_and_terminal_start_report_scratch_dir_creation_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let scratch_file = temp.path().join("scratch-file");
    fs::write(&scratch_file, "not a directory")?;
    let ctx = ToolContext::new(workspace, 5);

    let bash_error = BashTool {
        scratch_root: scratch_file.clone(),
        scratch_label: "scratch-file".to_owned(),
        backend: Arc::new(LocalExecutionBackend),
    }
    .execute(ctx.clone(), "bash".to_owned(), json!({ "command": "true" }))
    .await
    .expect_err("bash scratch file should fail create_dir_all");
    assert!(
        bash_error
            .to_string()
            .contains("failed to create scratch-file")
    );

    let terminal_error = TerminalStartTool {
        managers: Arc::new(TerminalProcessManagers::default()),
        artifact_root: PathBuf::from("state/artifacts/tasks"),
        artifact_label_root: PathBuf::from("state/artifacts/tasks"),
        scratch_root: scratch_file,
        scratch_label: "scratch-file".to_owned(),
    }
    .execute(
        ctx,
        "terminal-start".to_owned(),
        json!({ "command": "printf never" }),
    )
    .await
    .expect_err("terminal_start scratch file should fail create_dir_all");
    assert!(
        terminal_error
            .to_string()
            .contains("failed to create scratch-file")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn read_file_reports_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects = ReadFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_file_reports_existing_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects = WriteFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[cfg(unix)]
#[test]
fn write_file_reports_symlink_parent_escape_for_new_file_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let expected = outside.path().canonicalize()?.join("new.txt");
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects =
        WriteFileTool.permission_subjects(&ctx, &json!({ "path": "outside-dir/new.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert!(!outside.path().join("new.txt").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn edit_file_reports_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "hello old")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects = EditFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(fs::read_to_string(outside_file)?, "hello old");
    Ok(())
}

#[cfg(unix)]
#[test]
fn delete_file_reports_symlink_escape_as_external_subject() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("leak.txt"))?;
    let expected = fs::canonicalize(&outside_file)?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects = DeleteFileTool.permission_subjects(&ctx, &json!({ "path": "leak.txt" }))?;

    assert_eq!(subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[cfg(unix)]
#[test]
fn list_and_grep_report_external_symlink_roots_as_external_subjects() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let expected = outside.path().canonicalize()?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let list_subjects = ListTool.permission_subjects(&ctx, &json!({ "path": "outside-dir" }))?;
    let grep_subjects = GrepTool
        .permission_subjects(&ctx, &json!({ "path": "outside-dir", "pattern": "secret" }))?;

    assert_eq!(list_subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(grep_subjects[0].scope, ToolSubjectScope::External);
    assert_eq!(
        list_subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    assert_eq!(
        grep_subjects[0].canonical_path.as_deref(),
        Some(expected.as_path())
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn list_recursive_does_not_traverse_external_symlink_children() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    fs::write(workspace.path().join("visible.txt"), "visible")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let result = ListTool
        .execute(
            ctx,
            "list".to_owned(),
            json!({ "path": ".", "recursive": true }),
        )
        .await?;

    assert!(result.content.contains("visible.txt"));
    assert!(!result.content.contains("secret.txt"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn glob_does_not_traverse_external_symlink_targets() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path(), workspace.path().join("outside-dir"))?;
    fs::write(workspace.path().join("visible.txt"), "visible")?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let result = GlobTool
        .execute(ctx, "glob".to_owned(), json!({ "pattern": "**/*.txt" }))
        .await?;

    assert!(result.content.contains("visible.txt"));
    assert!(!result.content.contains("secret.txt"));
    Ok(())
}

#[tokio::test]
async fn bash_tool_timeout_surfaces_structured_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let result = bash_tool(temp.path())
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "sleep 2", "timeout_secs": 1 }),
        )
        .await?;

    let ToolResultStatus::Error(error) = result.status else {
        panic!("expected timeout to be surfaced as an error result");
    };
    assert_eq!(error.kind, ToolErrorKind::Timeout);
    assert!(error.message.contains("bash command timed out"));
    Ok(())
}

#[tokio::test]
async fn bash_tool_non_zero_exit_returns_error_result() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let result = bash_tool(temp.path())
        .execute(
            ctx,
            "bash".to_owned(),
            json!({ "command": "printf 'bad output' >&2; exit 7" }),
        )
        .await?;

    assert!(result.is_error());
    assert_eq!(result.metadata.exit_code, Some(7));
    assert!(result.content.contains("bad output"));
    Ok(())
}

#[test]
fn bash_permission_access_allows_only_simple_readonly_commands() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    for command in [
        "pwd",
        "ls src",
        "rg needle crates",
        "git status --short",
        "pwd && git status --short",
        "find . -name lib.rs",
        "command -v cargo",
        "rustc --version",
        "pwd | wc -l",
        "ls *.rs",
    ] {
        assert_eq!(
            bash_tool(temp.path()).permission_access(&ctx, &json!({ "command": command }))?,
            ToolAccess::Read,
            "{command} should be read-only"
        );
    }

    for command in [
        "echo hi > out.txt",
        "echo $HOME",
        "(pwd)",
        "find . -exec echo {} \\;",
        "find . -delete",
        "git push",
        "python script.py",
        "cargo test",
    ] {
        assert_eq!(
            bash_tool(temp.path()).permission_access(&ctx, &json!({ "command": command }))?,
            ToolAccess::Execute,
            "{command} should require execute approval"
        );
    }

    Ok(())
}

#[tokio::test]
async fn bash_permission_subjects_include_external_paths_and_redirections() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().canonicalize()?.join("input.txt");
    fs::write(&outside_file, "needle")?;
    let outside_output = outside.path().canonicalize()?.join("out.txt");
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects = bash_tool(workspace.path()).permission_subjects(
        &ctx,
        &json!({ "command": format!("cat {} > {}", outside_file.display(), outside_output.display()) }),
    )?;

    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_file.as_path())
    }));
    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_output.as_path())
    }));

    let fd_redirect_subjects = bash_tool(workspace.path())
        .permission_subjects(&ctx, &json!({ "command": "cargo check 2>&1" }))?;
    assert!(
        fd_redirect_subjects
            .iter()
            .filter(|subject| subject.kind == ToolSubjectKind::Path)
            .all(|subject| !subject.normalized.contains("&1"))
    );

    Ok(())
}

#[tokio::test]
async fn bash_shell_analysis_groups_workspace_checks_for_session_grants() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
    let tool = bash_tool(workspace.path());

    let first = tool.permission_subjects(&ctx, &json!({ "command": "cargo check 2>&1" }))?;
    let piped = tool.permission_subjects(
        &ctx,
        &json!({ "command": "cd . && cargo check 2>&1 | tail -20" }),
    )?;

    assert_eq!(first.len(), 1);
    assert_eq!(piped.len(), 1);
    assert_eq!(first[0].normalized, "family:cargo_check");
    assert_eq!(piped[0].normalized, "family:cargo_check");
    assert_eq!(
        tool.permission_access(&ctx, &json!({ "command": "cargo check 2>&1 | tail -20" }))?,
        ToolAccess::Execute
    );
    assert_eq!(
        tool.permission_operation(&ctx, &json!({ "command": "cargo check 2>&1 | tail -20" }))?,
        ToolOperation::ExecuteWorkspaceCheckCommand
    );
    assert!(
        sigil_kernel::tool_approval_session_grant_available_for_parts(
            ToolAccess::Execute,
            ToolOperation::ExecuteWorkspaceCheckCommand,
            PermissionRisk::Medium,
            &first,
            &[PathTrustZone::Unknown],
            None,
            false,
        )
    );
    Ok(())
}

#[tokio::test]
async fn bash_shell_analysis_allows_safe_search_and_devices_without_external_approval() -> Result<()>
{
    let workspace = tempfile::tempdir()?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
    let tool = bash_tool(workspace.path());

    assert_eq!(
        tool.permission_access(
            &ctx,
            &json!({ "command": "grep -r 'XYZ' --include='*.rs' --include='*.md' ." })
        )?,
        ToolAccess::Read
    );
    let subjects =
        tool.permission_subjects(&ctx, &json!({ "command": "cargo check >/dev/null 2>&1" }))?;
    assert!(
        subjects
            .iter()
            .all(|subject| subject.scope != ToolSubjectScope::External),
        "{subjects:?}"
    );
    assert_eq!(
        tool.permission_operation(&ctx, &json!({ "command": "cargo check > /dev/null 2>&1" }))?,
        ToolOperation::ExecuteWorkspaceCheckCommand
    );
    Ok(())
}

#[tokio::test]
async fn bash_tool_result_exposes_workspace_check_facts() -> Result<()> {
    let receipt = ExecutionReceipt {
        exit_code: Some(0),
        stdout: b"ok\n".to_vec(),
        stderr: Vec::new(),
        timed_out: false,
        backend: ExecutionBackendKind::Local,
        capabilities: ExecutionBackendCapabilities::default(),
        network: Default::default(),
        resources: Default::default(),
    };
    let workspace = tempfile::tempdir()?;
    let analysis = super::analyze_shell_command(
        workspace.path(),
        "./scripts/check-touched.sh --tier quick 2>&1",
    )?;
    let result = super::bash_tool_result_from_execution_receipt_with_analysis(
        "call".to_owned(),
        "bash".to_owned(),
        receipt,
        &analysis,
    )?;

    assert_eq!(result.metadata.exit_code, Some(0));
    assert_eq!(
        result.metadata.details["shell"]["command_family"],
        "check_touched"
    );
    assert_eq!(
        result.metadata.details["shell"]["command"],
        "./scripts/check-touched.sh --tier quick 2>&1"
    );
    assert_eq!(
        result.metadata.details["shell"]["grant_scope"],
        "workspace_script"
    );
    assert_eq!(
        result.metadata.details["shell"]["grant_scope_detail"]["path"],
        "scripts/check-touched.sh"
    );
    assert_eq!(
        result.metadata.details["shell"]["grant_scope_detail"]["args_family"],
        "quick"
    );
    assert_eq!(result.metadata.details["shell"]["exit_code"], 0);
    assert_eq!(result.metadata.details["shell"]["verdict"], "passed");
    assert_eq!(result.metadata.details["shell"]["rerun_not_needed"], true);
    Ok(())
}

#[tokio::test]
async fn bash_shell_analysis_treats_missing_relative_paths_as_workspace_subjects() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
    let tool = bash_tool(workspace.path());

    let subjects =
        tool.permission_subjects(&ctx, &json!({ "command": "ls missing_workspace_dir" }))?;

    assert!(subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path
            && subject.scope == ToolSubjectScope::Workspace
            && subject.normalized.ends_with("missing_workspace_dir")
    }));
    assert!(
        subjects
            .iter()
            .all(|subject| subject.scope != ToolSubjectScope::External),
        "{subjects:?}"
    );
    Ok(())
}

#[tokio::test]
async fn bash_permission_subjects_resolve_cd_relative_paths_against_external_cwd() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_root = outside.path().canonicalize()?;
    let outside_child = outside_root.join("child.txt");
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let subjects = bash_tool(workspace.path()).permission_subjects(
        &ctx,
        &json!({ "command": format!("cd {} && ls child.txt", outside_root.display()) }),
    )?;

    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_root.as_path())
    }));
    assert!(subjects.iter().any(|subject| {
        subject.scope == ToolSubjectScope::External
            && subject.canonical_path.as_deref() == Some(outside_child.as_path())
    }));
    Ok(())
}

#[tokio::test]
async fn grep_skips_non_utf8_files_without_panicking() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("valid.txt"), "needle\n")?;
    fs::write(temp.path().join("binary.bin"), [0xff_u8, 0xfe, 0xfd])?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let result = GrepTool
        .execute(ctx, "grep".to_owned(), json!({ "pattern": "needle" }))
        .await?;

    assert!(!result.is_error());
    assert!(result.content.contains("valid.txt"));
    assert!(!result.content.contains("binary.bin"));
    assert_eq!(result.metadata.details["binary_files_skipped"], 1);
    Ok(())
}

#[tokio::test]
async fn write_file_execute_creates_missing_parent_directories() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = tool_context_with_mutation_recorder(temp.path(), 5)?;

    let result = WriteFileTool
        .execute(
            ctx,
            "write".to_owned(),
            json!({ "path": "nested/deep/note.txt", "content": "hello" }),
        )
        .await?;

    assert_eq!(
        fs::read_to_string(temp.path().join("nested/deep/note.txt"))?,
        "hello"
    );
    assert_eq!(result.metadata.changed_files, vec!["nested/deep/note.txt"]);
    Ok(())
}

#[tokio::test]
async fn edit_file_errors_for_missing_and_ambiguous_old_text() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    fs::write(temp.path().join("note.txt"), "repeat old repeat old")?;

    let missing = EditFileTool
        .execute(
            ctx.clone(),
            "edit-missing".to_owned(),
            json!({ "path": "note.txt", "old_text": "absent", "new_text": "new" }),
        )
        .await
        .expect_err("missing old_text should fail");
    assert!(missing.to_string().contains("old_text not found"));

    let ambiguous = EditFileTool
        .execute(
            ctx,
            "edit-ambiguous".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("ambiguous old_text should fail");
    assert!(ambiguous.to_string().contains("old_text is ambiguous"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn delete_file_rejects_symlink_target() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, "secret")?;
    symlink(&outside_file, workspace.path().join("linked.txt"))?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let error = DeleteFileTool
        .execute(
            ctx,
            "delete-link".to_owned(),
            json!({ "path": "linked.txt" }),
        )
        .await
        .expect_err("symlink deletes should fail");

    assert!(error.to_string().contains("outside workspace"));
    assert_eq!(fs::read_to_string(outside_file)?, "secret");
    Ok(())
}

#[test]
fn builtin_path_and_truncation_helpers_preserve_boundaries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let subject = super::tool_path_subject(temp.path(), ".")?;
    assert_eq!(subject.scope, ToolSubjectScope::Workspace);
    assert_eq!(subject.normalized, ".");

    let repeated = "é".repeat(80);
    let truncated = super::limit_text_head_tail(&repeated, 32);
    assert!(truncated.truncated);
    assert!(truncated.content.contains("output truncated"));
    assert!(std::str::from_utf8(truncated.content.as_bytes()).is_ok());
    Ok(())
}

#[test]
fn builtin_argument_helpers_validate_types_and_sizes() {
    let missing = super::required_string(&json!({}), "path").expect_err("path should be required");
    assert!(missing.to_string().contains("missing string field path"));

    let wrong_type =
        super::required_string(&json!({ "path": 7 }), "path").expect_err("path should be string");
    assert!(wrong_type.to_string().contains("missing string field path"));

    let invalid_limit = super::optional_usize(&json!({ "limit": "many" }), "limit")
        .expect_err("limit should be numeric");
    assert!(
        invalid_limit
            .to_string()
            .contains("limit must be a positive integer")
    );
    assert_eq!(
        super::optional_string(&json!({ "path": "src" }), "path"),
        Some("src")
    );
    assert_eq!(
        super::optional_usize(&json!({ "limit": 3 }), "limit").expect("limit"),
        Some(3)
    );
}

#[tokio::test]
async fn tool_permission_subjects_validate_required_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    for (tool_name, result) in [
        (
            "read_file",
            ReadFileTool.permission_subjects(&ctx, &json!({})),
        ),
        (
            "write_file",
            WriteFileTool.permission_subjects(&ctx, &json!({ "content": "hello" })),
        ),
        (
            "edit_file",
            EditFileTool.permission_subjects(&ctx, &json!({ "old_text": "a", "new_text": "b" })),
        ),
        (
            "delete_file",
            DeleteFileTool.permission_subjects(&ctx, &json!({})),
        ),
    ] {
        let error = result.expect_err(tool_name);
        assert!(
            error.to_string().contains("missing string field path"),
            "{tool_name} should require a path"
        );
    }

    let empty_apply = apply_changeset_tool()
        .permission_subjects(&ctx, &json!({ "id": "change-empty", "files": [] }))
        .expect_err("apply_changeset should require at least one file");
    assert!(
        empty_apply
            .to_string()
            .contains("apply_changeset requires at least one file")
    );

    Ok(())
}

#[tokio::test]
async fn edit_file_preview_surfaces_missing_and_ambiguous_matches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("note.txt"), "old one old two")?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let missing = EditFileTool
        .preview(
            ctx.clone(),
            json!({ "path": "note.txt", "old_text": "absent", "new_text": "new" }),
        )
        .await
        .expect_err("missing old_text should fail preview");
    assert!(missing.to_string().contains("old_text not found"));

    let ambiguous = EditFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("ambiguous old_text should fail preview");
    assert!(ambiguous.to_string().contains("old_text is ambiguous"));
    Ok(())
}

#[tokio::test]
async fn read_list_glob_grep_and_bash_surface_input_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);

    let read_error = ReadFileTool
        .execute(
            ctx.clone(),
            "read".to_owned(),
            json!({ "path": "missing.txt", "limit": "lots" }),
        )
        .await
        .expect_err("invalid read limit should fail");
    assert!(
        read_error
            .to_string()
            .contains("limit must be a positive integer")
    );

    let list_error = ListTool
        .execute(
            ctx.clone(),
            "ls".to_owned(),
            json!({ "path": "missing-dir" }),
        )
        .await
        .expect_err("missing list path should fail");
    assert!(!list_error.to_string().is_empty());

    let glob_error = GlobTool
        .execute(
            ctx.clone(),
            "glob".to_owned(),
            json!({ "pattern": "[", "limit": 5 }),
        )
        .await
        .expect_err("invalid glob should fail");
    assert!(!glob_error.to_string().is_empty());

    let grep_error = GrepTool
        .execute(ctx.clone(), "grep".to_owned(), json!({ "pattern": "[" }))
        .await
        .expect_err("invalid regex should fail");
    assert!(!grep_error.to_string().is_empty());

    let bash_error = bash_tool(temp.path())
        .execute(ctx, "bash".to_owned(), json!({}))
        .await
        .expect_err("missing command should fail");
    assert!(
        bash_error
            .to_string()
            .contains("missing string field command")
    );
    Ok(())
}

#[test]
fn path_and_shell_helpers_cover_workspace_external_and_unknown_cases() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_file = outside.path().join("outside.txt");
    fs::write(&outside_file, "outside")?;

    let workspace_subject = super::tool_path_subject(workspace.path(), "new/missing.txt")?;
    assert_eq!(workspace_subject.scope, ToolSubjectScope::Workspace);
    assert_eq!(workspace_subject.normalized, "new/missing.txt");

    let external_subject =
        super::tool_path_subject(workspace.path(), outside_file.to_string_lossy().as_ref())?;
    let expected_external_file = outside_file.canonicalize()?;
    assert_eq!(external_subject.scope, ToolSubjectScope::External);
    assert_eq!(
        external_subject.canonical_path.as_deref(),
        Some(expected_external_file.as_path())
    );

    assert_eq!(
        super::command_permission_subject("  git   status   --short  "),
        "git status --short"
    );
    let long_subject = super::command_permission_subject(&"x ".repeat(100));
    assert!(long_subject.ends_with("..."));
    assert!(super::bash_command_is_safe_readonly(
        "git branch --show-current"
    ));
    assert!(!super::bash_command_is_safe_readonly("git branch -D main"));
    assert!(!super::bash_command_is_safe_readonly("command"));
    assert!(!super::bash_command_is_safe_readonly(""));
    Ok(())
}

#[test]
fn diff_and_text_limit_helpers_handle_noop_and_head_limits() {
    let diff = super::render_unified_diff("same\n", "same\n", "current", "proposed");
    assert_eq!(diff, "No textual changes detected.");

    let limited = super::limit_text_head("one\ntwo\nthree\n", 8, 2);
    assert!(limited.truncated);
    assert_eq!(limited.returned_lines, 2);
    assert!(limited.content.contains("output truncated"));

    let unchanged = super::limit_text_head_tail("short", 128);
    assert!(!unchanged.truncated);
    assert_eq!(unchanged.content, "short");
    assert_eq!(unchanged.omitted_bytes, 0);
}

#[test]
fn changeset_artifact_store_writes_diff_artifacts_and_hash_metadata() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let preview_diff =
        "--- current/note.txt\n+++ proposed/note.txt\n@@ -1 +1,2 @@\n-old\n+new\n+line\n";
    let reverse_diff =
        "--- proposed/note.txt\n+++ current/note.txt\n@@ -1,2 +1 @@\n-new\n-line\n+old\n";
    let store = ChangeSetArtifactStore::new(workspace.path())?;

    let record =
        store.write_diff_artifacts(ChangeSetId::new("change-1")?, preview_diff, reverse_diff)?;

    assert_eq!(record.artifact_dir, "state/artifacts/changesets/change-1");
    assert_eq!(
        record.preview.path,
        "state/artifacts/changesets/change-1/preview.diff"
    );
    assert_eq!(
        record.reverse.path,
        "state/artifacts/changesets/change-1/reverse.diff"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(&record.preview.path))?,
        preview_diff
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join(&record.reverse.path))?,
        reverse_diff
    );
    assert_eq!(record.preview.stats.added, 2);
    assert_eq!(record.preview.stats.removed, 1);
    assert_eq!(record.reverse.stats.added, 1);
    assert_eq!(record.reverse.stats.removed, 2);
    assert!(store.verify_diff_artifact(&record.preview)?);
    assert!(store.verify_diff_artifact(&record.reverse)?);

    fs::write(workspace.path().join(&record.preview.path), "tampered")?;
    assert!(!store.verify_diff_artifact(&record.preview)?);
    Ok(())
}

#[test]
fn changeset_artifact_store_bounds_large_diff_summary() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let preview_diff = (0..200)
        .map(|index| format!("+line-{index}\n"))
        .collect::<String>();
    let reverse_diff = preview_diff.replace("+line", "-line");
    let store = ChangeSetArtifactStore::new(workspace.path())?.with_summary_limit_bytes(96);

    let record = store.write_diff_artifacts(
        ChangeSetId::new("change-long")?,
        &preview_diff,
        &reverse_diff,
    )?;
    let serialized = serde_json::to_string(&record)?;

    assert!(record.summary.truncated);
    assert!(record.summary.omitted_bytes > 0);
    assert!(record.summary.text.contains("output truncated"));
    assert_eq!(record.summary.total_bytes, preview_diff.len() as u64);
    assert_eq!(
        fs::read_to_string(workspace.path().join(&record.preview.path))?,
        preview_diff
    );
    assert!(!serialized.contains("line-100"));
    assert!(serialized.contains("state/artifacts/changesets/change-long/preview.diff"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn changeset_artifact_store_writes_with_explicit_artifact_root() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let artifact_dir = workspace.path().join("custom-artifacts");
    let store = ChangeSetArtifactStore::new_with_artifact_root(
        workspace.path(),
        &artifact_dir,
        "custom-artifacts",
    )?;

    let record = store.write_diff_artifacts(ChangeSetId::new("change-1")?, "+new\n", "-old\n")?;
    assert!(artifact_dir.join("change-1/preview.diff").exists());
    assert_eq!(record.artifact_dir, "custom-artifacts/change-1");
    Ok(())
}

#[tokio::test]
async fn apply_changeset_tool_previews_and_applies_multi_file_changes() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("note.txt"), "old\n")?;
    fs::write(workspace.path().join("doomed.txt"), "remove me\n")?;
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5)
        .with_mutation_recorder(MutationEventRecorder::new(store.clone()));
    let args = json!({
        "id": "change-apply-1",
        "title": "Apply sample changes",
        "risk": "medium",
        "files": [
            { "path": "new.txt", "action": "create", "content": "created\n" },
            {
                "path": "note.txt",
                "action": "update",
                "old_text": "old",
                "new_text": "new",
                "before_hash": super::sha256_hex("old\n".as_bytes())
            },
            { "path": "doomed.txt", "action": "delete" }
        ]
    });

    let subjects = apply_changeset_tool().permission_subjects(&ctx, &args)?;
    assert_eq!(subjects.len(), 3);
    assert_eq!(subjects[0].normalized, "new.txt");

    let preview = apply_changeset_tool()
        .preview(ctx.clone(), args.clone())
        .await?
        .expect("apply_changeset should preview");
    assert!(preview.body.contains("--- current/new.txt"));
    assert!(preview.body.contains("+created"));
    assert_eq!(preview.file_diffs.len(), 3);
    assert!(
        !workspace
            .path()
            .join("state/artifacts/changesets/change-apply-1/preview.diff")
            .exists()
    );

    let result = apply_changeset_tool()
        .execute(ctx, "apply".to_owned(), args)
        .await?;

    assert!(!result.is_error());
    assert_eq!(
        fs::read_to_string(workspace.path().join("new.txt"))?,
        "created\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.path().join("note.txt"))?,
        "new\n"
    );
    assert!(!workspace.path().join("doomed.txt").exists());
    assert_eq!(
        result.metadata.changed_files,
        vec![
            "new.txt".to_owned(),
            "note.txt".to_owned(),
            "doomed.txt".to_owned()
        ]
    );
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("applied")
    );

    let reverse_path = result.metadata.details["artifacts"]["reverse"]["path"]
        .as_str()
        .expect("reverse artifact path");
    let reverse_diff = fs::read_to_string(workspace.path().join(reverse_path))?;
    assert!(reverse_diff.contains("rollback/note.txt"));
    assert!(reverse_diff.contains("+old"));
    assert_eq!(
        result.metadata.details["artifacts"]["reverse"]["sha256"],
        json!(super::sha256_hex(reverse_diff.as_bytes()))
    );
    assert_eq!(
        stored_event_types(&store)?,
        vec![
            DurableEventType::MutationBatchStarted.as_str(),
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
            DurableEventType::MutationBatchFinished.as_str(),
        ]
    );
    assert!(!result.to_model_content().contains("--- current/note.txt"));
    Ok(())
}

#[tokio::test]
async fn apply_changeset_hash_mismatch_does_not_write() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("note.txt"), "original\n")?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
    let result = apply_changeset_tool()
        .execute(
            ctx,
            "apply".to_owned(),
            json!({
                "id": "change-mismatch",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "content": "changed\n",
                    "before_hash": "not-the-current-hash"
                }]
            }),
        )
        .await?;

    assert!(result.is_error());
    assert_eq!(
        fs::read_to_string(workspace.path().join("note.txt"))?,
        "original\n"
    );
    assert!(
        !workspace
            .path()
            .join("state/artifacts/changesets/change-mismatch/preview.diff")
            .exists()
    );
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("failed")
    );
    assert!(result.to_model_content().contains("hash_mismatch"));
    Ok(())
}

#[tokio::test]
async fn apply_changeset_rejects_empty_file_list() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
    let args = json!({ "id": "change-empty", "files": [] });

    let preview_error = apply_changeset_tool()
        .preview(ctx.clone(), args.clone())
        .await
        .expect_err("empty change set should fail preview");
    assert!(
        preview_error
            .to_string()
            .contains("apply_changeset requires at least one file")
    );

    let execute_error = apply_changeset_tool()
        .execute(ctx, "apply".to_owned(), args)
        .await
        .expect_err("empty change set should fail execute");
    assert!(
        execute_error
            .to_string()
            .contains("apply_changeset requires at least one file")
    );
    Ok(())
}

#[tokio::test]
async fn apply_changeset_full_update_accepts_matching_mtime() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let file = workspace.path().join("note.txt");
    fs::write(&file, "old\n")?;
    let before_mtime_ms = super::metadata_mtime_ms(&fs::metadata(&file)?)
        .expect("regular file metadata should include mtime");
    let ctx = tool_context_with_mutation_recorder(workspace.path(), 5)?;
    let args = json!({
        "id": "change-full-update",
        "summary": "Replace note contents",
        "files": [{
            "path": "note.txt",
            "action": "update",
            "risk": "low",
            "content": "new\n",
            "before_mtime_ms": before_mtime_ms
        }]
    });

    let preview = apply_changeset_tool()
        .preview(ctx.clone(), args.clone())
        .await?
        .expect("full replacement should preview");
    assert!(preview.body.contains("+new"));

    let result = apply_changeset_tool()
        .execute(ctx, "apply".to_owned(), args)
        .await?;

    assert!(!result.is_error());
    assert_eq!(fs::read_to_string(file)?, "new\n");
    assert_eq!(
        result.metadata.details["change_set"]["files"][0]["after_hash"],
        json!(super::sha256_hex("new\n".as_bytes()))
    );
    Ok(())
}

#[tokio::test]
async fn apply_changeset_validation_reports_conflict_kinds_without_writes() -> Result<()> {
    let outside = tempfile::tempdir()?;
    let cases = vec![
        (
            "missing_content",
            json!({
                "id": "change-missing-content",
                "files": [{ "path": "new.txt", "action": "create" }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "duplicate_path",
            json!({
                "id": "change-duplicate",
                "files": [
                    { "path": "same.txt", "action": "create", "content": "one\n" },
                    { "path": "same.txt", "action": "create", "content": "two\n" }
                ]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "target_exists",
            json!({
                "id": "change-create-existing",
                "files": [{ "path": "exists.txt", "action": "create", "content": "new\n" }]
            }),
            vec![("exists.txt", b"old\n".as_slice())],
        ),
        (
            "missing_file",
            json!({
                "id": "change-update-missing",
                "files": [{ "path": "missing.txt", "action": "update", "content": "new\n" }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "ambiguous_update",
            json!({
                "id": "change-ambiguous-update",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "content": "new\n",
                    "old_text": "old",
                    "new_text": "new"
                }]
            }),
            vec![("note.txt", b"old\n".as_slice())],
        ),
        (
            "missing_snippet",
            json!({
                "id": "change-missing-old-text",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "new_text": "new"
                }]
            }),
            vec![("note.txt", b"old\n".as_slice())],
        ),
        (
            "missing_snippet",
            json!({
                "id": "change-missing-new-text",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "old_text": "old"
                }]
            }),
            vec![("note.txt", b"old\n".as_slice())],
        ),
        (
            "snippet_missing",
            json!({
                "id": "change-snippet-missing",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "old_text": "absent",
                    "new_text": "new"
                }]
            }),
            vec![("note.txt", b"old\n".as_slice())],
        ),
        (
            "binary_file",
            json!({
                "id": "change-binary-snippet",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "old_text": "old",
                    "new_text": "a\0b"
                }]
            }),
            vec![("note.txt", b"old\n".as_slice())],
        ),
        (
            "snippet_ambiguous",
            json!({
                "id": "change-snippet-ambiguous",
                "files": [{
                    "path": "note.txt",
                    "action": "update",
                    "old_text": "old",
                    "new_text": "new"
                }]
            }),
            vec![("note.txt", b"old old\n".as_slice())],
        ),
        (
            "invalid_delete_payload",
            json!({
                "id": "change-delete-payload",
                "files": [{ "path": "delete.txt", "action": "delete", "content": "bad\n" }]
            }),
            vec![("delete.txt", b"old\n".as_slice())],
        ),
        (
            "missing_file",
            json!({
                "id": "change-delete-missing",
                "files": [{ "path": "missing-delete.txt", "action": "delete" }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "binary_file",
            json!({
                "id": "change-binary-content",
                "files": [{ "path": "binary.txt", "action": "create", "content": "a\0b" }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "binary_file",
            json!({
                "id": "change-binary-update-content",
                "files": [{ "path": "note.txt", "action": "update", "content": "a\0b" }]
            }),
            vec![("note.txt", b"old\n".as_slice())],
        ),
        (
            "hash_mismatch",
            json!({
                "id": "change-create-before-hash",
                "files": [{
                    "path": "new.txt",
                    "action": "create",
                    "content": "new\n",
                    "before_hash": "expected-existing-hash"
                }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "mtime_changed",
            json!({
                "id": "change-mtime",
                "files": [{
                    "path": "mtime.txt",
                    "action": "update",
                    "content": "new\n",
                    "before_mtime_ms": 0
                }]
            }),
            vec![("mtime.txt", b"old\n".as_slice())],
        ),
        (
            "path_outside_workspace",
            json!({
                "id": "change-outside",
                "files": [{
                    "path": outside.path().join("outside.txt").to_string_lossy().to_string(),
                    "action": "create",
                    "content": "new\n"
                }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
        (
            "unsupported_action",
            json!({
                "id": "change-rename",
                "files": [{ "path": "old.txt", "action": "rename", "content": "new\n" }]
            }),
            Vec::<(&str, &[u8])>::new(),
        ),
    ];

    for (expected, args, files) in cases {
        let workspace = tempfile::tempdir()?;
        for (path, content) in files {
            fs::write(workspace.path().join(path), content)?;
        }
        let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
        let preview_error = apply_changeset_tool()
            .preview(ctx.clone(), args.clone())
            .await
            .expect_err("invalid changeset should fail preview");
        assert!(
            preview_error
                .to_string()
                .contains("change set validation failed"),
            "{expected} should fail preview with validation error"
        );
        let result = apply_changeset_tool()
            .execute(ctx, "apply".to_owned(), args)
            .await?;
        assert!(result.is_error(), "{expected} should return a tool error");
        assert!(
            result.to_model_content().contains(expected),
            "{expected} should be present in structured error content"
        );
        assert_eq!(
            result.metadata.details["apply_result"]["status"],
            json!("failed")
        );
    }
    Ok(())
}

#[tokio::test]
async fn apply_changeset_first_apply_failure_reports_failed_without_artifacts() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("blocked"), "not a directory\n")?;
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5)
        .with_mutation_recorder(MutationEventRecorder::new(store.clone()));

    let result = apply_changeset_tool()
        .execute(
            ctx,
            "apply".to_owned(),
            json!({
                "id": "change-first-failure",
                "files": [{ "path": "blocked/child.txt", "action": "create", "content": "child\n" }]
            }),
        )
        .await?;

    assert!(result.is_error());
    assert_eq!(
        fs::read_to_string(workspace.path().join("blocked"))?,
        "not a directory\n"
    );
    assert_eq!(result.metadata.changed_files, Vec::<String>::new());
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("failed")
    );
    assert_eq!(
        result.metadata.details["apply_result"]["file_results"][0]["status"],
        json!("failed")
    );
    assert!(result.metadata.details.get("artifacts").is_none());
    assert!(
        !stored_event_types(&store)?
            .iter()
            .any(|event_type| event_type == DurableEventType::MutationBatchFinished.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn apply_changeset_apply_stage_failure_records_failed_mutation_batch() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
    let plan = super::ApplyChangeSetPlan {
        change_set: ChangeSet {
            id: ChangeSetId::new("change-apply-stage-failure")?,
            title: "Apply stage failure".to_owned(),
            summary: "Apply stage failure".to_owned(),
            risk: ChangeSetRisk::Medium,
            files: vec![ChangeSetFile {
                path: "rename.txt".to_owned(),
                previous_path: Some("old-name.txt".to_owned()),
                action: ChangeSetFileAction::Rename,
                risk: ChangeSetRisk::Medium,
                before_hash: None,
                after_hash: None,
                diff_hash: None,
                additions: 0,
                deletions: 0,
                validations: Vec::new(),
            }],
            validations: Vec::new(),
        },
        files: vec![super::PlannedChangeSetFile {
            path: "rename.txt".to_owned(),
            absolute_path: workspace.path().join("rename.txt"),
            action: ChangeSetFileAction::Rename,
            after_content: None,
            preview_diff: String::new(),
            reverse_diff: String::new(),
            validations: Vec::new(),
        }],
        preview_diff: String::new(),
        reverse_diff: String::new(),
    };

    let result = super::apply_changeset_plan(
        workspace.path(),
        &workspace.path().join("state/artifacts/changesets"),
        PathBuf::from("state/artifacts/changesets"),
        "apply".to_owned(),
        Some(MutationEventRecorder::new(store.clone())),
        plan,
    )?;

    assert!(result.is_error());
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("failed")
    );
    assert!(
        stored_event_types(&store)?
            .iter()
            .any(|event_type| event_type == DurableEventType::MutationBatchFinished.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn apply_changeset_binary_existing_file_does_not_write() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("binary.txt"), b"a\0b")?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);
    let result = apply_changeset_tool()
        .execute(
            ctx,
            "apply".to_owned(),
            json!({
                "id": "change-binary-existing",
                "files": [{ "path": "binary.txt", "action": "update", "content": "text\n" }]
            }),
        )
        .await?;

    assert!(result.is_error());
    assert!(result.to_model_content().contains("binary_file"));
    assert_eq!(fs::read(workspace.path().join("binary.txt"))?, b"a\0b");
    Ok(())
}

#[tokio::test]
async fn apply_changeset_rejects_unreadable_text_and_directories() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(
        workspace.path().join("invalid-utf8.txt"),
        [0xff_u8, 0xfe, 0xfd],
    )?;
    fs::create_dir(workspace.path().join("dir"))?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let invalid_utf8 = apply_changeset_tool()
        .execute(
            ctx.clone(),
            "apply-invalid-utf8".to_owned(),
            json!({
                "id": "change-invalid-utf8",
                "files": [{ "path": "invalid-utf8.txt", "action": "update", "content": "text\n" }]
            }),
        )
        .await?;
    assert!(invalid_utf8.is_error());
    assert!(invalid_utf8.to_model_content().contains("binary_file"));
    assert_eq!(
        fs::read(workspace.path().join("invalid-utf8.txt"))?,
        [0xff_u8, 0xfe, 0xfd]
    );

    let directory_target = apply_changeset_tool()
        .execute(
            ctx,
            "apply-directory".to_owned(),
            json!({
                "id": "change-directory",
                "files": [{ "path": "dir", "action": "update", "content": "text\n" }]
            }),
        )
        .await?;
    assert!(directory_target.is_error());
    assert!(
        directory_target
            .to_model_content()
            .contains("not_regular_file")
    );
    assert!(workspace.path().join("dir").is_dir());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn apply_changeset_rejects_symlink_escape_and_reports_artifact_failure() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("target.txt"), "outside\n")?;
    symlink(
        outside.path().join("target.txt"),
        workspace.path().join("link.txt"),
    )?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5);

    let symlink_result = apply_changeset_tool()
        .execute(
            ctx.clone(),
            "apply".to_owned(),
            json!({
                "id": "change-symlink",
                "files": [{ "path": "link.txt", "action": "update", "content": "new\n" }]
            }),
        )
        .await?;
    assert!(symlink_result.is_error());
    assert!(
        symlink_result
            .to_model_content()
            .contains("path_outside_workspace")
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("target.txt"))?,
        "outside\n"
    );

    Ok(())
}

#[tokio::test]
async fn apply_changeset_partial_apply_reports_applied_and_skipped_files() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 5)
        .with_mutation_recorder(MutationEventRecorder::new(store.clone()));
    let result = apply_changeset_tool()
        .execute(
            ctx,
            "apply".to_owned(),
            json!({
                "id": "change-partial",
                "files": [
                    { "path": "blocked", "action": "create", "content": "file\n" },
                    { "path": "blocked/child.txt", "action": "create", "content": "child\n" },
                    { "path": "after.txt", "action": "create", "content": "after\n" }
                ]
            }),
        )
        .await?;

    assert!(result.is_error());
    assert_eq!(
        fs::read_to_string(workspace.path().join("blocked"))?,
        "file\n"
    );
    assert!(!workspace.path().join("blocked/child.txt").exists());
    assert!(!workspace.path().join("after.txt").exists());
    assert_eq!(result.metadata.changed_files, vec!["blocked".to_owned()]);
    assert_eq!(
        result.metadata.details["apply_result"]["status"],
        json!("partially_applied")
    );
    assert_eq!(
        result.metadata.details["apply_result"]["file_results"][0]["status"],
        json!("applied")
    );
    assert_eq!(
        result.metadata.details["apply_result"]["file_results"][1]["status"],
        json!("failed")
    );
    assert_eq!(
        result.metadata.details["apply_result"]["file_results"][2]["status"],
        json!("skipped")
    );
    let reverse_path = result.metadata.details["artifacts"]["reverse"]["path"]
        .as_str()
        .expect("reverse artifact path");
    let reverse_diff = fs::read_to_string(workspace.path().join(reverse_path))?;
    assert!(reverse_diff.contains("rollback/blocked"));
    assert!(!reverse_diff.contains("after.txt"));
    assert!(
        stored_event_types(&store)?
            .iter()
            .any(|event_type| event_type == DurableEventType::MutationBatchFinished.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn write_file_execute_creates_parent_dirs_and_reports_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = tool_context_with_mutation_recorder(temp.path(), 5)?;

    let result = WriteFileTool
        .execute(
            ctx,
            "write".to_owned(),
            json!({ "path": "nested/dir/note.txt", "content": "hello" }),
        )
        .await?;

    assert_eq!(
        fs::read_to_string(temp.path().join("nested/dir/note.txt"))?,
        "hello"
    );
    assert_eq!(result.metadata.changed_files, vec!["nested/dir/note.txt"]);
    assert_eq!(result.metadata.bytes, Some(5));
    Ok(())
}

#[tokio::test]
async fn edit_file_execute_and_preview_reject_missing_and_ambiguous_matches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let ctx = ToolContext::new(temp.path().to_path_buf(), 5);
    let file = temp.path().join("note.txt");
    fs::write(&file, "hello old old\n")?;

    let ambiguous = EditFileTool
        .execute(
            ctx.clone(),
            "edit".to_owned(),
            json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
        )
        .await
        .expect_err("ambiguous replacements should fail");
    assert!(ambiguous.to_string().contains("ambiguous"));

    let missing = EditFileTool
        .preview(
            ctx,
            json!({ "path": "note.txt", "old_text": "missing", "new_text": "new" }),
        )
        .await
        .expect_err("missing replacements should fail");
    assert!(missing.to_string().contains("not found"));
    Ok(())
}

#[test]
fn builtin_text_limit_and_path_helpers_cover_multibyte_edges() -> Result<()> {
    let limited = super::limit_text_head("one\ntwo\nthree", 7, 5);
    assert!(limited.truncated);
    assert!(limited.content.contains("output truncated"));

    let tail = super::limit_text_head_tail("abcdef", 5);
    assert!(tail.truncated);
    assert!(tail.content.contains("omitted"));
    assert!(tail.content.contains('\n'));

    let long_line = "x".repeat(super::MAX_MODEL_LINE_CHARS + 1);
    let truncated = super::truncate_line_for_model(&long_line);
    assert!(truncated.ends_with("[sigil: line truncated]"));

    let mut notice_only = String::new();
    super::append_truncation_notice(&mut notice_only);
    assert!(notice_only.starts_with("[sigil: output truncated"));

    let value = "a中b";
    assert_eq!(&value[..super::floor_char_boundary(value, 2)], "a");
    assert_eq!(&value[super::ceil_char_boundary(value, 2)..], "b");

    assert_eq!(
        super::lexically_normalize_path(Path::new("./notes/../draft.txt"))?,
        Path::new("draft.txt")
    );
    assert_eq!(
        super::lexically_normalize_path(Path::new("notes/../../draft.txt"))?,
        Path::new("../draft.txt")
    );

    let workspace = tempfile::tempdir()?;
    let resolved = super::resolve_existing_prefix(&workspace.path().join("missing/child.txt"))?;
    assert_eq!(
        resolved,
        workspace.path().canonicalize()?.join("missing/child.txt")
    );

    let missing_root = workspace.path().join("does-not-exist");
    assert!(
        super::canonical_workspace_root(&missing_root)
            .expect_err("missing workspaces should fail")
            .to_string()
            .contains("failed to resolve workspace root")
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn delete_file_and_path_resolution_helpers_cover_external_and_symlink_paths() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let workspace_file = workspace.path().join("note.txt");
    let outside_file = outside.path().join("secret.txt");
    fs::write(&workspace_file, "hello")?;
    fs::write(&outside_file, "secret")?;

    let target = super::resolve_delete_file_target(
        workspace.path(),
        workspace_file.to_str().expect("utf8 path"),
    )?;
    assert_eq!(target.path, workspace_file);
    assert_eq!(target.display_path, target.path.display().to_string());

    let outside_error = super::resolve_delete_file_target(
        workspace.path(),
        outside_file.to_str().expect("utf8 path"),
    )
    .expect_err("external delete targets should be rejected");
    assert!(outside_error.to_string().contains("outside workspace"));

    symlink(&outside_file, workspace.path().join("link.txt"))?;
    let symlink_error =
        super::validate_delete_file_target(&workspace.path().join("link.txt"), "link.txt")
            .expect_err("symlink delete targets should be rejected");
    assert!(
        symlink_error
            .to_string()
            .contains("does not support symlink")
    );
    Ok(())
}

#[test]
fn bash_and_shell_helper_functions_cover_parser_edges() -> Result<()> {
    assert!(!super::bash_command_is_safe_readonly(r#""""#));
    assert!(super::contains_unsupported_safe_shell_syntax("echo $HOME"));
    assert!(!super::bash_segment_is_safe_readonly(&[]));
    assert!(!super::bash_segment_is_safe_readonly(&[
        "cat".to_owned(),
        ">".to_owned(),
        "out.txt".to_owned(),
    ]));
    assert!(!super::git_segment_is_safe_readonly(&["git".to_owned()]));
    assert!(super::git_segment_is_safe_readonly(&[
        "git".to_owned(),
        "branch".to_owned(),
        "--list".to_owned(),
    ]));
    assert!(super::shell_command_is_destructive("rm -rf .sigil"));
    assert!(super::shell_command_is_destructive("git clean -fdx"));
    assert!(super::shell_command_is_destructive("git reset --hard"));
    assert!(super::shell_command_is_destructive("find . -delete"));
    assert!(super::shell_command_is_destructive(
        "dd if=/dev/zero of=target.bin bs=1"
    ));
    assert!(super::shell_command_is_destructive(
        "echo ok; rm -rf .sigil"
    ));
    assert!(super::shell_command_is_destructive("find . -exec rm {} ;"));
    assert!(super::shell_command_is_destructive("git restore --force ."));
    assert!(super::shell_command_is_destructive(
        "sh -lc 'rm -rf .sigil'"
    ));
    assert!(!super::shell_command_is_destructive("echo ok; printf done"));
    assert!(!super::shell_command_is_destructive("grep rm README.md"));
    assert_eq!(
        super::shell_command_permission_operation("cat Cargo.toml"),
        ToolOperation::ExecuteReadOnlyCommand
    );
    assert_eq!(
        super::shell_command_permission_operation("echo hello"),
        ToolOperation::ExecuteUnknownCommand
    );
    assert_eq!(
        super::terminal_input_permission_operation("rm -rf .sigil"),
        ToolOperation::ExecuteDestructiveCommand
    );
    assert_eq!(
        super::terminal_input_permission_operation("echo hello"),
        ToolOperation::SendTerminalInput
    );
    assert_eq!(
        super::shell_segment_command_and_args(&["FOO=bar".to_owned(), "rm".to_owned()])
            .map(|(command, args)| (command.to_owned(), args.len())),
        Some(("rm".to_owned(), 0))
    );
    assert!(super::shell_segment_command_and_args(&["FOO=bar".to_owned()]).is_none());

    let tokens =
        super::tokenize_shell_subject_words(r#"echo "a b" foo\ bar && cat file || ls; pwd"#);
    assert_eq!(
        tokens,
        vec![
            "echo", "a b", "foo bar", "&&", "cat", "file", "||", "ls", ";", "pwd",
        ]
    );
    assert_eq!(super::redirection_target("1>out.txt"), Some("out.txt"));
    assert_eq!(super::redirection_target("&>>all.log"), Some("all.log"));
    assert_eq!(super::redirection_target("2>>err.log"), Some("err.log"));
    assert_eq!(super::redirection_target("<"), None);
    assert_eq!(
        super::redirection_target("2>stderr.log"),
        Some("stderr.log")
    );
    assert!(super::is_redirection_operator("<<"));
    assert!(!super::is_path_argument("git", "--help"));
    assert!(!super::is_path_argument("cat", "https://example.com/file"));
    assert!(!super::is_path_argument("cat", "-n"));
    assert!(super::is_path_argument("cat", "Cargo.toml"));
    assert!(!super::is_path_argument("echo", "Cargo.toml"));
    assert_eq!(
        super::render_unified_diff("same\n", "same\n", "a", "b"),
        "No textual changes detected."
    );

    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("note.txt"), "note")?;
    let workspace_root = workspace.path().canonicalize()?;
    let dd_subjects = super::bash_path_subjects_from_cwd(
        &workspace_root,
        &workspace_root,
        "dd if=/dev/zero of=target.bin bs=1",
    )?;
    assert!(dd_subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path && subject.normalized == "target.bin"
    }));

    let mut cwd = workspace_root.clone();
    let mut subjects = Vec::new();
    super::collect_bash_segment_subjects(&workspace_root, &mut cwd, &[], &mut subjects)?;
    assert!(subjects.is_empty());

    super::collect_bash_segment_subjects(
        &workspace_root,
        &mut cwd,
        &["cd".to_owned(), "-".to_owned()],
        &mut subjects,
    )?;
    assert_eq!(cwd, workspace_root);

    super::collect_bash_segment_subjects(
        &workspace_root,
        &mut cwd,
        &[
            "cat".to_owned(),
            "./note.txt".to_owned(),
            "1>out.txt".to_owned(),
            ">".to_owned(),
            "nested/out.txt".to_owned(),
        ],
        &mut subjects,
    )?;
    assert_eq!(subjects.len(), 3);
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "note.txt")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "out.txt")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "nested/out.txt")
    );

    let no_target_subjects = super::bash_path_subjects(workspace.path(), "cat < && cd - && ls")?;
    assert!(no_target_subjects.is_empty());
    Ok(())
}

#[test]
fn bash_path_subjects_and_tokenizer_cover_segmented_and_quoted_edges() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::create_dir(workspace.path().join("src"))?;
    fs::write(
        workspace.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )?;
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")?;
    let workspace_root = workspace.path().canonicalize()?;

    let tokens =
        super::tokenize_shell_subject_words(r#"echo "a\"b" && cat src/lib.rs || ls Cargo.toml"#);
    assert_eq!(
        tokens,
        vec![
            "echo",
            "a\"b",
            "&&",
            "cat",
            "src/lib.rs",
            "||",
            "ls",
            "Cargo.toml",
        ]
    );
    let compact_tokens =
        super::tokenize_shell_subject_words(r#"echo hi&&cat 'src/lib.rs'||pwd;ls"#);
    assert_eq!(
        compact_tokens,
        vec![
            "echo",
            "hi",
            "&&",
            "cat",
            "src/lib.rs",
            "||",
            "pwd",
            ";",
            "ls",
        ]
    );

    let subjects = super::bash_path_subjects(
        workspace.path(),
        "cd src && cat lib.rs || ls ../Cargo.toml; cat <lib.rs &>../combined.log",
    )?;

    assert_eq!(subjects.len(), 5);
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(workspace_root.join("src").as_path())
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "src/lib.rs")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "Cargo.toml")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "combined.log")
    );
    Ok(())
}

#[test]
fn lexical_normalize_path_returns_dot_for_current_directory() -> Result<()> {
    assert_eq!(
        super::lexically_normalize_path(Path::new("."))?,
        Path::new(".")
    );
    Ok(())
}

fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.to_owned(),
        args_json: serde_json::to_string(&args).expect("tool args should serialize"),
    }
}

async fn wait_for_terminal_read(
    registry: &ToolRegistry,
    ctx: ToolContext,
    task_id: &str,
    limit_bytes: usize,
) -> Result<sigil_kernel::ToolResult> {
    for _ in 0..250 {
        let result = registry
            .execute(
                ctx.clone(),
                tool_call(
                    "terminal_read",
                    json!({
                        "task_id": task_id,
                        "limit_bytes": limit_bytes,
                        "include_content": true
                    }),
                ),
            )
            .await?;
        if result.metadata.total_bytes.unwrap_or_default() >= 10 {
            return Ok(result);
        }
        sleep(Duration::from_millis(20)).await;
    }
    registry
        .execute(
            ctx,
            tool_call(
                "terminal_read",
                json!({
                    "task_id": task_id,
                    "limit_bytes": limit_bytes,
                    "include_content": true
                }),
            ),
        )
        .await
}

async fn wait_for_terminal_read_contains(
    registry: &ToolRegistry,
    ctx: ToolContext,
    task_id: &str,
    needle: &str,
) -> Result<sigil_kernel::ToolResult> {
    for _ in 0..250 {
        let result = registry
            .execute(
                ctx.clone(),
                tool_call(
                    "terminal_read",
                    json!({
                        "task_id": task_id,
                        "limit_bytes": 1024,
                        "include_content": true
                    }),
                ),
            )
            .await?;
        if result.content.contains(needle) {
            return Ok(result);
        }
        sleep(Duration::from_millis(20)).await;
    }
    registry
        .execute(
            ctx,
            tool_call(
                "terminal_read",
                json!({
                    "task_id": task_id,
                    "limit_bytes": 1024,
                    "include_content": true
                }),
            ),
        )
        .await
}

#[cfg(unix)]
fn test_shell(dir: &Path) -> Result<String> {
    let shell = dir.join("test-shell");
    fs::write(
        &shell,
        "#!/bin/sh\nif [ \"$1\" = \"-lc\" ]; then shift; fi\nexec /bin/sh -c \"$1\"\n",
    )?;
    let mut permissions = fs::metadata(&shell)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shell, permissions)?;
    Ok(shell.display().to_string())
}

#[cfg(not(unix))]
fn test_shell(_dir: &Path) -> Result<String> {
    Ok("sh".to_owned())
}
