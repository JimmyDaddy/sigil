use std::collections::BTreeMap;

#[cfg(windows)]
use std::{fs, path::PathBuf, time::Duration};

use sigil_kernel::{ExecutionRequest, ProcessEnvironmentPolicy};

#[cfg(windows)]
use sigil_kernel::{
    ExecutionCleanupStatus, ExecutionTerminationCause, RunCancellationHandle, RunCancellationOwner,
};

#[cfg(windows)]
use serial_test::serial;

#[cfg(not(windows))]
use super::WindowsRestrictedProbeUnavailable;
use super::windows_restricted_launch_probe;

fn probe_request(program: String, args: Vec<String>) -> ExecutionRequest {
    ExecutionRequest {
        program,
        args,
        cwd: std::env::current_dir().expect("current directory should resolve"),
        env: BTreeMap::new(),
        environment_policy: ProcessEnvironmentPolicy::InheritParent,
        timeout_ms: Some(5_000),
        timeout_secs: 0,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    }
}

#[cfg(windows)]
fn powershell_path() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot should exist"))
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

#[cfg(windows)]
fn cmd_path() -> PathBuf {
    std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
}

#[cfg(windows)]
fn cmd_request(command: &str) -> ExecutionRequest {
    probe_request(
        cmd_path().to_string_lossy().into_owned(),
        vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            command.to_owned(),
        ],
    )
}

#[cfg(windows)]
fn configure_descendant_fixture(
    request: &mut ExecutionRequest,
    ready: &std::path::Path,
    survived: &std::path::Path,
) {
    request.env.insert(
        "SIGIL_RESTRICTED_DESCENDANT_FIXTURE".to_owned(),
        "1".to_owned(),
    );
    request.env.insert(
        "SIGIL_DESCENDANT_EXE".to_owned(),
        std::env::current_exe()
            .expect("current test executable should resolve")
            .to_string_lossy()
            .into_owned(),
    );
    request.env.insert(
        "SIGIL_DESCENDANT_READY".to_owned(),
        ready.to_string_lossy().into_owned(),
    );
    request.env.insert(
        "SIGIL_DESCENDANT_SURVIVED".to_owned(),
        survived.to_string_lossy().into_owned(),
    );
}

#[cfg(windows)]
fn descendant_fixture_command() -> &'static str {
    concat!(
        "start \"\" /B \"%SIGIL_DESCENDANT_EXE%\" --ignored --exact ",
        "execution_backends::windows_restricted::tests::restricted_descendant_fixture ",
        "--nocapture >nul 2>&1 & ping -n 30 127.0.0.1 >nul"
    )
}

#[cfg(windows)]
async fn supervise_native_probe(
    request: &ExecutionRequest,
    output_limits: super::super::OutputCollectionLimits,
    reader_fault: super::super::PreflightReaderFault,
    cancellation: Option<RunCancellationHandle>,
) -> anyhow::Result<super::super::SupervisedExecutionOutcome> {
    let child = super::NativeWindowsRestrictedChild::spawn(request)?;
    let deadline = super::super::execution_deadline(request)?;
    super::super::supervise_execution_child(
        super::super::SupervisedExecutionChild::WindowsRestricted(child),
        request,
        output_limits,
        reader_fault,
        None,
        deadline,
        cancellation,
    )
    .await
}

