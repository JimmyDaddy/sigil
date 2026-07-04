use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, atomic::AtomicBool},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

use anyhow::{Result, anyhow};
use sigil_kernel::{
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupStatus, ExecutionConfig,
    ExecutionSandboxFallback, ExecutionSandboxProfile, ExecutionSandboxStrategyConfig,
    TerminalExecutionBackendCapabilities, TerminalExecutionBackendKind, TerminalTaskEntry,
    TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus,
};
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    process::Command,
    sync::{Mutex, mpsc},
    time::sleep,
};

use super::{
    TerminalBackendKind, TerminalExecutionConfig, TerminalProcessManager, TerminalPtySize,
    TerminalStartRequest,
};
use serial_test::serial;

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

#[test]
fn terminal_process_manager_permission_context_reports_missing_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let error = manager
        .permission_context(&TerminalTaskId::new("missing-task")?)
        .expect_err("missing task should report unavailable context");

    assert!(
        error
            .to_string()
            .contains("terminal task permission context is unavailable")
    );
    Ok(())
}

#[test]
fn terminal_process_artifact_path_guards_cover_labels_and_relative_roots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new_with_artifact_root(
        temp.path(),
        PathBuf::from("relative-artifacts"),
        PathBuf::from("labels/tasks"),
    )?;
    assert!(
        manager
            .artifacts_for(&TerminalTaskId::new("terminal-relative")?)?
            .absolute_dir
            .starts_with(temp.path().canonicalize()?.join("relative-artifacts"))
    );

    let unknown_label = manager
        .stored_artifact_path(Path::new("other/tasks/output.log"))
        .expect_err("unknown artifact label should be rejected");
    assert!(unknown_label.to_string().contains("unknown label"));

    #[cfg(unix)]
    {
        let outside = tempfile::tempdir()?;
        let artifact_root = temp.path().join("artifacts");
        std::fs::create_dir_all(&artifact_root)?;
        symlink(outside.path(), artifact_root.join("leak"))?;
        let escaping = TerminalProcessManager::new_with_artifact_root(
            temp.path(),
            &artifact_root,
            PathBuf::from("state/artifacts/tasks"),
        )?;
        let error = escaping
            .stored_artifact_path(Path::new("state/artifacts/tasks/leak/output.log"))
            .expect_err("symlink escape should be rejected");
        assert!(error.to_string().contains("outside artifact root"));
    }
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_start_read_and_status_writes_artifacts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?.with_preview_limit_bytes(256);
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-1")?),
            command: "printf 'out\\n'; printf 'err\\n' >&2".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;

    assert!(matches!(entry.status, TerminalTaskStatus::Running));
    assert_eq!(
        entry.handle.execution_backend,
        Some(TerminalExecutionBackendKind::LocalProcess)
    );
    assert_eq!(
        entry.handle.execution_backend_capabilities,
        Some(TerminalExecutionBackendCapabilities::local_process())
    );
    assert_eq!(
        entry.handle.enforcement_backend,
        Some(ExecutionBackendKind::Local)
    );
    assert_eq!(
        entry.handle.enforcement_backend_capabilities,
        Some(ExecutionBackendCapabilities::default())
    );
    assert_eq!(
        entry.handle.sandbox_profile,
        Some(ExecutionSandboxProfile::Unconfined)
    );
    assert!(entry.cleanup.is_none());
    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;
    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert_eq!(
        final_entry.handle.execution_backend,
        Some(TerminalExecutionBackendKind::LocalProcess)
    );
    assert_eq!(
        final_entry.handle.log_path,
        PathBuf::from("state/artifacts/tasks/terminal-1/output.log")
    );
    assert!(!final_entry.output_truncated);
    assert!(final_entry.output_hash.is_some());
    assert_eq!(
        final_entry
            .cleanup
            .as_ref()
            .expect("terminal exit should record cleanup")
            .status,
        ExecutionCleanupStatus::NotNeeded
    );
    let preview = final_entry.output_preview.as_deref().unwrap_or_default();
    assert!(preview.contains("out"));
    assert!(preview.contains("err"));

    let artifacts = manager.artifacts_for(&entry.handle.task_id)?;
    assert!(artifacts.absolute_meta.exists());
    assert!(artifacts.absolute_output.exists());
    assert!(artifacts.absolute_stdout.exists());
    assert!(artifacts.absolute_stderr.exists());
    assert_eq!(std::fs::read_to_string(artifacts.absolute_stdout)?, "out\n");
    assert!(std::fs::read_to_string(artifacts.absolute_stderr)?.contains("err"));

    let read = manager.read(&entry.handle.task_id, 0, 1024).await?;
    assert!(read.content.contains("out"));
    assert!(read.content.contains("err"));
    assert!(!read.truncated);
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_read_is_bounded_by_offset_and_limit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-read")?),
            command: "printf abcdef".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;
    wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    let first = manager.read(&entry.handle.task_id, 0, 3).await?;
    let second = manager
        .read(
            &entry.handle.task_id,
            first.next_offset.expect("next offset"),
            3,
        )
        .await?;

    assert_eq!(first.content, "abc");
    assert_eq!(first.next_offset, Some(3));
    assert!(first.truncated);
    assert_eq!(second.content, "def");
    assert_eq!(second.next_offset, None);
    assert!(!second.truncated);
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_cancel_marks_running_task_cancelled() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager =
        TerminalProcessManager::new(temp.path())?.with_cancel_grace(Duration::from_millis(50));
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-cancel")?),
            command: "sleep 5".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;

    let resize_error = manager
        .resize(&entry.handle.task_id, TerminalPtySize::new(10, 40)?)
        .await
        .expect_err("non-PTY process backend should reject resize");
    assert!(resize_error.to_string().contains("does not support resize"));

    let cancelled = manager.cancel(&entry.handle.task_id).await?;

    assert!(matches!(cancelled.status, TerminalTaskStatus::Cancelled));
    assert_eq!(
        cancelled
            .cleanup
            .as_ref()
            .expect("terminal cancel should record cleanup")
            .status,
        ExecutionCleanupStatus::Completed
    );
    assert!(cancelled.status.is_terminal());
    let status = manager.status(&entry.handle.task_id).await?;
    assert!(matches!(status.status, TerminalTaskStatus::Cancelled));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_rejects_empty_command_and_workspace_escape() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;

    let empty_error = manager
        .start(TerminalStartRequest::new("   "))
        .await
        .expect_err("empty command should be rejected");
    assert!(empty_error.to_string().contains("cannot be empty"));

    let escape_error = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-escape")?),
            command: "pwd".to_owned(),
            cwd: Some(PathBuf::from("..")),
            shell: None,
            env: Default::default(),
        })
        .await
        .expect_err("workspace escape should be rejected");
    assert!(escape_error.to_string().contains("outside workspace"));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_rejects_duplicate_task_ids() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let task_id = TerminalTaskId::new("terminal-duplicate")?;
    manager
        .start(TerminalStartRequest {
            task_id: Some(task_id.clone()),
            command: "sleep 1".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;

    let error = manager
        .start(TerminalStartRequest {
            task_id: Some(task_id.clone()),
            command: "pwd".to_owned(),
            cwd: None,
            shell: None,
            env: Default::default(),
        })
        .await
        .expect_err("duplicate task id should be rejected");
    assert!(error.to_string().contains("already exists"));
    manager.cancel(&task_id).await?;
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_generates_ids_and_accepts_absolute_workspace_cwd() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let subdir = temp.path().join("subdir");
    std::fs::create_dir(&subdir)?;
    let manager = TerminalProcessManager::new(temp.path())?;

    let entry = manager
        .start(TerminalStartRequest {
            task_id: None,
            command: "pwd".to_owned(),
            cwd: Some(subdir.clone()),
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;
    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    assert!(entry.handle.task_id.as_str().starts_with("terminal-"));
    assert_eq!(entry.handle.cwd, PathBuf::from("subdir"));
    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    let read = manager.read(&entry.handle.task_id, 0, 1024).await?;
    assert!(read.content.contains(&subdir.display().to_string()));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_preview_and_reads_use_bounded_offsets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?.with_preview_limit_bytes(4);
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-truncate")?),
            command: "printf 0123456789".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;
    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    assert!(final_entry.output_truncated);
    assert!(
        final_entry
            .output_preview
            .as_deref()
            .unwrap_or_default()
            .contains("truncated")
    );

    let one_byte = manager.read(&entry.handle.task_id, 0, 0).await?;
    let past_end = manager.read(&entry.handle.task_id, 99, 10).await?;
    assert_eq!(one_byte.returned_bytes, 1);
    assert!(one_byte.truncated);
    assert_eq!(past_end.offset, past_end.total_bytes);
    assert_eq!(past_end.returned_bytes, 0);
    assert!(!past_end.truncated);
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_cancel_after_exit_returns_current_status() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-done")?),
            command: "printf done".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;
    wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    let cancel_result = manager.cancel(&entry.handle.task_id).await?;

    assert!(matches!(
        cancel_result.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    Ok(())
}

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_process_manager_pty_records_context_and_env() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let task_id = TerminalTaskId::new("terminal-pty-env")?;
    let entry = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(task_id.clone()),
                command: "printf 'env:%s' \"$SIGIL_TEST_ENV\"".to_owned(),
                cwd: None,
                shell: Some(shell),
                env: BTreeMap::from([("SIGIL_TEST_ENV".to_owned(), "ok".to_owned())]),
            },
            None,
        )
        .await?;

    let context = manager.permission_context(&task_id)?;
    assert_eq!(context.task_id, task_id);
    assert_eq!(context.command, "printf 'env:%s' \"$SIGIL_TEST_ENV\"");
    assert_eq!(
        entry.handle.execution_backend,
        Some(TerminalExecutionBackendKind::LocalPty)
    );
    assert_eq!(
        entry.handle.execution_backend_capabilities,
        Some(TerminalExecutionBackendCapabilities::local_pty())
    );
    assert_eq!(
        entry.handle.enforcement_backend,
        Some(ExecutionBackendKind::Local)
    );
    assert_eq!(
        entry.handle.sandbox_profile,
        Some(ExecutionSandboxProfile::Unconfined)
    );
    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;
    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert_eq!(
        final_entry
            .cleanup
            .as_ref()
            .expect("terminal exit should record cleanup")
            .status,
        ExecutionCleanupStatus::NotNeeded
    );
    let read = manager.read(&entry.handle.task_id, 0, 1024).await?;
    assert!(read.content.contains("env:ok"));
    Ok(())
}

#[cfg(target_os = "macos")]
#[serial]
#[tokio::test]
async fn terminal_process_manager_macos_seatbelt_pty_records_sandbox_and_denies_external_write()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    let outside_path = outside.path().join("denied.txt");
    let shell = "/bin/sh".to_owned();
    let execution_config = sandbox_execution_config(
        ExecutionBackendKind::MacosSeatbelt,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    );
    let manager = TerminalProcessManager::new_with_artifact_root_and_terminal_execution(
        temp.path(),
        temp.path().join("terminal-artifacts"),
        PathBuf::from("terminal-artifacts"),
        TerminalExecutionConfig::from_execution_config(&execution_config),
    )?;
    let entry = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(TerminalTaskId::new("terminal-sandboxed-pty")?),
                command: concat!(
                    "printf ok > allowed.txt; ",
                    "if printf nope > \"$OUTSIDE_PATH\" 2>/dev/null; ",
                    "then echo external-write-unexpected; exit 7; ",
                    "else echo external-write-blocked; fi"
                )
                .to_owned(),
                cwd: None,
                shell: Some(shell),
                env: BTreeMap::from([(
                    "OUTSIDE_PATH".to_owned(),
                    outside_path.to_string_lossy().into_owned(),
                )]),
            },
            None,
        )
        .await?;

    assert_eq!(
        entry.handle.execution_backend,
        Some(TerminalExecutionBackendKind::SandboxedPty)
    );
    assert_eq!(
        entry.handle.execution_backend_capabilities,
        Some(TerminalExecutionBackendCapabilities::sandboxed_pty())
    );
    assert_eq!(
        entry.handle.enforcement_backend,
        Some(ExecutionBackendKind::MacosSeatbelt)
    );
    let enforcement = entry
        .handle
        .enforcement_backend_capabilities
        .expect("sandboxed pty should record enforcement capabilities");
    assert!(enforcement.filesystem_isolation);
    assert!(enforcement.process_isolation);
    assert!(enforcement.persistent_pty);
    assert_eq!(
        entry.handle.sandbox_profile,
        Some(ExecutionSandboxProfile::WorkspaceWrite)
    );

    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;
    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("allowed.txt"))?,
        "ok"
    );
    assert!(!outside_path.exists());
    let read = manager.read(&entry.handle.task_id, 0, 2048).await?;
    assert!(read.content.contains("external-write-blocked"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[serial]
#[ignore = "requires Linux host with bubblewrap user/mount namespaces"]
#[tokio::test]
async fn terminal_process_manager_linux_bubblewrap_pty_records_sandbox_and_denies_external_write()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir_in("/var/tmp")?;
    let outside_path = outside.path().join("denied.txt");
    let execution_config = sandbox_execution_config(
        ExecutionBackendKind::LinuxBubblewrap,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    );
    let manager = TerminalProcessManager::new_with_artifact_root_and_terminal_execution(
        temp.path(),
        temp.path().join("terminal-artifacts"),
        PathBuf::from("terminal-artifacts"),
        TerminalExecutionConfig::from_execution_config(&execution_config),
    )?;
    let entry = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(TerminalTaskId::new("terminal-bubblewrap-pty")?),
                command: concat!(
                    "printf ok > allowed.txt; ",
                    "if printf nope > \"$OUTSIDE_PATH\" 2>/dev/null; ",
                    "then echo external-write-unexpected; exit 7; ",
                    "else echo external-write-blocked; fi"
                )
                .to_owned(),
                cwd: None,
                shell: Some(test_shell(temp.path())?),
                env: BTreeMap::from([(
                    "OUTSIDE_PATH".to_owned(),
                    outside_path.to_string_lossy().into_owned(),
                )]),
            },
            None,
        )
        .await?;

    assert_eq!(
        entry.handle.execution_backend,
        Some(TerminalExecutionBackendKind::SandboxedPty)
    );
    assert_eq!(
        entry.handle.execution_backend_capabilities,
        Some(TerminalExecutionBackendCapabilities::sandboxed_pty())
    );
    assert_eq!(
        entry.handle.enforcement_backend,
        Some(ExecutionBackendKind::LinuxBubblewrap)
    );
    let enforcement = entry
        .handle
        .enforcement_backend_capabilities
        .expect("sandboxed pty should record enforcement capabilities");
    assert!(enforcement.filesystem_isolation);
    assert!(enforcement.network_isolation);
    assert!(enforcement.process_isolation);
    assert!(enforcement.persistent_pty);
    assert_eq!(
        entry.handle.sandbox_profile,
        Some(ExecutionSandboxProfile::WorkspaceWrite)
    );

    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;
    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("allowed.txt"))?,
        "ok"
    );
    assert!(!outside_path.exists());
    let read = manager.read(&entry.handle.task_id, 0, 2048).await?;
    assert!(read.content.contains("external-write-blocked"));
    Ok(())
}

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_process_manager_docker_pty_fails_closed_without_local_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let execution_config = sandbox_execution_config(
        ExecutionBackendKind::Docker,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Unconfined,
        Some("sigil-test:latest".to_owned()),
    );
    let manager = TerminalProcessManager::new_with_artifact_root_and_terminal_execution(
        temp.path(),
        temp.path().join("terminal-artifacts"),
        PathBuf::from("terminal-artifacts"),
        TerminalExecutionConfig::from_execution_config(&execution_config),
    )?;

    let error = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(TerminalTaskId::new("terminal-docker-pty")?),
                command: "printf should-not-run".to_owned(),
                cwd: None,
                shell: Some(test_shell(temp.path())?),
                env: Default::default(),
            },
            None,
        )
        .await
        .expect_err("docker pty should fail closed instead of falling back to local pty");

    assert!(
        error
            .to_string()
            .contains("docker execution backend does not support persistent terminal pty")
    );
    assert!(
        error
            .to_string()
            .contains("unconfined fallback is not used for terminal pty tasks")
    );
    Ok(())
}

