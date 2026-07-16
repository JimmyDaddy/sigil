use anyhow::Result;

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_stat_parser_handles_parentheses_in_command_names() -> Result<()> {
    let stat = parse_linux_proc_stat("123 (worker (phase)) S 7 91 91 0 -1")?;

    assert_eq!(stat.state, 'S');
    assert_eq!(stat.process_group_id, 91);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_stat_parser_distinguishes_zombie_and_live_states() -> Result<()> {
    let zombie = parse_linux_proc_stat("123 (worker) Z 7 91 91 0 -1")?;
    let live = parse_linux_proc_stat("124 (worker) R 7 91 91 0 -1")?;

    assert!(!linux_state_is_live(zombie.state));
    assert!(linux_state_is_live(live.state));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_stat_parser_rejects_multi_character_state() {
    let error = parse_linux_proc_stat("123 (worker) ZZ 7 91 91 0 -1")
        .expect_err("multi-character process state must be rejected");

    assert!(error.to_string().contains("multi-character state"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_stat_parser_rejects_missing_command_opener() {
    let error = parse_linux_proc_stat("123 worker) S 7 91 91 0 -1")
        .expect_err("missing process command opener must be rejected");

    assert!(error.to_string().contains("command opener"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_process_group_scan_treats_zombie_only_group_as_quiescent() -> Result<()> {
    let proc_root = tempfile::tempdir()?;
    write_proc_stat(proc_root.path(), 123, "worker", 'Z', 91)?;
    write_proc_stat(proc_root.path(), 124, "worker", 'X', 91)?;
    write_proc_stat(proc_root.path(), 125, "other", 'R', 92)?;

    assert!(!linux_process_group_has_live_members_in(
        proc_root.path(),
        91
    ));
    write_proc_stat(proc_root.path(), 126, "worker", 'S', 91)?;
    assert!(linux_process_group_has_live_members_in(
        proc_root.path(),
        91
    ));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_process_group_scan_is_conservative_when_proc_is_unavailable() -> Result<()> {
    let proc_root = tempfile::tempdir()?;

    assert!(linux_process_group_has_live_members_in(
        &proc_root.path().join("missing"),
        91
    ));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn macos_ps_parser_ignores_zombies_and_detects_live_group_members() -> Result<()> {
    let zombie_only = "  91 Z\n  91 Z+\n  92 R\n";
    assert!(!macos_ps_has_live_group_members(zombie_only, 91)?);

    let live_member = "  91 Z\n  91 S+\n";
    assert!(macos_ps_has_live_group_members(live_member, 91)?);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn macos_ps_parser_rejects_incomplete_rows() {
    let error =
        macos_ps_has_live_group_members("91\n", 91).expect_err("missing state must fail closed");

    assert!(error.to_string().contains("missing state"));
}

#[cfg(unix)]
#[test]
fn process_probe_reports_current_test_process_as_live() -> Result<()> {
    assert!(process_is_live(std::process::id())?);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn process_group_probe_reports_missing_group_as_quiescent() -> Result<()> {
    assert!(!process_group_has_live_members(i32::MAX as u32).await?);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn process_group_probe_reports_current_group_as_live() -> Result<()> {
    let process_group_id = u32::try_from(nix::unistd::getpgrp().as_raw())?;
    assert!(process_group_has_live_members(process_group_id).await?);
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_proc_stat(
    proc_root: &std::path::Path,
    process_id: u32,
    command: &str,
    state: char,
    process_group_id: u32,
) -> Result<()> {
    let process_root = proc_root.join(process_id.to_string());
    std::fs::create_dir(&process_root)?;
    std::fs::write(
        process_root.join("stat"),
        format!("{process_id} ({command}) {state} 1 {process_group_id} {process_group_id} 0 -1"),
    )?;
    Ok(())
}
