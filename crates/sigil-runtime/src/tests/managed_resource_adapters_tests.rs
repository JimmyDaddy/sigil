use super::*;
use sigil_kernel::resource::CanonicalHash;
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};
use std::time::Duration;

fn test_process_inventory() -> Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1> {
    Arc::new(sigil_resource_authority::InMemoryAuthorityProcessInventoryV1::default())
}

#[cfg(unix)]
fn output_command(output: &str) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/sh"),
        vec!["-c".to_owned(), format!("printf {output}")],
    )
}

#[cfg(windows)]
fn output_command(output: &str) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("pwsh.exe"),
        vec![
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            format!("[Console]::Out.Write('{output}')"),
        ],
    )
}

#[cfg(windows)]
fn comspec_path() -> PathBuf {
    std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
}

#[cfg(windows)]
fn powershell_path() -> PathBuf {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            candidates.push(directory.join("pwsh.exe"));
            candidates.push(directory.join("powershell.exe"));
        }
    }
    for variable in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(directory) = std::env::var_os(variable) {
            let directory = PathBuf::from(directory);
            candidates.push(directory.join(r"PowerShell\7\pwsh.exe"));
            candidates.push(directory.join(r"WindowsPowerShell\v1.0\powershell.exe"));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"))
}

#[cfg(unix)]
fn pty_environment() -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::new()
}

#[cfg(windows)]
fn pty_environment() -> std::collections::BTreeMap<String, String> {
    let mut environment = std::collections::BTreeMap::new();
    for name in [
        "PATH",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "LOCALAPPDATA",
        "APPDATA",
        "PROGRAMDATA",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(name.to_owned(), value.to_string_lossy().into_owned());
        }
    }
    environment
}

#[cfg(unix)]
fn pty_line_command() -> (String, Vec<String>) {
    (
        "/bin/sh".to_owned(),
        vec![
            "-c".to_owned(),
            "printf 'runtime-pty-ready\\n'; read line; printf '%s\\n' \"$line\"".to_owned(),
        ],
    )
}

#[cfg(windows)]
fn pty_line_command() -> (String, Vec<String>) {
    (
        powershell_path().to_string_lossy().into_owned(),
        vec![
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            "[Console]::WriteLine('runtime-pty-ready'); [Console]::Out.Flush(); $line = [Console]::ReadLine(); [Console]::WriteLine($line)".to_owned(),
        ],
    )
}

#[cfg(unix)]
fn readiness_command(readiness: &str) -> (String, Vec<String>) {
    (
        "sh".to_owned(),
        vec![
            "-lc".to_owned(),
            format!("printf '{readiness}\\n'; while :; do sleep 1; done"),
        ],
    )
}

#[cfg(windows)]
fn readiness_command(readiness: &str) -> (String, Vec<String>) {
    (
        "cmd.exe".to_owned(),
        vec![
            "/D".to_owned(),
            "/C".to_owned(),
            format!("echo {readiness}& ping -n 30 127.0.0.1 >NUL"),
        ],
    )
}

#[cfg(unix)]
fn manager_persistent_command() -> (String, String) {
    ("sleep 30".to_owned(), "/bin/sh".to_owned())
}

#[cfg(windows)]
fn manager_persistent_command() -> (String, String) {
    (
        "ping -n 30 127.0.0.1 >NUL".to_owned(),
        comspec_path().to_string_lossy().into_owned(),
    )
}

#[test]
fn r71_runtime_compose_holds_only_pathless_ports() {
    let storage = Arc::new(AuthorityManagedStorageServiceV1::new(
        AuthorityStorageGrantTableV1::new(),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([1u8; 32]),
        },
    ));
    let file_access = sigil_resource_authority::file_access_stub::stub_file_access_service();
    let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new(
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 2,
            instance_hash: CanonicalHash::from_bytes([2u8; 32]),
        },
        storage,
        file_access,
    )
    .build_bundle();
    let composed = RuntimeManagedResourceServicesV1::compose(
        bundle,
        sigil_kernel::capability_issuer::mock_issuer(),
        Arc::new(StubProjectionServiceV1),
    );
    // The runtime view is fully trait-object-y: no concrete authority type escapes.
    let _ = composed.file_access;
    let _ = composed.storage;
    let _ = composed.projection;
}