#[cfg(unix)]
#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_pty_accepts_input_resize_and_writes_combined_artifacts()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?.with_preview_limit_bytes(512);
    let entry = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(TerminalTaskId::new("terminal-pty")?),
                command: "trap '' WINCH; IFS= read -r line; printf 'got:%s\\n' \"$line\""
                    .to_owned(),
                cwd: None,
                shell: Some(shell),
                env: Default::default(),
            },
            Some(TerminalPtySize::new(12, 50)?),
        )
        .await?;

    let resized = manager
        .resize(&entry.handle.task_id, TerminalPtySize::new(18, 72)?)
        .await?;
    assert_eq!(resized.size.rows, 18);
    assert_eq!(resized.size.cols, 72);

    let oversize = manager
        .input(
            &entry.handle.task_id,
            "x".repeat(super::MAX_TERMINAL_INPUT_BYTES + 1),
        )
        .await
        .expect_err("oversized terminal input should be rejected");
    assert!(oversize.to_string().contains("exceeds maximum"));

    let input = manager.input(&entry.handle.task_id, "hello-pty\n").await?;
    assert_eq!(input.input_bytes, 10);
    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    let read = manager.read(&entry.handle.task_id, 0, 1024).await?;
    assert!(read.content.contains("got:hello-pty"));
    let artifacts = manager.artifacts_for(&entry.handle.task_id)?;
    assert!(std::fs::read_to_string(artifacts.absolute_output)?.contains("got:hello-pty"));
    assert!(std::fs::read_to_string(artifacts.absolute_stdout)?.contains("got:hello-pty"));
    assert_eq!(std::fs::read_to_string(artifacts.absolute_stderr)?, "");

    let late_input = manager
        .input(&entry.handle.task_id, "late\n")
        .await
        .expect_err("terminal input after exit should be rejected");
    assert!(late_input.to_string().contains("not running"));
    let late_resize = manager
        .resize(&entry.handle.task_id, TerminalPtySize::new(20, 80)?)
        .await
        .expect_err("terminal resize after exit should be rejected");
    assert!(late_resize.to_string().contains("not running"));
    Ok(())
}

