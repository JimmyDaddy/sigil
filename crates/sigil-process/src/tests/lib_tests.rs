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

#[cfg(windows)]
#[test]
fn windows_job_limit_structure_default_is_zeroed() {
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;

    let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();

    assert_eq!(limits.BasicLimitInformation.LimitFlags, 0);
}
