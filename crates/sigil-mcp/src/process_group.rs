#[cfg(unix)]
use anyhow::{Context, Result, anyhow, bail};
#[cfg(all(test, unix))]
use nix::sys::signal::kill;
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};

#[cfg(unix)]
pub(crate) fn signal_process_group(process_id: u32, signal: &str) -> Result<()> {
    let process_group = process_group_id(process_id)?;
    let signal = match signal {
        "TERM" => Signal::SIGTERM,
        "KILL" => Signal::SIGKILL,
        unsupported => bail!("unsupported process-group signal {unsupported}"),
    };
    killpg(process_group, signal)
        .with_context(|| format!("failed to signal process group {process_id} with {signal}"))
}

#[cfg(unix)]
pub(crate) async fn process_group_has_live_members(process_id: u32) -> Result<bool> {
    let process_group = process_group_id(process_id)?;
    match killpg(process_group, None) {
        Ok(()) => {}
        Err(Errno::EPERM) => return Ok(true),
        Err(Errno::ESRCH) => return Ok(false),
        Err(error) => {
            return Err(anyhow!(error))
                .with_context(|| format!("failed to probe process group {process_id}"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let scan = tokio::task::spawn_blocking(move || scan_linux_process_group(process_id))
            .await
            .context("Linux process-group liveness scan task failed")?;
        Ok(linux_process_group_scan_has_live_effect(scan))
    }

    #[cfg(not(target_os = "linux"))]
    Ok(true)
}

#[cfg(all(test, unix))]
pub(crate) fn process_has_live_effect(process_id: u32) -> Result<bool> {
    let process = process_group_id(process_id)?;
    match kill(process, None) {
        Ok(()) => {}
        Err(Errno::EPERM) => return Ok(true),
        Err(Errno::ESRCH) => return Ok(false),
        Err(error) => {
            return Err(anyhow!(error))
                .with_context(|| format!("failed to probe process {process_id}"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        let stat_path = format!("/proc/{process_id}/stat");
        match std::fs::read_to_string(&stat_path) {
            Ok(stat) => parse_linux_proc_stat(&stat)
                .map(|stat| linux_process_state_is_live(stat.state))
                .with_context(|| format!("failed to parse {stat_path}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("failed to read {stat_path}")),
        }
    }

    #[cfg(not(target_os = "linux"))]
    Ok(true)
}

#[cfg(unix)]
fn process_group_id(process_id: u32) -> Result<Pid> {
    let process_id = i32::try_from(process_id)
        .with_context(|| format!("process id {process_id} exceeds the platform PID range"))?;
    Ok(Pid::from_raw(process_id))
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxProcessGroupScan {
    Live,
    Quiescent,
    Indeterminate,
}

#[cfg(target_os = "linux")]
fn scan_linux_process_group(process_group_id: u32) -> LinuxProcessGroupScan {
    let entries = match std::fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return LinuxProcessGroupScan::Indeterminate,
    };
    let mut scan = LinuxProcessGroupScan::Quiescent;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                scan = LinuxProcessGroupScan::Indeterminate;
                continue;
            }
        };
        let is_process_entry = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_some();
        if !is_process_entry {
            continue;
        }
        let stat_path = entry.path().join("stat");
        let stat = match std::fs::read_to_string(&stat_path) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                scan = LinuxProcessGroupScan::Indeterminate;
                continue;
            }
        };
        scan =
            observe_linux_process_group_stat(scan, process_group_id, parse_linux_proc_stat(&stat));
        if scan == LinuxProcessGroupScan::Live {
            return LinuxProcessGroupScan::Live;
        }
    }
    scan
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinuxProcStat {
    state: char,
    process_group_id: u32,
}

#[cfg(target_os = "linux")]
fn parse_linux_proc_stat(stat: &str) -> Result<LinuxProcStat> {
    let command_start = stat
        .find('(')
        .context("missing opening command delimiter")?;
    stat.get(..command_start)
        .context("invalid command delimiter boundary")?
        .trim()
        .parse::<u32>()
        .context("invalid process id")?;
    let command_end = stat
        .rfind(')')
        .context("missing closing command delimiter")?;
    if command_end <= command_start {
        bail!("closing command delimiter precedes opening delimiter");
    }
    let fields = stat
        .get(command_end + 1..)
        .context("invalid command delimiter boundary")?
        .split_whitespace()
        .collect::<Vec<_>>();
    let state = fields
        .first()
        .and_then(|field| {
            let mut chars = field.chars();
            let state = chars.next()?;
            chars.next().is_none().then_some(state)
        })
        .context("missing or invalid process state")?;
    let process_group_id = fields
        .get(2)
        .context("missing process-group id")?
        .parse::<u32>()
        .context("invalid process-group id")?;
    Ok(LinuxProcStat {
        state,
        process_group_id,
    })
}

#[cfg(target_os = "linux")]
fn linux_process_state_is_live(state: char) -> bool {
    !matches!(state, 'Z' | 'X' | 'x')
}

#[cfg(target_os = "linux")]
fn observe_linux_process_group_stat(
    current: LinuxProcessGroupScan,
    process_group_id: u32,
    stat: Result<LinuxProcStat>,
) -> LinuxProcessGroupScan {
    if current == LinuxProcessGroupScan::Live {
        return current;
    }
    match stat {
        Ok(stat)
            if stat.process_group_id == process_group_id
                && linux_process_state_is_live(stat.state) =>
        {
            LinuxProcessGroupScan::Live
        }
        Ok(_) => current,
        Err(_) => LinuxProcessGroupScan::Indeterminate,
    }
}

#[cfg(target_os = "linux")]
fn linux_process_group_scan_has_live_effect(scan: LinuxProcessGroupScan) -> bool {
    !matches!(scan, LinuxProcessGroupScan::Quiescent)
}

#[cfg(test)]
#[path = "tests/process_group_tests.rs"]
mod tests;
