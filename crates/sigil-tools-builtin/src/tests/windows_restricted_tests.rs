use std::collections::BTreeMap;

#[cfg(windows)]
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use sigil_kernel::{ExecutionRequest, ProcessEnvironmentPolicy};

#[cfg(windows)]
use sigil_kernel::{
    ExecutionCleanupStatus, ExecutionTerminationCause, RunCancellationHandle, RunCancellationOwner,
};

#[cfg(windows)]
use serial_test::serial;
#[cfg(windows)]
use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

#[cfg(windows)]
const SECURITY_MANDATORY_LOW_RID: u32 = 0x1000;

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
const RESTRICTED_FIXTURE_TEST: &str =
    "execution_backends::windows_restricted::tests::restricted_process_fixture";

#[cfg(windows)]
fn fixture_request(mode: &str) -> ExecutionRequest {
    let mut request = probe_request(
        std::env::current_exe()
            .expect("current test executable should resolve")
            .to_string_lossy()
            .into_owned(),
        vec![
            "--ignored".to_owned(),
            "--exact".to_owned(),
            RESTRICTED_FIXTURE_TEST.to_owned(),
            "--nocapture".to_owned(),
        ],
    );
    request
        .env
        .insert("SIGIL_RESTRICTED_FIXTURE_MODE".to_owned(), mode.to_owned());
    request
}

#[cfg(windows)]
fn configure_descendant_fixture(
    request: &mut ExecutionRequest,
    ready: &std::path::Path,
    survived: &std::path::Path,
) {
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
async fn supervise_restricting_sid_probe(
    request: &ExecutionRequest,
    restricting_sid: &super::WindowsRestrictingSid,
) -> anyhow::Result<(super::super::SupervisedExecutionOutcome, u32)> {
    let child =
        super::NativeWindowsRestrictedChild::spawn_with_restricting_sid(request, restricting_sid)?;
    let restricting_sid_count = child.privilege_evidence().restricting_sid_count;
    let deadline = super::super::execution_deadline(request)?;
    let outcome = super::super::supervise_execution_child(
        super::super::SupervisedExecutionChild::WindowsRestricted(child),
        request,
        super::super::OutputCollectionLimits::execution(),
        super::super::PreflightReaderFault::None,
        None,
        deadline,
        None,
    )
    .await?;
    Ok((outcome, restricting_sid_count))
}

#[cfg(windows)]
async fn supervise_app_container_probe(
    request: &ExecutionRequest,
    app_container_sid: &super::WindowsAppContainerSid,
) -> anyhow::Result<(
    super::super::SupervisedExecutionOutcome,
    super::RestrictedTokenPrivilegeEvidence,
)> {
    let child =
        super::NativeWindowsRestrictedChild::spawn_with_app_container(request, app_container_sid)?;
    anyhow::ensure!(
        child.is_app_container(),
        "child token is not an AppContainer"
    );
    let evidence = child.privilege_evidence();
    let deadline = super::super::execution_deadline(request)?;
    let outcome = super::super::supervise_execution_child(
        super::super::SupervisedExecutionChild::WindowsRestricted(child),
        request,
        super::super::OutputCollectionLimits::execution(),
        super::super::PreflightReaderFault::None,
        None,
        deadline,
        None,
    )
    .await?;
    Ok((outcome, evidence))
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
    assert_eq!(receipt.restricting_sid_count, 0);
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
async fn write_restricted_sid_initializes_runtime_and_denies_ungranted_same_user_path() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let host_can_write = temp.path().join("host-can-write.txt");
    fs::write(&host_can_write, b"host")
        .expect("the unrestricted test process should be able to write the fixture root");
    let denied_target = temp.path().join("restricted-write-must-fail.txt");
    let mut request = fixture_request("deny-write");
    request.env.insert(
        "SIGIL_RESTRICTED_DENIED_PATH".to_owned(),
        denied_target.to_string_lossy().into_owned(),
    );
    let restricting_sid =
        super::WindowsRestrictingSid::new_unique().expect("unique restricting SID should resolve");

    let (outcome, restricting_sid_count) =
        supervise_restricting_sid_probe(&request, &restricting_sid)
            .await
            .expect("write-restricted probe should return a receipt");

    assert_eq!(
        restricting_sid_count, 3,
        "token should carry the workspace, logon, and Everyone compatibility restricting SIDs"
    );
    assert_eq!(
        outcome.exit_code,
        Some(0),
        "restricted Rust runtime should initialize before proving the denied write"
    );
    assert_eq!(
        outcome.output.termination,
        ExecutionTerminationCause::Exited
    );
    assert!(
        !denied_target.exists(),
        "ungranted same-user path became writable under the restricting SID"
    );
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn app_container_profile_launches_system_command_and_cleans_up() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let profile =
        super::WindowsAppContainerProfile::create_private_probe(&temp.path().join("profile-state"))
            .expect("private AppContainer profile should be created with a durable owner journal");
    assert!(profile.journal_path().is_file());
    let profile_name = profile.name().to_owned();
    let program = cmd_path();
    let mut request = probe_request(
        program.to_string_lossy().into_owned(),
        vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            "echo appcontainer-ok".to_owned(),
        ],
    );
    request.cwd = program
        .parent()
        .expect("cmd.exe should have a parent directory")
        .to_path_buf();

    let (outcome, evidence) = supervise_app_container_probe(&request, profile.sid())
        .await
        .expect("profile-backed AppContainer launch should return a receipt");

    assert!(evidence.privileges_constrained());
    assert_eq!(evidence.integrity_rid, SECURITY_MANDATORY_LOW_RID);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(
        outcome.output.termination,
        ExecutionTerminationCause::Exited
    );
    assert!(String::from_utf8_lossy(&outcome.stdout).contains("appcontainer-ok"));
    let journal_path = profile.journal_path().to_path_buf();
    profile
        .close()
        .expect("private AppContainer profile should delete after the child is reaped");
    assert!(!journal_path.exists());

    let deleted_sid = super::WindowsAppContainerSid::derive_named(&profile_name)
        .expect("deleted profile SID should remain derivable by name");
    let launch_error =
        match super::NativeWindowsRestrictedChild::spawn_with_app_container(&request, &deleted_sid)
        {
            Ok(_) => panic!("a deleted AppContainer profile must not remain launchable"),
            Err(error) => error,
        };
    assert!(
        launch_error
            .to_string()
            .contains("CreateProcessW failed for AppContainer")
    );
}

