use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::sync::{Mutex as StdMutex, atomic::AtomicBool};

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupStatus, ExecutionConfig,
    ExecutionSandboxFallback, ExecutionSandboxProfile, ExecutionSandboxStrategyConfig,
    TerminalExecutionBackendCapabilities, TerminalExecutionBackendKind,
    TerminalOutputTerminationReason, TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId,
    TerminalTaskStatus,
};
#[cfg(unix)]
use tokio::process::Command;
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    sync::{Mutex, mpsc},
    time::{sleep, timeout},
};

#[cfg(unix)]
use super::TerminalExecutionConfig;
use super::{TerminalBackendKind, TerminalProcessManager, TerminalPtySize, TerminalStartRequest};
use serial_test::serial;

#[cfg(unix)]
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

#[cfg(windows)]
#[test]
fn terminal_cwd_accepts_prefixed_workspace_paths_and_keeps_confinement() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let canonical_workspace = workspace.path().canonicalize()?;
    let nested = canonical_workspace.join("nested");
    std::fs::create_dir(&nested)?;

    let resolved = super::resolve_terminal_cwd(&canonical_workspace, Some(&nested))?;
    assert_eq!(resolved.relative, PathBuf::from("nested"));
    assert_eq!(resolved.absolute, nested.canonicalize()?);

    let outside = tempfile::tempdir()?;
    let error =
        super::resolve_terminal_cwd(&canonical_workspace, Some(&outside.path().canonicalize()?))
            .expect_err("prefixed cwd outside the workspace must remain rejected");
    assert!(error.to_string().contains("outside workspace"));
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

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_process_manager_read_clamps_public_limit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-read-hard-clamp")?),
            command: "dd if=/dev/zero bs=1024 count=200 2>/dev/null | tr '\\000' x".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;
    wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    let read = manager.read(&entry.handle.task_id, 0, usize::MAX).await?;

    assert_eq!(
        read.returned_bytes,
        crate::constants::HARD_TERMINAL_READ_LIMIT_BYTES as u64
    );
    assert!(read.truncated);
    assert_eq!(read.next_offset, Some(read.returned_bytes));
    Ok(())
}

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_output_limit_kills_term_ignoring_descendant_and_records_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let descendant_pid_file = temp.path().join("descendant.pid");
    let manager = TerminalProcessManager::new(temp.path())?
        .with_artifact_limits(1024, 2048)
        .with_cancel_grace(Duration::from_millis(50));
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-output-limit")?),
            command: concat!(
                "sh -c 'trap \"\" TERM; echo $$ > \"$DESCENDANT_PID_FILE\"; ",
                "while :; do sleep 1; done' & ",
                "while [ ! -s \"$DESCENDANT_PID_FILE\" ]; do sleep 0.01; done; ",
                "trap '' TERM; while :; do printf 0123456789abcdef; done"
            )
            .to_owned(),
            cwd: None,
            shell: Some(shell),
            env: BTreeMap::from([(
                "DESCENDANT_PID_FILE".to_owned(),
                descendant_pid_file.display().to_string(),
            )]),
        })
        .await?;

    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Failed { .. }
    ));
    assert_eq!(final_entry.output_limit_bytes, Some(1024));
    assert_eq!(
        final_entry.output_termination_reason,
        Some(TerminalOutputTerminationReason::OutputLimitExceeded)
    );
    assert!(final_entry.output_truncated);
    assert!(final_entry.output_total_bytes > 1024);
    assert_eq!(
        final_entry
            .cleanup
            .as_ref()
            .expect("output-limit cleanup receipt")
            .status,
        ExecutionCleanupStatus::Completed
    );
    let artifacts = manager.artifacts_for(&entry.handle.task_id)?;
    assert!(std::fs::metadata(&artifacts.absolute_stdout)?.len() <= 1024);
    assert!(std::fs::metadata(&artifacts.absolute_output)?.len() <= 2048);

    let descendant_pid = std::fs::read_to_string(&descendant_pid_file)?
        .trim()
        .parse::<u32>()?;
    assert!(!process_is_alive(descendant_pid).await);
    Ok(())
}

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_fast_exit_dual_stream_limits_preserve_observed_total() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?
        .with_artifact_limits(1024, 2048)
        .with_cancel_grace(Duration::from_millis(50));
    let entry = manager
        .start(TerminalStartRequest {
            task_id: Some(TerminalTaskId::new("terminal-fast-dual-limit")?),
            command: "head -c 4096 /dev/zero; head -c 4096 /dev/zero >&2".to_owned(),
            cwd: None,
            shell: Some(shell),
            env: Default::default(),
        })
        .await?;

    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;
    let artifacts = manager.artifacts_for(&entry.handle.task_id)?;
    let stdout_bytes = std::fs::metadata(&artifacts.absolute_stdout)?.len();
    let stderr_bytes = std::fs::metadata(&artifacts.absolute_stderr)?.len();
    let combined_bytes = std::fs::metadata(&artifacts.absolute_output)?.len();

    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Failed { .. }
    ));
    assert_eq!(
        final_entry.output_termination_reason,
        Some(TerminalOutputTerminationReason::OutputLimitExceeded)
    );
    assert_eq!(final_entry.output_limit_bytes, Some(1024));
    assert!(stdout_bytes <= 1024);
    assert!(stderr_bytes <= 1024);
    assert!(stdout_bytes == 1024 || stderr_bytes == 1024);
    assert_eq!(combined_bytes, stdout_bytes + stderr_bytes);
    assert!(combined_bytes <= 2048);
    // The first reader to exceed its limit triggers immediate process-tree cleanup. The other
    // pipe may therefore remain unread even when the command had already produced more bytes.
    assert!(final_entry.output_total_bytes > combined_bytes);
    assert!(final_entry.output_truncated);
    Ok(())
}