#[tokio::test]
async fn r71_managed_command_route_seals_and_executes_one_shot() {
    use sigil_tools_builtin::ManagedCommandExecutionPortV1;

    let root = tempfile::tempdir().expect("workspace");
    let execution_temp = tempfile::tempdir().expect("execution temp");
    let route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
        execution_temp.path().to_path_buf(),
    )
    .with_process_inventory(test_process_inventory());
    let mut environment = std::collections::BTreeMap::new();
    environment.insert("TMPDIR".to_owned(), "/ambient-override".to_owned());
    environment.insert("HOME".to_owned(), "/ambient-home".to_owned());
    #[cfg(unix)]
    let (program, args) = (
        "/bin/sh".to_owned(),
        vec![
            "-c".to_owned(),
            concat!(
                "test -d \"$TMPDIR\" && test -d \"$HOME\" && ",
                "test -d \"$XDG_STATE_HOME\" && test -d \"$XDG_CACHE_HOME\" && ",
                "test -d \"$SIGIL_STATE_HOME\" && test -d \"$SIGIL_CACHE_HOME\" && ",
                "test \"$TMPDIR\" != /ambient-override && ",
                "test \"$HOME\" != /ambient-home && ",
                "touch \"$TMPDIR/child-created\" && printf managed-route"
            )
            .to_owned(),
        ],
    );
    #[cfg(windows)]
    let (program, args) = (
        "pwsh.exe".to_owned(),
        vec![
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            concat!(
                "$required = @($env:TMPDIR, $env:HOME, $env:XDG_STATE_HOME, ",
                "$env:XDG_CACHE_HOME, $env:SIGIL_STATE_HOME, $env:SIGIL_CACHE_HOME); ",
                "foreach ($path in $required) { ",
                "if (-not (Test-Path -LiteralPath $path -PathType Container)) { exit 1 } ",
                "}; ",
                "if ($env:TMPDIR -eq '/ambient-override' -or $env:HOME -eq '/ambient-home') { exit 1 }; ",
                "New-Item -ItemType File -Path (Join-Path $env:TMPDIR 'child-created') -Force | Out-Null; ",
                "[Console]::Out.Write('managed-route')"
            )
            .to_owned(),
        ],
    );
    let receipt = route
        .execute_with_cancellation(
            sigil_kernel::ExecutionRequest {
                program,
                args,
                cwd: root.path().to_path_buf(),
                env: environment,
                environment_policy: sigil_kernel::ProcessEnvironmentPolicy::default(),
                timeout_ms: Some(30_000),
                timeout_secs: 30,
                cpu_time_ms: None,
                memory_limit_bytes: None,
                process_count_limit: None,
                capture: None,
            },
            None,
        )
        .await
        .expect("managed command route");
    assert_eq!(receipt.stdout, b"managed-route");
    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(receipt.backend, sigil_kernel::ExecutionBackendKind::Local);
    assert_eq!(
        receipt.resources.cleanup.status,
        sigil_kernel::ExecutionCleanupStatus::Completed
    );
    assert_eq!(
        std::fs::read_dir(execution_temp.path())
            .expect("execution temp anchor")
            .count(),
        0,
        "the exact per-attempt generation must be released"
    );
}

#[tokio::test]
async fn r71_managed_code_intel_route_bridges_stdout_and_finalizes_process() {
    use sigil_code_intel::LanguageServerLaunchPortV1;
    use tokio::io::AsyncReadExt;

    let root = tempfile::tempdir().expect("workspace");
    let execution_temp = tempfile::tempdir().expect("execution temp");
    let route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
        execution_temp.path().to_path_buf(),
    )
    .with_process_inventory(test_process_inventory());
    let (program, args) = output_command("managed-code-intel");
    let io = route
        .launch(sigil_code_intel::LanguageServerLaunchRequestV1 {
            server_name: "fake-lsp".to_owned(),
            program,
            args,
            cwd: root.path().to_path_buf(),
            environment: Vec::new(),
        })
        .await
        .expect("managed code-intel route");
    let (mut reader, _writer, _shutdown) = io.into_parts();
    let mut output = Vec::new();
    reader
        .read_to_end(&mut output)
        .await
        .expect("managed code-intel output");
    assert_eq!(output, b"managed-code-intel");
    assert_eq!(
        std::fs::read_dir(execution_temp.path())
            .expect("execution temp anchor")
            .count(),
        0,
        "code-intel settlement must release the exact ExecutionTemp generation"
    );
}

