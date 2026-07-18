use std::collections::BTreeMap;

#[cfg(windows)]
use std::path::PathBuf;

use sigil_kernel::{ExecutionRequest, ProcessEnvironmentPolicy};

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

    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot should exist"))
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
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
        powershell.to_string_lossy().into_owned(),
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