#[cfg(windows)]
#[serial]
#[test]
fn app_container_profile_recovers_an_interrupted_owned_profile() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let state_dir = temp.path().join("profile-state");
    let abandoned = super::WindowsAppContainerProfile::create_private_probe(&state_dir)
        .expect("first private AppContainer profile should be created");
    let abandoned_name = abandoned
        .abandon_for_recovery()
        .expect("test owner should release only the cross-process lease");

    let recovered = super::WindowsAppContainerProfile::create_private_probe(&state_dir)
        .expect("next owner should delete the interrupted profile before creating a new one");
    assert_ne!(recovered.name(), abandoned_name);
    let abandoned_sid = super::WindowsAppContainerSid::derive_named(&abandoned_name)
        .expect("abandoned profile SID should remain derivable by name");
    let program = cmd_path();
    let mut request = probe_request(
        program.to_string_lossy().into_owned(),
        vec!["/D".to_owned(), "/C".to_owned(), "exit 0".to_owned()],
    );
    request.cwd = program
        .parent()
        .expect("cmd.exe should have a parent directory")
        .to_path_buf();
    if super::NativeWindowsRestrictedChild::spawn_with_app_container(&request, &abandoned_sid)
        .is_ok()
    {
        panic!("recovery left the interrupted AppContainer profile launchable");
    }
    recovered
        .close()
        .expect("recovery probe profile should delete cleanly");
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn app_container_dacl_grant_cannot_write_medium_integrity_workspace() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let existing = workspace.join("existing.txt");
    fs::write(&existing, b"before").expect("existing workspace file should be created");

    let profile =
        super::WindowsAppContainerProfile::create_private_probe(&temp.path().join("profile-state"))
            .expect("private AppContainer profile should be created");
    let app_container_sid = profile
        .sid()
        .as_restricting_sid()
        .expect("AppContainer SID should convert to an ACL principal");
    let grant = super::WindowsFilesystemGrant::acquire_for_sid(
        &workspace,
        &temp.path().join("acl-state"),
        &app_container_sid,
    )
    .expect("AppContainer workspace grant should be provisioned");
    let root_descriptor = super::WindowsFilesystemGrant::descriptor_hash(&workspace)
        .expect("workspace descriptor should be captured");
    let existing_descriptor = super::WindowsFilesystemGrant::descriptor_hash(&existing)
        .expect("existing file descriptor should be captured");
    assert!(
        super::WindowsFilesystemGrant::sid_has_mutating_rights(&workspace, grant.restricting_sid())
            .expect("AppContainer root DACL rights should resolve")
    );
    assert!(
        super::WindowsFilesystemGrant::sid_has_mutating_rights(&existing, grant.restricting_sid())
            .expect("AppContainer existing-file DACL rights should resolve")
    );

    let program = cmd_path();
    let mut inside = probe_request(
        program.to_string_lossy().into_owned(),
        vec![
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            "echo created>created.txt && echo modified>existing.txt && echo deleted>deleted.txt && del /Q deleted.txt"
                .to_owned(),
        ],
    );
    inside.cwd = workspace.clone();
    let (inside_outcome, evidence) = supervise_app_container_probe(&inside, profile.sid())
        .await
        .expect("AppContainer workspace mutation probe should return a receipt");
    assert_eq!(evidence.integrity_rid, SECURITY_MANDATORY_LOW_RID);
    assert_ne!(
        inside_outcome.exit_code,
        Some(0),
        "DACL-only AppContainer mutation unexpectedly succeeded; stdout={:?}; stderr={:?}",
        String::from_utf8_lossy(&inside_outcome.stdout),
        String::from_utf8_lossy(&inside_outcome.stderr)
    );
    assert_eq!(
        fs::read(&existing).expect("existing workspace file should remain readable"),
        b"before"
    );
    assert!(!workspace.join("created.txt").exists());
    assert!(!workspace.join("deleted.txt").exists());

    grant
        .release()
        .expect("AppContainer workspace grant should revalidate on release");
    let second_grant = super::WindowsFilesystemGrant::acquire_for_sid(
        &workspace,
        &temp.path().join("acl-state"),
        &app_container_sid,
    )
    .expect("AppContainer workspace grant should revalidate without ACL mutation");
    assert_eq!(
        super::WindowsFilesystemGrant::descriptor_hash(&workspace)
            .expect("revalidated workspace descriptor should be captured"),
        root_descriptor
    );
    assert_eq!(
        super::WindowsFilesystemGrant::descriptor_hash(&existing)
            .expect("revalidated existing-file descriptor should be captured"),
        existing_descriptor
    );
    assert!(
        !super::WindowsFilesystemGrant::sid_has_security_control_rights(
            &existing,
            second_grant.restricting_sid(),
        )
        .expect("AppContainer security-control rights should resolve")
    );
    second_grant
        .release()
        .expect("revalidated AppContainer grant should release cleanly");
    profile
        .close()
        .expect("AppContainer profile should delete after the filesystem probes");
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn write_restricted_sid_everyone_compatibility_is_not_filesystem_isolation() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let broad_root = temp.path().join("everyone-writable");
    fs::create_dir_all(&broad_root).expect("Everyone-writable root should be created");
    let everyone_sid =
        super::WindowsRestrictingSid::new_everyone().expect("Everyone SID should resolve");
    super::WindowsFilesystemGrant::apply_test_root_grant(&broad_root, &everyone_sid)
        .expect("negative fixture should grant Everyone write access");
    let external = broad_root.join("escaped.txt");
    let mut request = fixture_request("expect-write");
    request.env.insert(
        "SIGIL_RESTRICTED_EXPECTED_WRITE".to_owned(),
        external.to_string_lossy().into_owned(),
    );
    let workspace_sid =
        super::WindowsRestrictingSid::new_unique().expect("workspace SID should resolve");

    let (outcome, restricting_sid_count) =
        supervise_restricting_sid_probe(&request, &workspace_sid)
            .await
            .expect("Everyone compatibility probe should return a receipt");

    assert_eq!(restricting_sid_count, 3);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(
        fs::read(&external).expect("Everyone-granted external write should be observable"),
        b"written"
    );
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn write_restricted_sid_durable_grant_is_stable_across_runs() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let granted_root = temp.path().join("workspace");
    let denied_root = temp.path().join("sibling");
    let state_dir = temp.path().join("acl-state");
    fs::create_dir_all(&granted_root).expect("granted root should be created");
    fs::create_dir_all(&denied_root).expect("denied root should be created");
    let existing = granted_root.join("existing.txt");
    fs::write(&existing, b"before").expect("existing workspace file should be created");
    let denied = denied_root.join("escape.txt");
    let grant = super::WindowsFilesystemGrant::acquire(&granted_root, &state_dir)
        .expect("minimal durable workspace grant should be provisioned");
    let renamed_root = temp.path().join("renamed-workspace");
    let rename_error = fs::rename(&granted_root, &renamed_root)
        .expect_err("an active grant lease should pin the workspace root identity");
    assert_eq!(
        rename_error.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION.cast_signed())
    );
    let restricting_sid_value = grant.restricting_sid().as_str().to_owned();
    let root_descriptor_after_provision =
        super::WindowsFilesystemGrant::descriptor_hash(&granted_root)
            .expect("provisioned root descriptor should be captured");
    let existing_descriptor_after_provision =
        super::WindowsFilesystemGrant::descriptor_hash(&existing)
            .expect("provisioned existing-file descriptor should be captured");
    let mut request = fixture_request("filesystem-grant");
    request.env.insert(
        "SIGIL_GRANTED_ROOT".to_owned(),
        granted_root.to_string_lossy().into_owned(),
    );
    request.env.insert(
        "SIGIL_GRANTED_EXISTING".to_owned(),
        existing.to_string_lossy().into_owned(),
    );
    request.env.insert(
        "SIGIL_RESTRICTED_DENIED_PATH".to_owned(),
        denied.to_string_lossy().into_owned(),
    );
    let run_result = supervise_restricting_sid_probe(&request, grant.restricting_sid()).await;
    let (outcome, restricting_sid_count) =
        run_result.expect("filesystem containment fixture should return a receipt");
    grant
        .release()
        .expect("durable workspace grant should revalidate before releasing its lease");

    assert_eq!(restricting_sid_count, 3);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(&existing).expect("existing file should remain readable"),
        "modified"
    );
    assert_eq!(
        fs::read_to_string(granted_root.join("created.txt"))
            .expect("new workspace file should remain readable"),
        "created"
    );
    let second_grant = super::WindowsFilesystemGrant::acquire(&granted_root, &state_dir)
        .expect("durable workspace grant should revalidate without ACL mutation");
    assert_eq!(
        second_grant.restricting_sid().as_str(),
        restricting_sid_value
    );
    assert!(
        super::WindowsFilesystemGrant::sid_has_mutating_rights(
            &existing,
            second_grant.restricting_sid(),
        )
        .expect("existing file effective rights should resolve"),
        "existing child lost the durable workspace SID's mutating rights"
    );
    assert_eq!(
        super::WindowsFilesystemGrant::descriptor_hash(&existing)
            .expect("existing file descriptor should remain readable"),
        existing_descriptor_after_provision,
        "ordinary restricted execution rewrote an existing child descriptor"
    );
    assert_eq!(
        super::WindowsFilesystemGrant::descriptor_hash(&granted_root)
            .expect("revalidated root descriptor should be captured"),
        root_descriptor_after_provision,
        "durable grant revalidation rewrote the root descriptor"
    );
    assert_eq!(
        super::WindowsFilesystemGrant::descriptor_hash(&existing)
            .expect("revalidated existing-file descriptor should be captured"),
        existing_descriptor_after_provision,
        "durable grant revalidation rewrote an existing child descriptor"
    );
    assert!(
        super::WindowsFilesystemGrant::sid_has_mutating_rights(
            &granted_root.join("created.txt"),
            second_grant.restricting_sid(),
        )
        .expect("created file effective rights should resolve"),
        "new child did not inherit the durable workspace SID's mutating rights"
    );
    assert!(
        !super::WindowsFilesystemGrant::sid_has_security_control_rights(
            &granted_root.join("created.txt"),
            second_grant.restricting_sid(),
        )
        .expect("created file security-control rights should resolve"),
        "new child inherited forbidden ACL or owner mutation rights"
    );
    let created_descriptor_before_second_run =
        super::WindowsFilesystemGrant::descriptor_hash(&granted_root.join("created.txt"))
            .expect("created-file descriptor should be captured before the second run");
    let (second_outcome, second_restricting_sid_count) =
        supervise_restricting_sid_probe(&request, second_grant.restricting_sid())
            .await
            .expect("second filesystem containment run should return a receipt");
    assert_eq!(second_restricting_sid_count, 3);
    assert_eq!(second_outcome.exit_code, Some(0));
    assert_eq!(
        super::WindowsFilesystemGrant::descriptor_hash(&granted_root.join("created.txt"))
            .expect("created-file descriptor should be captured after the second run"),
        created_descriptor_before_second_run,
        "a repeated restricted run rewrote the durable child descriptor"
    );
    second_grant
        .release()
        .expect("revalidated durable workspace lease should release cleanly");
    assert!(!granted_root.join("deleted.txt").exists());
    assert!(!denied.exists(), "sibling path escaped the root grant");
}