#[cfg(unix)]
#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_pty_cancel_marks_task_cancelled() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager =
        TerminalProcessManager::new(temp.path())?.with_cancel_grace(Duration::from_millis(50));
    let entry = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(TerminalTaskId::new("terminal-pty-cancel")?),
                command: "sleep 5".to_owned(),
                cwd: None,
                shell: Some(shell),
                env: Default::default(),
            },
            None,
        )
        .await?;

    let cancelled = manager.cancel(&entry.handle.task_id).await?;

    assert!(matches!(cancelled.status, TerminalTaskStatus::Cancelled));
    assert_eq!(
        cancelled
            .cleanup
            .as_ref()
            .expect("terminal cancel should record cleanup")
            .status,
        ExecutionCleanupStatus::Completed
    );
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_reports_unknown_tasks_and_spawn_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let missing = TerminalTaskId::new("terminal-missing")?;

    assert!(manager.status(&missing).await.is_err());
    assert!(manager.read(&missing, 0, 10).await.is_err());
    assert!(manager.cancel(&missing).await.is_err());

    let spawn_error = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-spawn-error")?),
            command: "echo never".to_owned(),
            cwd: None,
            shell: Some("/missing/shell".to_owned()),
            env: Default::default(),
        })
        .await
        .expect_err("missing shell should fail spawn");
    assert!(spawn_error.to_string().contains("failed to start"));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_manager_kill_fallback_cancels_term_ignoring_process() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager =
        TerminalProcessManager::new(temp.path())?.with_cancel_grace(Duration::from_millis(1));
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-kill-fallback")?),
            command: "trap '' TERM; sleep 5".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;

    let cancelled = manager.cancel(&entry.handle.task_id).await?;

    assert!(matches!(cancelled.status, TerminalTaskStatus::Cancelled));
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_private_helpers_cover_error_and_empty_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    assert_eq!(TerminalBackendKind::Process.as_str(), "process");
    assert_eq!(TerminalBackendKind::Pty.as_str(), "pty");
    assert!(TerminalPtySize::new(0, 10).is_err());
    assert!(TerminalPtySize::new(10, 0).is_err());
    let absolute_error = manager
        .stored_artifact_path(Path::new("/tmp/outside"))
        .expect_err("absolute artifact path should be rejected");
    assert!(absolute_error.to_string().contains("must be relative"));

    let missing_log = temp.path().join("missing.log");
    assert!(super::summarize_log(&missing_log, 8).await.is_err());
    assert!(matches!(
        super::status_from_wait_result(Err(std::io::Error::other("boom"))),
        TerminalTaskStatus::Failed { .. }
    ));
    assert!(matches!(
        super::status_from_pty_wait_result(Err(std::io::Error::other("pty boom"))),
        TerminalTaskStatus::Failed { .. }
    ));

    let limited = super::limit_output_bytes(b"abcdef", 4);
    assert!(limited.truncated);
    assert!(limited.content.contains("truncated"));
    let empty = super::limit_output_bytes(b"", 4);
    assert_eq!(empty.content, "");
    assert!(!empty.truncated);

    let output = temp.path().join("combined.log");
    let output_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)
            .await?,
    ));
    let stream_path = temp.path().join("stream.log");
    let empty_capture =
        super::capture_stream::<tokio::io::Empty>(None, stream_path, output_file).await?;
    assert_eq!(empty_capture, 0);

    assert!(super::is_pty_eof_error(&std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "closed"
    )));
    assert!(!super::is_pty_eof_error(&std::io::Error::other("boom")));
    let pty_error = super::capture_pty_reader(
        Box::new(ErrorReader),
        temp.path().join("pty-stream.log"),
        temp.path().join("pty-output.log"),
    )
    .expect_err("pty reader error should be reported");
    assert!(
        pty_error
            .to_string()
            .contains("failed to read terminal pty stream")
    );
    let pty_panic_thread = std::thread::spawn(|| -> Result<u64> {
        panic!("pty reader panicked");
    });
    assert!(
        super::join_pty_read_thread(pty_panic_thread)
            .unwrap_or_default()
            .contains("panicked")
    );
    let pty_error_thread = std::thread::spawn(|| Err::<u64, anyhow::Error>(anyhow!("pty read")));
    assert!(
        super::join_pty_read_thread(pty_error_thread)
            .unwrap_or_default()
            .contains("pty read")
    );

    let aborted_task = tokio::spawn(async {
        sleep(Duration::from_secs(60)).await;
        Ok::<u64, anyhow::Error>(0)
    });
    aborted_task.abort();
    assert!(super::join_capture_task(aborted_task).await.is_err());
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_private_helpers_cover_capture_and_cancel_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = std::fs::canonicalize(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?;

    assert!(super::resolve_terminal_cwd(&workspace_root, Some(Path::new(""))).is_err());
    assert!(super::resolve_terminal_cwd(&workspace_root, Some(Path::new("missing"))).is_err());
    assert_eq!(
        super::resolve_terminal_cwd(&workspace_root, None)?.relative,
        PathBuf::from(".")
    );
    assert_eq!(
        super::lexically_normalize_path(Path::new("../child"))?,
        PathBuf::from("../child")
    );

    let output = temp.path().join("combined-with-data.log");
    let output_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)
            .await?,
    ));
    let stream_path = temp.path().join("stream-with-data.log");
    let (mut writer, reader) = tokio::io::duplex(64);
    writer.write_all(b"chunk").await?;
    drop(writer);
    let captured = super::capture_stream(Some(reader), stream_path.clone(), output_file).await?;
    assert_eq!(captured, 5);
    assert_eq!(std::fs::read_to_string(stream_path)?, "chunk");
    assert_eq!(std::fs::read_to_string(output)?, "chunk");

    let failed_task = tokio::spawn(async { Err::<u64, anyhow::Error>(anyhow!("capture failed")) });
    assert!(super::join_capture_task(failed_task).await.is_err());

    let quick_child = Command::new("/bin/sh").arg("-c").arg("exit 0").spawn()?;
    let mut quick_child = quick_child;
    assert!(matches!(
        super::cancel_child(&mut quick_child, None, Duration::from_secs(1)).await,
        TerminalTaskStatus::Cancelled
    ));

    let slow_child = Command::new("/bin/sh").arg("-c").arg("sleep 5").spawn()?;
    let mut slow_child = slow_child;
    assert!(matches!(
        super::cancel_child(&mut slow_child, None, Duration::from_millis(1)).await,
        TerminalTaskStatus::Cancelled
    ));

    #[cfg(unix)]
    {
        let mut signal_fallback_child = Command::new("/bin/sh").arg("-c").arg("sleep 5").spawn()?;
        let process_id = signal_fallback_child.id().expect("child should expose pid");
        super::send_terminate_signal(process_id).await?;
        let _ = signal_fallback_child.wait().await?;
    }

    let worker_task_id = TerminalTaskId::new("terminal-worker-closed")?;
    let worker_artifacts = manager.artifacts_for(&worker_task_id)?;
    tokio::fs::create_dir_all(&worker_artifacts.absolute_dir).await?;
    super::create_empty_log_files(&worker_artifacts).await?;
    let worker_summary = Arc::new(Mutex::new(test_entry(worker_task_id)));
    let (closed_cancel_tx, closed_cancel_rx) = mpsc::channel::<super::CancelCommand>(1);
    drop(closed_cancel_tx);
    let worker_child = Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 0.05")
        .spawn()?;
    super::run_terminal_worker(super::TerminalWorker {
        child: worker_child,
        process_id: None,
        summary: Arc::clone(&worker_summary),
        artifacts: worker_artifacts,
        stdout_task: tokio::spawn(async { Ok::<u64, anyhow::Error>(0) }),
        stderr_task: tokio::spawn(async { Ok::<u64, anyhow::Error>(0) }),
        cancel_rx: closed_cancel_rx,
        preview_limit_bytes: 8,
        cancel_grace: Duration::from_millis(1),
    })
    .await;
    assert!(matches!(
        worker_summary.lock().await.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));

    let task_id = TerminalTaskId::new("terminal-cancel-drop")?;
    let summary = Arc::new(Mutex::new(test_entry(task_id.clone())));
    let (cancel_tx, mut cancel_rx) = mpsc::channel::<super::CancelCommand>(1);
    tokio::spawn(async move {
        if let Some(command) = cancel_rx.recv().await {
            drop(command.respond_to);
        }
    });
    manager.tasks.lock().await.insert(
        task_id.clone(),
        super::ManagedTerminalTask {
            summary: Arc::clone(&summary),
            control: super::TerminalTaskControl::Process { cancel_tx },
        },
    );
    let current = manager.cancel(&task_id).await?;
    assert!(matches!(current.status, TerminalTaskStatus::Running));

    let missing_receiver_task_id = TerminalTaskId::new("terminal-cancel-no-receiver")?;
    let missing_receiver_summary =
        Arc::new(Mutex::new(test_entry(missing_receiver_task_id.clone())));
    let (missing_receiver_tx, missing_receiver_rx) = mpsc::channel::<super::CancelCommand>(1);
    drop(missing_receiver_rx);
    manager.tasks.lock().await.insert(
        missing_receiver_task_id.clone(),
        super::ManagedTerminalTask {
            summary: Arc::clone(&missing_receiver_summary),
            control: super::TerminalTaskControl::Process {
                cancel_tx: missing_receiver_tx,
            },
        },
    );
    let send_error = manager
        .cancel(&missing_receiver_task_id)
        .await
        .expect_err("closed cancel receiver should be reported");
    assert!(send_error.to_string().contains("no longer running"));

    let waiting_summary = Arc::new(Mutex::new(test_entry(TerminalTaskId::new(
        "terminal-wait-running",
    )?)));
    assert!(
        super::wait_for_terminal_summary(&waiting_summary, Duration::ZERO)
            .await
            .is_none()
    );
    waiting_summary.lock().await.status = TerminalTaskStatus::Cancelled;
    assert!(
        super::wait_for_terminal_summary(&waiting_summary, Duration::ZERO)
            .await
            .is_some()
    );

    let pty_cancel_summary = Arc::new(Mutex::new(test_entry(TerminalTaskId::new(
        "terminal-pty-cancel-helper",
    )?)));
    let pty_cancel_artifacts =
        manager.artifacts_for(&TerminalTaskId::new("terminal-pty-cancel-helper")?)?;
    tokio::fs::create_dir_all(&pty_cancel_artifacts.absolute_dir).await?;
    super::create_empty_log_files(&pty_cancel_artifacts).await?;
    let cancelled = super::cancel_pty_task(
        &pty_cancel_summary,
        Arc::new(StdMutex::new(
            Box::new(SuccessfulKiller) as Box<dyn portable_pty::ChildKiller + Send + Sync>
        )),
        None,
        Arc::new(AtomicBool::new(false)),
        Duration::ZERO,
        Arc::new(pty_cancel_artifacts),
        8,
    )
    .await?;
    assert!(matches!(cancelled.status, TerminalTaskStatus::Cancelled));

    let pty_cancel_error_summary = Arc::new(Mutex::new(test_entry(TerminalTaskId::new(
        "terminal-pty-cancel-helper-error",
    )?)));
    let pty_cancel_error_artifacts =
        manager.artifacts_for(&TerminalTaskId::new("terminal-pty-cancel-helper-error")?)?;
    let cancel_error = super::cancel_pty_task(
        &pty_cancel_error_summary,
        Arc::new(StdMutex::new(
            Box::new(FailingKiller) as Box<dyn portable_pty::ChildKiller + Send + Sync>
        )),
        None,
        Arc::new(AtomicBool::new(false)),
        Duration::ZERO,
        Arc::new(pty_cancel_error_artifacts),
        8,
    )
    .await
    .expect_err("failing pty killer should be reported");
    assert!(
        cancel_error
            .to_string()
            .contains("failed to kill terminal pty child")
    );
    Ok(())
}