#[cfg(windows)]
async fn wait_for_file(path: &std::path::Path) -> anyhow::Result<()> {
    for _ in 0..800 {
        if path.is_file() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!(
        "timed out waiting for restricted child marker at {}",
        path.display()
    )
}

#[cfg(windows)]
fn assert_cleanup_completed(outcome: &super::super::SupervisedExecutionOutcome) {
    assert_eq!(
        outcome.resources.cleanup.status,
        ExecutionCleanupStatus::Completed,
        "forced termination must prove process-tree cleanup: {:?}",
        outcome.resources.cleanup
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn restricted_launch_probe_reports_typed_platform_unavailability() {
    let request = probe_request("unused".to_owned(), Vec::new());
    let error = windows_restricted_launch_probe(&request)
        .await
        .expect_err("non-Windows probe should be unavailable");
    let unavailable = error
        .downcast_ref::<WindowsRestrictedProbeUnavailable>()
        .expect("error should preserve the typed platform failure");

    assert_eq!(unavailable.platform(), std::env::consts::OS);
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_probe_captures_output_and_exit_status() {
    let command = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    let request = probe_request(
        command.to_string_lossy().into_owned(),
        vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            "(echo probe-out)&(echo probe-err 1>&2)&exit /b 7".to_owned(),
        ],
    );

    let receipt = windows_restricted_launch_probe(&request)
        .await
        .expect("restricted launch probe should run");

    assert!(receipt.privileges_constrained);
    assert_eq!(receipt.restricted_enabled_non_traverse_privilege_count, 0);
    assert!(
        receipt.source_enabled_non_traverse_privilege_count
            >= receipt.restricted_enabled_non_traverse_privilege_count
    );
    assert_eq!(receipt.exit_code, Some(7));
    assert!(String::from_utf8_lossy(&receipt.stdout).contains("probe-out"));
    assert!(String::from_utf8_lossy(&receipt.stderr).contains("probe-err"));
    assert!(!receipt.timed_out);
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_probe_preserves_unicode_environment_and_quoted_command() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let marker = temp.path().join("值 with spaces.txt");
    let mut request = cmd_request("rem Unicode 参数 & echo unicode-ok>\"%SIGIL_UNICODE_PATH%\"");
    request.env.insert(
        "SIGIL_UNICODE_PATH".to_owned(),
        marker.to_string_lossy().into_owned(),
    );

    let receipt = windows_restricted_launch_probe(&request)
        .await
        .expect("Unicode restricted launch should run");

    assert_eq!(receipt.exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(marker)
            .expect("Unicode environment path should be written")
            .trim(),
        "unicode-ok"
    );
    assert_eq!(
        receipt.environment_policy,
        ProcessEnvironmentPolicy::InheritParent
    );
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_probe_uses_exact_executable_path_with_spaces_and_unicode() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let copied_dir = temp.path().join("路径 with spaces");
    fs::create_dir(&copied_dir).expect("copied executable directory should be created");
    let copied_command = copied_dir.join("cmd copy.exe");
    fs::copy(cmd_path(), &copied_command).expect("cmd.exe should copy into the fixture path");
    let request = probe_request(
        copied_command.to_string_lossy().into_owned(),
        vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            "echo exact-path".to_owned(),
        ],
    );

    let receipt = windows_restricted_launch_probe(&request)
        .await
        .expect("copied executable should launch by exact path");

    assert_eq!(receipt.exit_code, Some(0));
    assert!(String::from_utf8_lossy(&receipt.stdout).contains("exact-path"));
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_probe_does_not_inherit_unlisted_handle() {
    use std::{fs::OpenOptions, os::windows::io::AsRawHandle};

    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

    let sentinel = tempfile::NamedTempFile::new().expect("sentinel file should be created");
    let sentinel_file = OpenOptions::new()
        .write(true)
        .open(sentinel.path())
        .expect("sentinel file should open for writing");
    let sentinel_handle = sentinel_file.as_raw_handle();
    // SAFETY: sentinel_file owns the live handle for the full launch and closes it on drop.
    assert_ne!(
        unsafe { SetHandleInformation(sentinel_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT,) },
        0,
        "sentinel handle should become inheritable"
    );

    let script = concat!(
        "$raw=[IntPtr]::new([long]$env:SIGIL_TEST_SENTINEL_HANDLE);",
        "$safe=[Microsoft.Win32.SafeHandles.SafeFileHandle]::new($raw,$false);",
        "try {",
        "$stream=[IO.FileStream]::new($safe,[IO.FileAccess]::Write);",
        "$bytes=[Text.Encoding]::UTF8.GetBytes('inherited');",
        "$stream.Write($bytes,0,$bytes.Length);$stream.Flush();exit 9",
        "} catch { exit 0 }"
    );
    let mut request = probe_request(
        powershell_path().to_string_lossy().into_owned(),
        vec![
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            script.to_owned(),
        ],
    );
    request.env.insert(
        "SIGIL_TEST_SENTINEL_HANDLE".to_owned(),
        (sentinel_handle as usize).to_string(),
    );

    let receipt = windows_restricted_launch_probe(&request)
        .await
        .expect("restricted launch probe should run");
    drop(sentinel_file);

    assert!(
        receipt.exit_code.is_some(),
        "handle canary child should reach a native terminal status"
    );
    assert_ne!(
        receipt.exit_code,
        Some(9),
        "exit 9 means the unlisted parent handle remained usable in the child"
    );
    assert_eq!(
        receipt.output.termination,
        sigil_kernel::ExecutionTerminationCause::Exited
    );
    assert!(
        std::fs::read(sentinel.path())
            .expect("sentinel should remain readable")
            .is_empty(),
        "unlisted inheritable handle must not reach the child"
    );
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_timeout_uses_shared_cleanup_receipt() {
    let mut request = cmd_request("ping -n 30 127.0.0.1 >nul");
    request.timeout_ms = Some(250);

    let outcome = supervise_native_probe(
        &request,
        super::super::OutputCollectionLimits::execution(),
        super::super::PreflightReaderFault::None,
        None,
    )
    .await
    .expect("timed restricted launch should return a receipt");

    assert_eq!(
        outcome.output.termination,
        ExecutionTerminationCause::TimedOut
    );
    assert!(outcome.timed_out);
    assert_cleanup_completed(&outcome);
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_cancellation_reaps_descendants() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let ready = temp.path().join("descendant-ready.txt");
    let survived = temp.path().join("descendant-survived.txt");
    let mut request = cmd_request(descendant_fixture_command());
    request.timeout_ms = Some(30_000);
    configure_descendant_fixture(&mut request, &ready, &survived);
    let owner = RunCancellationOwner::new();
    let cancellation = owner.handle();
    let task = tokio::spawn(async move {
        supervise_native_probe(
            &request,
            super::super::OutputCollectionLimits::execution(),
            super::super::PreflightReaderFault::None,
            Some(cancellation),
        )
        .await
    });

    wait_for_file(&ready)
        .await
        .expect("descendant should publish readiness before cancellation");
    assert!(owner.request_cancel());
    let outcome = task
        .await
        .expect("supervisor task should join")
        .expect("cancelled restricted launch should return a receipt");

    assert_eq!(
        outcome.output.termination,
        ExecutionTerminationCause::Cancelled
    );
    assert_cleanup_completed(&outcome);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !survived.exists(),
        "cancelled descendant escaped the Job Object"
    );
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_output_limit_uses_shared_bounded_collector() {
    let mut request =
        cmd_request("for /L %i in (1,1,40000) do @echo 1234567890 & ping -n 30 127.0.0.1 >nul");
    request.timeout_ms = Some(30_000);

    let outcome = supervise_native_probe(
        &request,
        super::super::OutputCollectionLimits::preflight(),
        super::super::PreflightReaderFault::None,
        None,
    )
    .await
    .expect("output-limited restricted launch should return a receipt");

    assert!(matches!(
        outcome.output.termination,
        ExecutionTerminationCause::OutputLimit { .. }
    ));
    assert!(outcome.output.stdout.total_bytes > 256 * 1024);
    assert_cleanup_completed(&outcome);
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_reader_failure_uses_shared_cleanup_path() {
    let mut request = cmd_request("echo x&ping -n 30 127.0.0.1 >nul");
    request.timeout_ms = Some(30_000);

    let outcome = supervise_native_probe(
        &request,
        super::super::OutputCollectionLimits::preflight(),
        super::super::PreflightReaderFault::PanicStdout,
        None,
    )
    .await
    .expect("reader-failed restricted launch should return a receipt");

    assert!(matches!(
        outcome.output.termination,
        ExecutionTerminationCause::ReaderFailed { .. }
    ));
    assert_cleanup_completed(&outcome);
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn dropping_restricted_supervisor_reaps_descendants() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let ready = temp.path().join("drop-ready.txt");
    let survived = temp.path().join("drop-survived.txt");
    let mut request = cmd_request(descendant_fixture_command());
    request.timeout_ms = Some(30_000);
    configure_descendant_fixture(&mut request, &ready, &survived);
    let task = tokio::spawn(async move {
        supervise_native_probe(
            &request,
            super::super::OutputCollectionLimits::execution(),
            super::super::PreflightReaderFault::None,
            None,
        )
        .await
    });

    wait_for_file(&ready)
        .await
        .expect("descendant should publish readiness before supervisor drop");
    task.abort();
    let join_error = match task.await {
        Err(error) => error,
        Ok(_) => panic!("aborted supervisor task should not complete normally"),
    };
    assert!(join_error.is_cancelled());
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !survived.exists(),
        "dropped supervisor left a descendant running"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "spawned only as a restricted descendant conformance fixture"]
fn restricted_descendant_fixture() {
    if std::env::var_os("SIGIL_RESTRICTED_DESCENDANT_FIXTURE").is_none() {
        return;
    }
    let ready = std::env::var_os("SIGIL_DESCENDANT_READY")
        .map(PathBuf::from)
        .expect("descendant ready marker path should be provided");
    let survived = std::env::var_os("SIGIL_DESCENDANT_SURVIVED")
        .map(PathBuf::from)
        .expect("descendant survived marker path should be provided");
    fs::write(ready, b"ready").expect("descendant ready marker should be written");
    std::thread::sleep(Duration::from_secs(2));
    fs::write(survived, b"survived").expect("descendant survived marker should be written");
}
