use super::*;

#[test]
fn owner_probe_and_non_windows_guard_are_constructible() -> anyhow::Result<()> {
    validate_process_tree_owner()?;
    #[cfg(not(windows))]
    let _guard = ProcessTreeOwnerGuard::assign(None)?;
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_job_limit_structure_default_is_zeroed() {
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;

    let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();

    assert_eq!(limits.BasicLimitInformation.LimitFlags, 0);
}
