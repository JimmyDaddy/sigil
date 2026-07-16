#[cfg(target_os = "macos")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, bail};
#[cfg(test)]
use nix::sys::signal::kill;
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinuxProcStat {
    state: char,
    process_group_id: u32,
}

#[cfg(unix)]
pub(crate) async fn send_process_group_signal(process_id: u32, signal: &str) -> Result<()> {
    let process_group_id = process_group_id(process_id)?;
    let signal = signal_from_name(signal)?;
    killpg(process_group_id, signal).with_context(|| {
        format!(
            "failed to send {signal:?} to process group {}",
            process_group_id.as_raw()
        )
    })
}

#[cfg(unix)]
pub(crate) async fn process_group_has_live_members(process_id: u32) -> Result<bool> {
    let process_group_id = process_group_id(process_id)?;
    match killpg(process_group_id, None) {
        Ok(()) | Err(Errno::EPERM) => {}
        Err(Errno::ESRCH) => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect process group {}",
                    process_group_id.as_raw()
                )
            });
        }
    }

    #[cfg(target_os = "linux")]
    {
        let process_group_id = process_id;
        tokio::task::spawn_blocking(move || linux_process_group_has_live_members(process_group_id))
            .await
            .context("Linux process-group inspection task failed")
    }

    #[cfg(target_os = "macos")]
    {
        macos_process_group_has_live_members(process_id).await
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(true)
    }
}

#[cfg(test)]
pub(crate) fn process_is_live(process_id: u32) -> Result<bool> {
    let process_id = pid_from_u32(process_id)?;
    match kill(process_id, None) {
        Ok(()) | Err(Errno::EPERM) => {}
        Err(Errno::ESRCH) => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect process {}", process_id.as_raw()));
        }
    }

    #[cfg(target_os = "linux")]
    {
        linux_process_is_live(process_id.as_raw() as u32)
    }

    #[cfg(not(target_os = "linux"))]
    Ok(true)
}

#[cfg(unix)]
fn process_group_id(process_id: u32) -> Result<Pid> {
    Ok(Pid::from_raw(i32::try_from(process_id).with_context(
        || format!("process group id {process_id} exceeds the platform pid range"),
    )?))
}

#[cfg(test)]
fn pid_from_u32(process_id: u32) -> Result<Pid> {
    Ok(Pid::from_raw(i32::try_from(process_id).with_context(
        || format!("process id {process_id} exceeds the platform pid range"),
    )?))
}

#[cfg(unix)]
fn signal_from_name(signal: &str) -> Result<Signal> {
    match signal {
        "TERM" => Ok(Signal::SIGTERM),
        "KILL" => Ok(Signal::SIGKILL),
        _ => bail!("unsupported process-group signal {signal}"),
    }
}

#[cfg(target_os = "linux")]
fn linux_process_group_has_live_members(process_group_id: u32) -> bool {
    linux_process_group_has_live_members_in(Path::new("/proc"), process_group_id)
}

#[cfg(target_os = "linux")]
fn linux_process_group_has_live_members_in(proc_root: &Path, process_group_id: u32) -> bool {
    let entries = match fs::read_dir(proc_root) {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return true;
        };
        let Some(process_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        match read_linux_proc_stat(proc_root, process_id) {
            Ok(Some(stat))
                if stat.process_group_id == process_group_id && linux_state_is_live(stat.state) =>
            {
                return true;
            }
            Ok(_) => {}
            Err(_) => return true,
        }
    }
    false
}

#[cfg(all(test, target_os = "linux"))]
fn linux_process_is_live(process_id: u32) -> Result<bool> {
    Ok(read_linux_proc_stat(Path::new("/proc"), process_id)?
        .is_some_and(|stat| linux_state_is_live(stat.state)))
}

#[cfg(target_os = "linux")]
fn read_linux_proc_stat(proc_root: &Path, process_id: u32) -> Result<Option<LinuxProcStat>> {
    let path = proc_root.join(process_id.to_string()).join("stat");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read Linux process stat {}", path.display()));
        }
    };
    parse_linux_proc_stat(&contents)
        .map(Some)
        .with_context(|| format!("failed to parse Linux process stat {}", path.display()))
}

#[cfg(target_os = "linux")]
fn parse_linux_proc_stat(contents: &str) -> Result<LinuxProcStat> {
    let command_start = contents
        .find('(')
        .context("Linux process stat is missing its command opener")?;
    contents[..command_start]
        .trim()
        .parse::<u32>()
        .context("Linux process stat has an invalid process id")?;
    let command_end = contents
        .rfind(')')
        .context("Linux process stat is missing its command terminator")?;
    if command_end <= command_start {
        bail!("Linux process stat command terminator precedes its opener");
    }
    let mut fields = contents[command_end + 1..].split_whitespace();
    let state_field = fields
        .next()
        .context("Linux process stat is missing its state")?;
    let mut state_chars = state_field.chars();
    let state = state_chars
        .next()
        .context("Linux process stat has an empty state")?;
    if state_chars.next().is_some() {
        bail!("Linux process stat has an invalid multi-character state");
    }
    fields
        .next()
        .context("Linux process stat is missing its parent process id")?;
    let process_group_id = fields
        .next()
        .context("Linux process stat is missing its process group id")?
        .parse::<u32>()
        .context("Linux process stat has an invalid process group id")?;
    Ok(LinuxProcStat {
        state,
        process_group_id,
    })
}

#[cfg(target_os = "linux")]
fn linux_state_is_live(state: char) -> bool {
    !matches!(state, 'Z' | 'X' | 'x')
}

#[cfg(target_os = "macos")]
async fn macos_process_group_has_live_members(process_group_id: u32) -> Result<bool> {
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("/bin/ps")
            .args(["-axo", "pgid=,state="])
            .output(),
    )
    .await
    .context("macOS process-group inspection timed out")?
    .context("failed to run macOS process-group inspection")?;
    if !output.status.success() {
        bail!(
            "macOS process-group inspection exited with {}",
            output.status
        );
    }
    let stdout = std::str::from_utf8(&output.stdout)
        .context("macOS process-group inspection output is not UTF-8")?;
    macos_ps_has_live_group_members(stdout, process_group_id)
}

#[cfg(target_os = "macos")]
fn macos_ps_has_live_group_members(stdout: &str, process_group_id: u32) -> Result<bool> {
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let observed_group = fields
            .next()
            .context("macOS process-group inspection row is missing pgid")?
            .parse::<u32>()
            .context("macOS process-group inspection row has invalid pgid")?;
        let state = fields
            .next()
            .context("macOS process-group inspection row is missing state")?;
        if fields.next().is_some() {
            bail!("macOS process-group inspection row has unexpected fields");
        }
        if observed_group == process_group_id && !state.starts_with('Z') {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "tests/process_group_tests.rs"]
mod tests;