#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_finalize_covers_capture_and_summary_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let task_id = TerminalTaskId::new("terminal-finalize-error")?;
    let artifacts = manager.artifacts_for(&task_id)?;
    let summary = Arc::new(Mutex::new(test_entry(task_id)));
    let stdout_task = tokio::spawn(async { Err::<u64, anyhow::Error>(anyhow!("capture failed")) });
    let stderr_task = tokio::spawn(async { Ok::<u64, anyhow::Error>(0) });

    let entry = super::finalize_terminal_task(
        &summary,
        &artifacts,
        TerminalTaskStatus::Exited { exit_code: Some(0) },
        stdout_task,
        stderr_task,
        8,
    )
    .await;

    assert!(matches!(entry.status, TerminalTaskStatus::Failed { .. }));
    assert!(
        entry
            .output_preview
            .as_deref()
            .unwrap_or_default()
            .contains("failed to summarize")
    );

    let worker_task_id = TerminalTaskId::new("terminal-pty-worker-abort")?;
    let worker_artifacts = manager.artifacts_for(&worker_task_id)?;
    tokio::fs::create_dir_all(&worker_artifacts.absolute_dir).await?;
    super::create_empty_log_files(&worker_artifacts).await?;
    let worker_summary = Arc::new(Mutex::new(test_entry(worker_task_id)));
    let aborted_wait_task = tokio::spawn(async {
        sleep(Duration::from_secs(60)).await;
        super::PtyWaitOutcome {
            status: TerminalTaskStatus::Exited { exit_code: Some(0) },
            capture_error: None,
        }
    });
    aborted_wait_task.abort();
    super::run_pty_worker(super::PtyWorker {
        summary: Arc::clone(&worker_summary),
        artifacts: worker_artifacts,
        wait_task: aborted_wait_task,
        preview_limit_bytes: 8,
    })
    .await;
    assert!(matches!(
        worker_summary.lock().await.status,
        TerminalTaskStatus::Failed { .. }
    ));
    Ok(())
}

