use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::{Result, anyhow};
use sigil_kernel::{TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus};
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    process::Command,
    sync::{Mutex, mpsc},
    time::sleep,
};

use super::{TerminalProcessManager, TerminalStartRequest};

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
        })
        .await?;

    assert!(matches!(entry.status, TerminalTaskStatus::Running));
    let final_entry = wait_for_terminal_status(&manager, &entry.handle.task_id).await?;
    assert!(matches!(
        final_entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert_eq!(
        final_entry.handle.log_path,
        PathBuf::from(".sigil/tasks/terminal-1/output.log")
    );
    assert!(!final_entry.output_truncated);
    assert!(final_entry.output_hash.is_some());
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
        })
        .await?;

    let cancelled = manager.cancel(&entry.handle.task_id).await?;

    assert!(matches!(cancelled.status, TerminalTaskStatus::Cancelled));
    assert!(cancelled.status.is_terminal());
    let status = manager.status(&entry.handle.task_id).await?;
    assert!(matches!(status.status, TerminalTaskStatus::Cancelled));
    Ok(())
}

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
        })
        .await
        .expect_err("workspace escape should be rejected");
    assert!(escape_error.to_string().contains("outside workspace"));
    Ok(())
}

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
        })
        .await?;

    let error = manager
        .start(TerminalStartRequest {
            task_id: Some(task_id.clone()),
            command: "pwd".to_owned(),
            cwd: None,
            shell: None,
        })
        .await
        .expect_err("duplicate task id should be rejected");
    assert!(error.to_string().contains("already exists"));
    manager.cancel(&task_id).await?;
    Ok(())
}

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
        })
        .await
        .expect_err("missing shell should fail spawn");
    assert!(spawn_error.to_string().contains("failed to start"));
    Ok(())
}

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
        })
        .await?;

    let cancelled = manager.cancel(&entry.handle.task_id).await?;

    assert!(matches!(cancelled.status, TerminalTaskStatus::Cancelled));
    Ok(())
}

#[tokio::test]
async fn terminal_process_private_helpers_cover_error_and_empty_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let manager = TerminalProcessManager::new(temp.path())?;
    let absolute_error = manager
        .workspace_artifact_path(Path::new("/tmp/outside"))
        .expect_err("absolute artifact path should be rejected");
    assert!(absolute_error.to_string().contains("workspace-relative"));

    let missing_log = temp.path().join("missing.log");
    assert!(super::summarize_log(&missing_log, 8).await.is_err());
    assert!(matches!(
        super::status_from_wait_result(Err(std::io::Error::other("boom"))),
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

    let aborted_task = tokio::spawn(async {
        sleep(Duration::from_secs(60)).await;
        Ok::<u64, anyhow::Error>(0)
    });
    aborted_task.abort();
    assert!(super::join_capture_task(aborted_task).await.is_err());
    Ok(())
}

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
            cancel_tx,
        },
    );
    let current = manager.cancel(&task_id).await?;
    assert!(matches!(current.status, TerminalTaskStatus::Running));
    Ok(())
}

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
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_process_artifacts_reject_workspace_symlink_escape() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::os::unix::fs::symlink(outside.path(), workspace.path().join(".sigil"))?;
    let manager = TerminalProcessManager::new(workspace.path())?;

    let error = manager
        .artifacts_for(&TerminalTaskId::new("terminal-symlink")?)
        .expect_err("artifact path should reject symlink escape");

    assert!(error.to_string().contains("outside workspace"));
    Ok(())
}

fn test_entry(task_id: TerminalTaskId) -> TerminalTaskEntry {
    TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id,
            command: "test".to_owned(),
            cwd: PathBuf::from("."),
            shell: "sh".to_owned(),
            log_path: PathBuf::from(".sigil/tasks/test/output.log"),
            created_at_ms: 1,
        },
        status: TerminalTaskStatus::Running,
        output_preview: None,
        output_hash: None,
        output_truncated: false,
        updated_at_ms: 1,
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
    for _ in 0..50 {
        let status = manager.status(task_id).await?;
        if status.status.is_terminal() {
            return Ok(status);
        }
        sleep(Duration::from_millis(20)).await;
    }
    manager.status(task_id).await
}