#[tokio::test]
async fn r71_managed_terminal_route_seals_and_owns_persistent_process() {
    use sigil_tools_builtin::{ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1};

    let root = tempfile::tempdir().expect("workspace");
    let execution_temp = tempfile::tempdir().expect("execution temp");
    let route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
        execution_temp.path().to_path_buf(),
    )
    .with_process_inventory(test_process_inventory());
    let (program, args) = output_command("terminal-route");
    let mut handle = route
        .start_persistent(ManagedTerminalStartRequestV1 {
            program: program.to_string_lossy().into_owned(),
            args,
            cwd: root.path().to_path_buf(),
            environment: std::collections::BTreeMap::new(),
            pty_size: None,
        })
        .await
        .expect("managed terminal route");
    let mut stream = handle.take_output_stream().expect("managed stream");
    let mut output = Vec::new();
    while let Some(frame) = stream.next_frame().await.expect("output frame") {
        if !frame.end_of_stream {
            output.extend(frame.payload);
        }
    }
    let receipt = handle.wait_and_finalize().await.expect("terminal receipt");
    assert_eq!(output, b"terminal-route");
    assert!(matches!(
        receipt.process.termination,
        sigil_kernel::managed_execution::ProcessTerminationV1::Exited { code: 0 }
    ));
    assert_eq!(
        receipt.resources.cleanup_status,
        sigil_kernel::resource::ResourceCleanupStatusV1::Released
    );
    assert_eq!(
        std::fs::read_dir(execution_temp.path())
            .expect("execution temp anchor")
            .count(),
        0,
        "persistent settlement must release the exact ExecutionTemp generation"
    );
}

#[tokio::test]
async fn r71_managed_terminal_route_supports_pty_control_and_receipt() {
    use sigil_tools_builtin::{ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1};

    let root = tempfile::tempdir().expect("workspace");
    let execution_temp = tempfile::tempdir().expect("execution temp");
    let route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
        execution_temp.path().to_path_buf(),
    )
    .with_process_inventory(test_process_inventory());
    let (program, args) = pty_line_command();
    let mut handle = tokio::time::timeout(
        Duration::from_secs(30),
        route.start_persistent(ManagedTerminalStartRequestV1 {
            program,
            args,
            cwd: root.path().to_path_buf(),
            environment: pty_environment(),
            pty_size: Some(sigil_kernel::managed_execution::BoundedPtySizeV1 {
                rows: 24,
                cols: 80,
            }),
        }),
    )
    .await
    .expect("pty start timed out")
    .expect("managed pty terminal route");
    tokio::time::timeout(
        Duration::from_secs(30),
        handle.resize_pty(sigil_kernel::managed_execution::BoundedPtySizeV1 {
            rows: 32,
            cols: 100,
        }),
    )
    .await
    .expect("pty resize timed out")
    .expect("resize");
    let mut stream = handle.take_output_stream().expect("managed stream");
    let readiness = b"runtime-pty-ready";
    let mut output = Vec::new();
    loop {
        let frame = stream.next_frame();
        let frame = tokio::time::timeout(Duration::from_secs(30), frame)
            .await
            .expect("pty readiness timed out")
            .expect("output frame")
            .expect("pty ended before readiness marker");
        if !frame.end_of_stream {
            output.extend(frame.payload);
            if output
                .windows(readiness.len())
                .any(|window| window == readiness)
            {
                break;
            }
        }
    }
    #[cfg(unix)]
    let input = b"runtime-pty\n".to_vec();
    #[cfg(windows)]
    // ConPTY presents a real Windows console line discipline to PowerShell; CRLF is required to
    // submit the line to `Console.ReadLine`, whereas the Unix shell test consumes LF directly.
    let input = b"runtime-pty\r\n".to_vec();
    tokio::time::timeout(
        Duration::from_secs(30),
        handle
            .write_stdin(sigil_kernel::managed_execution::BoundedProcessInputV1 { payload: input }),
    )
    .await
    .expect("pty write timed out")
    .expect("write");
    tokio::time::timeout(Duration::from_secs(30), handle.close_stdin())
        .await
        .expect("pty close timed out")
        .expect("close");
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(30), stream.next_frame())
        .await
        .expect("pty output timed out")
        .expect("output frame")
    {
        if !frame.end_of_stream {
            output.extend(frame.payload);
        }
    }
    let receipt = tokio::time::timeout(Duration::from_secs(30), handle.wait_and_finalize())
        .await
        .expect("pty finalization timed out")
        .expect("terminal receipt");
    assert!(
        output
            .windows(b"runtime-pty".len())
            .any(|window| window == b"runtime-pty")
    );
    assert!(matches!(
        receipt.process.termination,
        sigil_kernel::managed_execution::ProcessTerminationV1::Exited { code: 0 }
    ));
    assert_eq!(
        receipt.resources.cleanup_status,
        sigil_kernel::resource::ResourceCleanupStatusV1::Released
    );
}