#[tokio::test]
async fn terminal_capture_limit_records_observed_bytes_without_unbounded_drain() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output_path = temp.path().join("capture-limit-output.log");
    let output_file = Arc::new(Mutex::new(super::CombinedOutputWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .await?,
        8,
    )));
    let stream_path = temp.path().join("capture-limit-stdout.log");
    let ledger = Arc::new(super::TerminalCaptureLedger::default());
    let (mut writer, reader) = tokio::io::duplex(8);
    writer.write_all(b"observed").await?;
    drop(writer);
    let (capture_failure_tx, mut capture_failure_rx) = mpsc::unbounded_channel();

    let error = super::capture_stream(
        Some(reader),
        super::TerminalOutputStream::Stdout,
        stream_path.clone(),
        output_file,
        super::TerminalArtifactLimits {
            stream_bytes: 4,
            combined_bytes: 8,
        },
        Arc::clone(&ledger),
        capture_failure_tx,
    )
    .await
    .expect_err("the stream limit should stop capture");

    assert!(error.to_string().contains("output limit exceeded"));
    let failure = capture_failure_rx
        .recv()
        .await
        .expect("capture failure should be reported");
    assert_eq!(failure.limit_bytes(), Some(4));
    assert_eq!(ledger.total_observed_bytes(), 8);
    assert_eq!(ledger.omitted_observed_bytes(), 4);
    assert_eq!(
        ledger.termination_reason(),
        Some(TerminalOutputTerminationReason::OutputLimitExceeded)
    );
    Ok(())
}

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_pty_output_limit_is_structured_and_bounded() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let shell = test_shell(temp.path())?;
    let manager = TerminalProcessManager::new(temp.path())?
        .with_artifact_limits(1024, 2048)
        .with_cancel_grace(Duration::from_millis(50));
    let entry = manager
        .start_pty(
            TerminalStartRequest {
                task_id: Some(TerminalTaskId::new("terminal-pty-output-limit")?),
                command: "trap '' TERM; while :; do printf 0123456789abcdef; done".to_owned(),
                cwd: None,
                shell: Some(shell),
                env: Default::default(),
            },
            None,
        )
        .await?;

    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;

    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Failed { .. }
    ));
    assert_eq!(final_entry.output_limit_bytes, Some(1024));
    assert_eq!(
        final_entry.output_termination_reason,
        Some(TerminalOutputTerminationReason::OutputLimitExceeded)
    );
    assert!(final_entry.output_total_bytes > 1024);
    let artifacts = manager.artifacts_for(&entry.handle.task_id)?;
    assert!(std::fs::metadata(artifacts.absolute_stdout)?.len() <= 1024);
    Ok(())
}

