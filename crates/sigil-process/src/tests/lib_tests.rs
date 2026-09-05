use super::*;

#[test]
fn owner_probe_and_non_windows_guard_are_constructible() -> anyhow::Result<()> {
    validate_process_tree_owner()?;
    #[cfg(not(windows))]
    let _guard = ProcessTreeOwnerGuard::assign(None)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn configured_unix_process_group_can_be_terminated_and_reaped() -> anyhow::Result<()> {
    use std::{
        fs,
        process::Command,
        thread,
        time::{Duration, Instant, SystemTime},
    };

    use anyhow::bail;
    use nix::{errno::Errno, sys::signal, unistd::Pid};

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let descendant_path = std::env::temp_dir().join(format!(
        "sigil-process-group-descendant-{}-{unique}.pid",
        std::process::id()
    ));
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "sleep 30 & echo $! > \"$1\"; wait",
            "sigil-process-test",
        ])
        .arg(&descendant_path);
    configure_process_tree(&mut command);
    let mut child = command.spawn()?;
    let process_id = child.id();
    let owner = ProcessTreeOwnerGuard::assign(Some(process_id))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let descendant_pid = loop {
        if let Ok(raw) = fs::read_to_string(&descendant_path)
            && let Ok(process_id) = raw.trim().parse::<i32>()
        {
            break process_id;
        }
        if Instant::now() >= deadline {
            let _ = terminate_owned_process_tree(process_id);
            let _ = child.wait();
            bail!("descendant process id was not reported before the deadline");
        }
        thread::sleep(Duration::from_millis(10));
    };

    if let Err(error) = owner.terminate() {
        let _ = child.kill();
        let _ = child.wait();
        fs::remove_file(&descendant_path).ok();
        return Err(error);
    }
    let status = child.wait()?;
    assert!(!status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match signal::kill(Pid::from_raw(descendant_pid), None) {
            Err(Errno::ESRCH) => break,
            Ok(()) | Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            _ => {
                fs::remove_file(&descendant_path).ok();
                bail!("descendant remained after process-group termination");
            }
        }
    }
    fs::remove_file(descendant_path)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn bounded_reaper_accepts_a_normal_terminal_leader_when_its_group_is_already_absent()
-> anyhow::Result<()> {
    use std::{
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use anyhow::bail;

    let mut command = Command::new("sh");
    command.args(["-c", "sleep 0.05"]);
    configure_process_tree(&mut command);
    let child = command.spawn()?;
    let mut reaper = UnixOwnedChildReaperV1::adopt(child)?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match reaper.observe_terminal_without_reap()? {
            NonConsumingChildTerminalObservationV1::Terminal => break,
            NonConsumingChildTerminalObservationV1::StillRunning if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            NonConsumingChildTerminalObservationV1::StillRunning => {
                bail!("normal helper did not become terminal before the deadline");
            }
        }
    }

    let receipt = reaper.cleanup_and_reap(deadline, Duration::ZERO)?;
    assert_eq!(receipt.registered_member_count(), 0);
    assert_eq!(
        receipt.direct_child_process_id(),
        reaper.direct_child_process_id()
    );
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn bounded_reaper_keeps_terminal_leader_unreaped_until_detached_member_cleanup()
-> anyhow::Result<()> {
    use std::{
        fs, io,
        process::{Command, Stdio},
        time::{Duration, Instant, SystemTime},
    };

    use anyhow::{Context, bail};

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "sigil-process-bounded-reaper-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&root).context("create owned bounded-reaper fixture root")?;
    let detached_pid_path = root.join("detached.pid");
    let detached_control_path = root.join("detached.control");
    let result = (|| -> anyhow::Result<()> {
        fs::write(&detached_control_path, b"keep-alive")
            .context("create detached helper control marker")?;
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "python3 -c 'import os,sys,time\nos.setsid()\nwith open(sys.argv[1], \"w\") as pid:\n    pid.write(f\"{os.getpid()}\\n\")\n    pid.flush()\nos.close(1)\ndeadline = time.monotonic() + 30\nwhile time.monotonic() < deadline and os.path.exists(sys.argv[2]):\n    time.sleep(0.1)' \"$1\" \"$2\" & exit 0",
                "sigil-process-bounded-reaper",
            ])
            .arg(&detached_pid_path)
            .arg(&detached_control_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_process_tree(&mut command);
        let mut child = command
            .spawn()
            .context("spawn owned bounded-reaper helper")?;
        // The detached Python process closes its inherited stdout after the startup handshake.
        // Removing the marker below makes a failed handshake or timeout terminate the fixture.
        let _stdout_pipe = child.stdout.take().context("capture helper stdout pipe")?;
        let mut reaper = UnixOwnedChildReaperV1::adopt(child)
            .context("adopt configured direct child into the unique reaper")?;
        let cleanup_deadline = Instant::now() + Duration::from_secs(5);
        let test_result = (|| -> anyhow::Result<()> {
            let detached_process_id = loop {
                if let Ok(raw) = fs::read_to_string(&detached_pid_path)
                    && let Ok(process_id) = raw.trim().parse::<u32>()
                {
                    break process_id;
                }
                if Instant::now() >= cleanup_deadline {
                    bail!("detached helper did not publish its process id before the deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let detached_identity = loop {
                match observe_process_identity(detached_process_id) {
                    Ok(identity) => break identity,
                    Err(ProcessIdentityObservationErrorV1::Absent)
                    | Err(ProcessIdentityObservationErrorV1::NotLive(_))
                        if Instant::now() < cleanup_deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => {
                        bail!("detached helper birth identity was not observable: {error}");
                    }
                }
            };
            reaper
                .register_known_member(detached_identity)
                .context("register exact setsid member")?;

            loop {
                match reaper.observe_terminal_without_reap()? {
                    NonConsumingChildTerminalObservationV1::Terminal => break,
                    NonConsumingChildTerminalObservationV1::StillRunning
                        if Instant::now() < cleanup_deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    NonConsumingChildTerminalObservationV1::StillRunning => {
                        bail!("direct helper did not become terminal before the deadline");
                    }
                }
            }
            assert_eq!(
                reaper.observe_terminal_without_reap()?,
                NonConsumingChildTerminalObservationV1::Terminal,
                "a second observation must not consume the direct child before cleanup"
            );

            let receipt = reaper
                .cleanup_and_reap(cleanup_deadline, Duration::from_millis(250))
                .context("bounded cleanup must terminate the exact detached member")?;
            assert_eq!(receipt.registered_member_count(), 1);
            assert_eq!(
                receipt.direct_child_process_id(),
                reaper.direct_child_process_id()
            );
            Ok(())
        })();
        if test_result.is_err() {
            fs::remove_file(&detached_control_path).ok();
            let _ =
                reaper.cleanup_and_reap(Instant::now() + Duration::from_secs(2), Duration::ZERO);
        }
        test_result
    })();
    fs::remove_file(&detached_control_path).ok();
    let cleanup = fs::remove_dir_all(&root);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(io::Error::new(
            error.kind(),
            format!(
                "remove owned bounded-reaper fixture root {}: {error}",
                root.display()
            ),
        )
        .into()),
    }
}

#[cfg(windows)]
#[test]
fn windows_job_limit_structure_default_is_zeroed() {
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;

    let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();

    assert_eq!(limits.BasicLimitInformation.LimitFlags, 0);
}
