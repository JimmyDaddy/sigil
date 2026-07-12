#[cfg(target_os = "linux")]
use super::*;

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_stat_parser_handles_spaces_and_nested_parentheses() -> Result<()> {
    let stat = "123 (worker (nested) name) S 1 77 77 0 -1";
    assert_eq!(
        parse_linux_proc_stat(stat)?,
        LinuxProcStat {
            state: 'S',
            process_group_id: 77,
        }
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_stat_parser_rejects_missing_or_invalid_fields() {
    assert!(parse_linux_proc_stat("123 worker S 1 77").is_err());
    assert!(parse_linux_proc_stat("worker) S 1 77").is_err());
    assert!(parse_linux_proc_stat("123 (worker) SS 1 77").is_err());
    assert!(parse_linux_proc_stat("123 (worker) S 1 nope").is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_process_states_distinguish_live_members_from_zombies() {
    for state in ['R', 'S', 'D', 'T', 't', 'W', 'I'] {
        assert!(linux_process_state_is_live(state));
    }
    for state in ['Z', 'X', 'x'] {
        assert!(!linux_process_state_is_live(state));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_process_group_scan_treats_zombie_only_groups_as_quiescent() {
    let process_group_id = 77;
    let mut scan = LinuxProcessGroupScan::Quiescent;
    for state in ['Z', 'X', 'x'] {
        scan = observe_linux_process_group_stat(
            scan,
            process_group_id,
            Ok(LinuxProcStat {
                state,
                process_group_id,
            }),
        );
    }
    scan = observe_linux_process_group_stat(
        scan,
        process_group_id,
        Ok(LinuxProcStat {
            state: 'S',
            process_group_id: 88,
        }),
    );

    assert_eq!(scan, LinuxProcessGroupScan::Quiescent);
    assert!(!linux_process_group_scan_has_live_effect(scan));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_process_group_scan_is_live_for_members_and_indeterminate_reads() {
    let live = observe_linux_process_group_stat(
        LinuxProcessGroupScan::Quiescent,
        77,
        Ok(LinuxProcStat {
            state: 'S',
            process_group_id: 77,
        }),
    );
    assert_eq!(live, LinuxProcessGroupScan::Live);
    assert!(linux_process_group_scan_has_live_effect(live));

    let indeterminate = observe_linux_process_group_stat(
        LinuxProcessGroupScan::Quiescent,
        77,
        parse_linux_proc_stat("unreadable proc stat"),
    );
    assert_eq!(indeterminate, LinuxProcessGroupScan::Indeterminate);
    assert!(linux_process_group_scan_has_live_effect(indeterminate));
}

#[cfg(unix)]
#[tokio::test]
async fn process_group_probe_reports_current_group_as_live() -> anyhow::Result<()> {
    let process_group_id = u32::try_from(nix::unistd::getpgrp().as_raw())?;
    assert!(super::process_group_has_live_members(process_group_id).await?);
    Ok(())
}