#[tokio::test]
async fn terminal_streaming_summary_hashes_and_retains_only_head_tail() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("streaming-summary.log");
    let mut bytes = vec![b'x'; 32 * 1024];
    bytes[..5].copy_from_slice(b"HEAD!");
    let tail = bytes.len() - 5;
    bytes[tail..].copy_from_slice(b"TAIL!");
    tokio::fs::write(&path, &bytes).await?;

    let summary = super::summarize_log(&path, 96).await?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);

    assert_eq!(summary.total_bytes, bytes.len() as u64);
    assert_eq!(summary.sha256, format!("{:x}", hasher.finalize()));
    assert!(summary.truncated);
    assert!(summary.preview.starts_with("HEAD!"));
    assert!(summary.preview.ends_with("TAIL!"));
    assert!(summary.preview.len() <= 96);
    assert!(summary.preview.contains("total 32768 bytes"));
    Ok(())
}

#[tokio::test]
async fn terminal_streaming_summary_bounds_lossy_utf8_expansion() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("invalid-utf8.log");
    tokio::fs::write(&path, vec![0xff; 32 * 1024]).await?;

    let summary = super::summarize_log(&path, 1024).await?;

    assert!(summary.truncated);
    assert!(summary.preview.len() <= 1024);
    assert!(summary.preview.contains("terminal output truncated"));
    assert!(summary.preview.contains("total 32768 bytes"));
    assert_eq!(summary.total_bytes, 32 * 1024);
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
    let subdir = temp.path().join("subdir");
    std::fs::create_dir(&subdir)?;
    let manager = TerminalProcessManager::new(temp.path())?;

    #[cfg(windows)]
    let command = "(Get-Location).Path";
    #[cfg(not(windows))]
    let command = "pwd";

    let entry = manager
        .start(TerminalStartRequest {
            task_id: None,
            command: command.to_owned(),
            cwd: Some(subdir.clone()),
            shell: None,
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
    assert!(
        read.content
            .to_ascii_lowercase()
            .contains(&subdir.display().to_string().to_ascii_lowercase())
    );
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
    let preview = final_entry.output_preview.as_deref().unwrap_or_default();
    assert!(!preview.is_empty());
    assert!(preview.len() <= 4);

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

    assert!(
        matches!(
            cancel_result.status,
            TerminalTaskStatus::Exited { exit_code: Some(0) }
        ),
        "unexpected status: {:?}",
        cancel_result.status
    );
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
            shell: Some("/missing/sh".to_owned()),
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
        .stored_artifact_path(temp.path())
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

    let output = temp.path().join("combined.log");
    let output_file = Arc::new(Mutex::new(super::CombinedOutputWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)
            .await?,
        1024,
    )));
    let stream_path = temp.path().join("stream.log");
    let (capture_failure_tx, _capture_failure_rx) = mpsc::unbounded_channel();
    let empty_capture = super::capture_stream::<tokio::io::Empty>(
        None,
        super::TerminalOutputStream::Stdout,
        stream_path,
        output_file,
        super::TerminalArtifactLimits::default(),
        Arc::new(super::TerminalCaptureLedger::default()),
        capture_failure_tx,
    )
    .await?;
    assert_eq!(empty_capture.observed_bytes, 0);

    assert!(super::is_pty_eof_error(&std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "closed"
    )));
    assert!(!super::is_pty_eof_error(&std::io::Error::other("boom")));
    let (capture_failure_tx, _capture_failure_rx) = mpsc::unbounded_channel();
    let pty_error = super::capture_pty_reader(
        Box::new(ErrorReader),
        temp.path().join("pty-stream.log"),
        temp.path().join("pty-output.log"),
        super::TerminalArtifactLimits::default(),
        Arc::new(super::TerminalCaptureLedger::default()),
        capture_failure_tx,
    )
    .expect_err("pty reader error should be reported");
    assert!(pty_error.to_string().contains("read pty stream failed"));
    let pty_panic_thread = std::thread::spawn(|| -> Result<super::io::CaptureOutcome> {
        panic!("pty reader panicked");
    });
    assert!(
        super::join_pty_read_thread(pty_panic_thread)
            .unwrap_or_default()
            .contains("panicked")
    );
    let pty_error_thread =
        std::thread::spawn(|| Err::<super::io::CaptureOutcome, anyhow::Error>(anyhow!("pty read")));
    assert!(
        super::join_pty_read_thread(pty_error_thread)
            .unwrap_or_default()
            .contains("pty read")
    );

    let aborted_task = tokio::spawn(async {
        sleep(Duration::from_secs(60)).await;
        Ok::<super::io::CaptureOutcome, anyhow::Error>(capture_outcome(0))
    });
    aborted_task.abort();
    assert!(aborted_task.await.is_err());
    Ok(())
}