#[tokio::test]
async fn r71_managed_terminal_route_accepts_runtime_shell_environment_and_readiness() {
    use sigil_tools_builtin::{ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1};

    let root = tempfile::tempdir().expect("workspace");
    let execution_temp = tempfile::tempdir().expect("execution temp");
    let scratch = tempfile::tempdir().expect("scratch");
    let route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
        execution_temp.path().to_path_buf(),
    )
    .with_process_inventory(test_process_inventory());
    let readiness = "runtime-terminal-ready";
    let (program, args) = readiness_command(readiness);
    let mut handle = route
        .start_persistent(ManagedTerminalStartRequestV1 {
            program,
            args,
            cwd: root.path().to_path_buf(),
            environment: [(
                "SIGIL_SCRATCH_DIR".to_owned(),
                scratch.path().to_string_lossy().into_owned(),
            )]
            .into_iter()
            .collect(),
            pty_size: None,
        })
        .await
        .expect("managed terminal route with runtime environment");
    let mut stream = handle.take_output_stream().expect("managed stream");
    let frame = stream
        .next_frame()
        .await
        .expect("output frame")
        .expect("readiness output");
    assert!(String::from_utf8_lossy(&frame.payload).contains(readiness));
    handle
        .cancel(sigil_kernel::managed_execution::ProcessCancelReasonV1::UserCancelled)
        .await
        .expect("cancel terminal");
    handle.wait_and_finalize().await.expect("terminal receipt");
}

#[tokio::test]
async fn r71_managed_terminal_manager_cancel_waits_for_persistent_receipt() -> anyhow::Result<()> {
    let root = tempfile::tempdir().expect("workspace");
    let artifact_root = tempfile::tempdir().expect("artifacts");
    let execution_temp = tempfile::tempdir().expect("execution temp");
    let route = Arc::new(
        RuntimeManagedCommandExecutionRouteV1::new(
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp.path().to_path_buf(),
        )
        .with_process_inventory(test_process_inventory()),
    );
    let manager = sigil_tools_builtin::TerminalProcessManager::new_with_artifact_root(
        root.path(),
        artifact_root.path(),
        "state/artifacts/tasks",
    )?
    .with_managed_execution(route);
    let (command, shell) = manager_persistent_command();
    let entry = manager
        .start_pty(
            sigil_tools_builtin::TerminalStartRequest {
                command,
                shell: Some(shell),
                ..Default::default()
            },
            Some(sigil_tools_builtin::TerminalPtySize { rows: 24, cols: 80 }),
        )
        .await?;
    assert_eq!(
        entry.handle.execution_backend,
        Some(sigil_kernel::terminal_task::TerminalExecutionBackendKind::SandboxedPty)
    );
    manager
        .resize(
            &entry.handle.task_id,
            sigil_tools_builtin::TerminalPtySize {
                rows: 32,
                cols: 100,
            },
        )
        .await?;
    let input = manager
        .input(&entry.handle.task_id, "managed-input\n")
        .await?;
    assert_eq!(input.input_bytes, "managed-input\n".len() as u64);
    let cancelled = manager.cancel(&entry.handle.task_id).await?;
    assert!(matches!(
        cancelled.status,
        sigil_kernel::TerminalTaskStatus::Cancelled
    ));
    Ok(())
}

/// Minimal projection stub for the isolated composition test.
struct StubProjectionServiceV1;

#[async_trait::async_trait]
impl ManagedProjectionServiceV1 for StubProjectionServiceV1 {
    async fn open_rebuildable_projection(
        &self,
        _handle: &sigil_kernel::managed_storage::ManagedStorageNamespaceHandleV1,
        _request: sigil_kernel::managed_projection::OpenProjectionConnectionRequestV1,
    ) -> Result<
        Box<dyn sigil_kernel::managed_projection::ManagedProjectionConnectionV1>,
        sigil_kernel::managed_projection::ProjectionErrorV1,
    > {
        Err(sigil_kernel::managed_projection::ProjectionErrorV1::ConnectionClosed)
    }
}