#[cfg(unix)]
#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_artifacts_succeeds_with_explicit_artifact_root() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new_with_artifact_root(
        workspace.path(),
        workspace.path().join("custom-artifacts"),
        "custom-artifacts",
    )?;
    let artifacts = manager.artifacts_for(&TerminalTaskId::new("terminal-custom")?)?;
    assert!(artifacts.relative_dir.starts_with("custom-artifacts"));
    Ok(())
}

fn test_entry(task_id: TerminalTaskId) -> TerminalTaskEntry {
    TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id,
            command: "test".to_owned(),
            cwd: PathBuf::from("."),
            shell: "sh".to_owned(),
            log_path: PathBuf::from("state/artifacts/tasks/test/output.log"),
            created_at_ms: 1,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        status: TerminalTaskStatus::Running,
        output_preview: None,
        output_hash: None,
        output_truncated: false,
        cleanup: None,
        updated_at_ms: 1,
    }
}

struct ErrorReader;

impl Read for ErrorReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("pty read failed"))
    }
}

#[derive(Debug)]
struct FailingKiller;

#[derive(Debug)]
struct SuccessfulKiller;

impl portable_pty::ChildKiller for SuccessfulKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Self)
    }
}

impl portable_pty::ChildKiller for FailingKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("kill failed"))
    }

    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Self)
    }
}

#[cfg(unix)]
fn test_shell(dir: &Path) -> Result<String> {
    let shell = dir.join("test-shell");
    std::fs::write(
        &shell,
        "#!/bin/sh\nif [ \"$1\" = \"-lc\" ]; then shift; fi\nexec /bin/sh -c \"$1\"\n",
    )?;
    let mut permissions = std::fs::metadata(&shell)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shell, permissions)?;
    Ok(shell.display().to_string())
}

#[cfg(not(unix))]
fn test_shell(_dir: &Path) -> Result<String> {
    Ok("sh".to_owned())
}

async fn wait_for_terminal_status(
    manager: &TerminalProcessManager,
    task_id: &TerminalTaskId,
) -> Result<sigil_kernel::TerminalTaskEntry> {
    for _ in 0..250 {
        let status = manager.status(task_id).await?;
        if status.status.is_terminal() {
            return Ok(status);
        }
        sleep(Duration::from_millis(20)).await;
    }
    manager.status(task_id).await
}