#[cfg(windows)]
#[serial]
#[tokio::test]
async fn restricted_launch_probe_preserves_unicode_environment_and_path() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let marker = temp.path().join("值 with spaces.txt");
    let mut request = fixture_request("unicode-environment");
    request.env.insert(
        "SIGIL_UNICODE_PATH".to_owned(),
        marker.to_string_lossy().into_owned(),
    );
    request.env.insert(
        "SIGIL_UNICODE_VALUE".to_owned(),
        "值 空格 \"quoted\" 尾斜杠\\".to_owned(),
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
    let mut request = fixture_request("sleep");
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
    let mut request = fixture_request("descendant-parent");
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
    let mut request = fixture_request("output-limit");
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
    let mut request = fixture_request("reader-failure");
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
    let mut request = fixture_request("descendant-parent");
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
#[ignore = "spawned only as a native Windows restricted-launch conformance fixture"]
fn restricted_process_fixture() {
    let mode = std::env::var("SIGIL_RESTRICTED_FIXTURE_MODE")
        .expect("restricted fixture mode should be provided");
    match mode.as_str() {
        "unicode-environment" => {
            assert_eq!(
                std::env::var("SIGIL_UNICODE_VALUE")
                    .expect("Unicode fixture value should be provided"),
                "值 空格 \"quoted\" 尾斜杠\\"
            );
            let marker = std::env::var_os("SIGIL_UNICODE_PATH")
                .map(PathBuf::from)
                .expect("Unicode marker path should be provided");
            fs::write(marker, b"unicode-ok").expect("Unicode marker should be written");
        }
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "output-limit" => {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(&vec![b'x'; 300_000])
                .expect("fixture output should be written");
            stdout.flush().expect("fixture output should flush");
            std::thread::sleep(Duration::from_secs(30));
        }
        "reader-failure" => {
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(b"x")
                .expect("fixture output should be written");
            stdout.flush().expect("fixture output should flush");
            std::thread::sleep(Duration::from_secs(30));
        }
        "deny-write" => {
            let denied_path = std::env::var_os("SIGIL_RESTRICTED_DENIED_PATH")
                .map(PathBuf::from)
                .expect("denied write path should be provided");
            let error = fs::write(denied_path, b"escaped")
                .expect_err("write-restricted token unexpectedly wrote an ungranted path");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        "expect-write" => {
            let target = std::env::var_os("SIGIL_RESTRICTED_EXPECTED_WRITE")
                .map(PathBuf::from)
                .expect("expected write path should be provided");
            fs::write(target, b"written").expect("compatibility SID should permit this write");
        }
        "filesystem-grant" => {
            let root = std::env::var_os("SIGIL_GRANTED_ROOT")
                .map(PathBuf::from)
                .expect("granted root should be provided");
            let existing = std::env::var_os("SIGIL_GRANTED_EXISTING")
                .map(PathBuf::from)
                .expect("existing granted file should be provided");
            let denied_path = std::env::var_os("SIGIL_RESTRICTED_DENIED_PATH")
                .map(PathBuf::from)
                .expect("denied write path should be provided");
            fs::write(existing, b"modified").expect("granted existing file should be modified");
            fs::write(root.join("created.txt"), b"created")
                .expect("file should be created in granted root");
            let deleted = root.join("deleted.txt");
            fs::write(&deleted, b"delete").expect("file should be created before delete");
            fs::remove_file(deleted).expect("file should be deleted in granted root");
            let error = fs::write(denied_path, b"escaped")
                .expect_err("write-restricted token unexpectedly wrote outside granted root");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        "descendant-parent" => {
            let mut descendant =
                Command::new(std::env::current_exe().expect("fixture executable should resolve"))
                    .args([
                        "--ignored",
                        "--exact",
                        RESTRICTED_FIXTURE_TEST,
                        "--nocapture",
                    ])
                    .env("SIGIL_RESTRICTED_FIXTURE_MODE", "descendant-child")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("restricted descendant should spawn");
            let status = descendant
                .wait()
                .expect("restricted descendant should remain waitable");
            assert!(status.success(), "restricted descendant should succeed");
        }
        "descendant-child" => {
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
        other => panic!("unknown restricted fixture mode: {other}"),
    }
}