#[cfg(unix)]
#[serial]
#[tokio::test]
async fn terminal_reader_task_panic_triggers_live_cleanup_without_failure_signal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let task_id = TerminalTaskId::new("terminal-reader-panic")?;
    let artifacts = manager.artifacts_for(&task_id)?;
    tokio::fs::create_dir_all(&artifacts.absolute_dir).await?;
    super::create_empty_log_files(&artifacts).await?;
    let summary = Arc::new(Mutex::new(test_entry(task_id)));
    let pid_file = temp.path().join("reader-panic-child.pid");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("trap '' TERM; sleep 30 & echo $! > \"$1\"; wait")
        .arg("sh")
        .arg(&pid_file)
        .kill_on_drop(true);
    super::configure_process_group(&mut command);
    let child = command.spawn()?;
    let process_id = child.id();
    let panic_pid_file = pid_file.clone();
    let stdout_task = tokio::spawn(async move {
        for _ in 0..200 {
            if tokio::fs::metadata(&panic_pid_file).await.is_ok() {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
        panic!("spontaneous terminal stdout reader panic");
        #[allow(unreachable_code)]
        Ok::<super::io::CaptureOutcome, anyhow::Error>(capture_outcome(0))
    });
    let stderr_task =
        tokio::spawn(async { Ok::<super::io::CaptureOutcome, anyhow::Error>(capture_outcome(0)) });
    let (capture_failure_tx, capture_failure_rx) = mpsc::unbounded_channel();
    drop(capture_failure_tx);
    let (_cancel_tx, cancel_rx) = mpsc::channel(1);
    let started = std::time::Instant::now();

    super::run_terminal_worker(super::TerminalWorker {
        _process_owner: crate::process_owner::ProcessTreeOwnerGuard::assign(process_id)?,
        child,
        process_id,
        summary: Arc::clone(&summary),
        artifacts,
        stdout_task,
        stderr_task,
        capture_ledger: Arc::new(super::TerminalCaptureLedger::default()),
        capture_failure_rx,
        cancel_rx,
        preview_limit_bytes: 8,
        cancel_grace: Duration::from_millis(50),
    })
    .await;

    assert!(started.elapsed() < Duration::from_secs(2));
    let entry = summary.lock().await.clone();
    let TerminalTaskStatus::Failed { reason } = &entry.status else {
        panic!("spontaneous reader panic should fail the task: {entry:?}");
    };
    assert!(reason.contains("terminal stdout capture task failed"));
    assert!(reason.contains("panicked"));
    assert_eq!(
        entry.output_termination_reason,
        Some(TerminalOutputTerminationReason::OutputCaptureFailed)
    );
    assert_eq!(
        entry.cleanup.as_ref().map(|cleanup| cleanup.status),
        Some(ExecutionCleanupStatus::Completed)
    );
    let pid = std::fs::read_to_string(pid_file)?;
    let process_id = pid.trim().parse::<u32>()?;
    for _ in 0..20 {
        if !process_is_alive(process_id).await {
            return Ok(());
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "child process {} should be gone after reader-panic cleanup",
        pid.trim()
    );
}

#[tokio::test]
async fn terminal_pty_reader_panic_is_reported_live_after_the_panic() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (capture_failure_tx, mut capture_failure_rx) = mpsc::unbounded_channel();
    let read_thread = super::spawn_pty_read_thread(
        Box::new(PanicReader),
        temp.path().join("pty-panic-stdout.log"),
        temp.path().join("pty-panic-output.log"),
        super::TerminalArtifactLimits::default(),
        Arc::new(super::TerminalCaptureLedger::default()),
        capture_failure_tx,
    );

    let failure = timeout(Duration::from_secs(1), capture_failure_rx.recv())
        .await?
        .ok_or_else(|| anyhow!("pty panic fixture closed without a failure"))?;
    assert!(failure.to_string().contains("pty output reader panicked"));
    assert!(
        super::join_pty_read_thread(read_thread)
            .unwrap_or_default()
            .contains("pty output reader panicked")
    );
    Ok(())
}

#[cfg(unix)]
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
    let output_file = Arc::new(Mutex::new(super::CombinedOutputWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)
            .await?,
        1024,
    )));
    let stream_path = temp.path().join("stream-with-data.log");
    let (mut writer, reader) = tokio::io::duplex(64);
    writer.write_all(b"chunk").await?;
    drop(writer);
    let (capture_failure_tx, _capture_failure_rx) = mpsc::unbounded_channel();
    let captured = super::capture_stream(
        Some(reader),
        super::TerminalOutputStream::Stdout,
        stream_path.clone(),
        output_file,
        super::TerminalArtifactLimits::default(),
        Arc::new(super::TerminalCaptureLedger::default()),
        capture_failure_tx,
    )
    .await?;
    assert_eq!(captured.observed_bytes, 5);
    assert_eq!(std::fs::read_to_string(stream_path)?, "chunk");
    assert_eq!(std::fs::read_to_string(output)?, "chunk");

    let failed_task = tokio::spawn(async {
        Err::<super::io::CaptureOutcome, anyhow::Error>(anyhow!("capture failed"))
    });
    assert!(failed_task.await?.is_err());

    let quick_child = Command::new("/bin/sh").arg("-c").arg("exit 0").spawn()?;
    let mut quick_child = quick_child;
    assert!(matches!(
        super::cancel_child(&mut quick_child, None, Duration::from_secs(1)).await,
        TerminalTaskStatus::Interrupted
    ));

    let slow_child = Command::new("/bin/sh").arg("-c").arg("sleep 5").spawn()?;
    let mut slow_child = slow_child;
    assert!(matches!(
        super::cancel_child(&mut slow_child, None, Duration::from_millis(1)).await,
        TerminalTaskStatus::Interrupted
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
    let (capture_failure_tx, capture_failure_rx) = mpsc::unbounded_channel();
    drop(capture_failure_tx);
    drop(closed_cancel_tx);
    let worker_child = Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 0.05")
        .spawn()?;
    super::run_terminal_worker(super::TerminalWorker {
        _process_owner: crate::process_owner::ProcessTreeOwnerGuard::assign(None)?,
        child: worker_child,
        process_id: None,
        summary: Arc::clone(&worker_summary),
        artifacts: worker_artifacts,
        stdout_task: tokio::spawn(async {
            Ok::<super::io::CaptureOutcome, anyhow::Error>(capture_outcome(0))
        }),
        stderr_task: tokio::spawn(async {
            Ok::<super::io::CaptureOutcome, anyhow::Error>(capture_outcome(0))
        }),
        capture_ledger: Arc::new(super::TerminalCaptureLedger::default()),
        capture_failure_rx,
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
    let error = manager
        .cancel(&task_id)
        .await
        .expect_err("lost cancellation response must not report running as success");
    assert!(error.to_string().contains("cleanup could be confirmed"));

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
        Arc::new(super::TerminalCaptureLedger::default()),
        Arc::new(AtomicBool::new(false)),
        Duration::ZERO,
        Arc::new(pty_cancel_artifacts),
        8,
    )
    .await?;
    assert!(matches!(cancelled.status, TerminalTaskStatus::Interrupted));
    assert!(cancelled.cleanup.as_ref().is_some_and(|cleanup| {
        cleanup.status != sigil_kernel::ExecutionCleanupStatus::Completed
    }));

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
        Arc::new(super::TerminalCaptureLedger::default()),
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

#[cfg(unix)]
#[serial]
#[cfg_attr(coverage, ignore)]
#[tokio::test]
async fn terminal_process_finalize_covers_capture_and_summary_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let task_id = TerminalTaskId::new("terminal-finalize-error")?;
    let artifacts = manager.artifacts_for(&task_id)?;
    let summary = Arc::new(Mutex::new(test_entry(task_id)));
    let stdout_task = tokio::spawn(async {
        Err::<super::io::CaptureOutcome, anyhow::Error>(anyhow!("capture failed"))
    });
    let stderr_task =
        tokio::spawn(async { Ok::<super::io::CaptureOutcome, anyhow::Error>(capture_outcome(0)) });

    let entry = super::finalize_terminal_task(
        &summary,
        &artifacts,
        TerminalTaskStatus::Exited { exit_code: Some(0) },
        stdout_task,
        stderr_task,
        Arc::new(super::TerminalCaptureLedger::default()),
        8,
    )
    .await;

    assert!(matches!(entry.status, TerminalTaskStatus::Failed { .. }));
    assert_eq!(
        entry.output_termination_reason,
        Some(TerminalOutputTerminationReason::OutputCaptureFailed)
    );
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
    let (_capture_failure_tx, capture_failure_rx) = mpsc::unbounded_channel();
    let (_child_exit_tx, child_exit_rx) = mpsc::unbounded_channel();
    let aborted_wait_task = tokio::spawn(async {
        sleep(Duration::from_secs(60)).await;
        super::PtyWaitOutcome {
            status: TerminalTaskStatus::Exited { exit_code: Some(0) },
            capture_error: None,
        }
    });
    aborted_wait_task.abort();
    super::run_pty_worker(super::PtyWorker {
        _process_owner: crate::process_owner::ProcessTreeOwnerGuard::assign(None)?,
        summary: Arc::clone(&worker_summary),
        artifacts: worker_artifacts,
        wait_task: aborted_wait_task,
        killer: Arc::new(StdMutex::new(
            Box::new(SuccessfulKiller) as Box<dyn portable_pty::ChildKiller + Send + Sync>
        )),
        process_id: None,
        capture_ledger: Arc::new(super::TerminalCaptureLedger::default()),
        cancel_requested: Arc::new(AtomicBool::new(false)),
        capture_failure_rx,
        child_exit_rx,
        preview_limit_bytes: 8,
        cancel_grace: Duration::from_millis(1),
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
        output_total_bytes: 0,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: 1,
    }
}

fn capture_outcome(bytes: u64) -> super::io::CaptureOutcome {
    super::io::CaptureOutcome {
        observed_bytes: bytes,
        written_bytes: bytes,
    }
}

struct ErrorReader;

impl Read for ErrorReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("pty read failed"))
    }
}

struct PanicReader;

impl Read for PanicReader {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        panic!("spontaneous pty reader panic")
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct FailingKiller;

#[cfg(unix)]
#[derive(Debug)]
struct SuccessfulKiller;

#[cfg(unix)]
impl portable_pty::ChildKiller for SuccessfulKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Self)
    }
}

#[cfg(unix)]
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
    let shell = dir.join("sh");
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

#[cfg(unix)]
async fn process_is_alive(process_id: u32) -> bool {
    crate::process_group::process_is_live(process_id).unwrap_or(true)
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
