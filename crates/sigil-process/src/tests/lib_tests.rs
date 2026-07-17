use super::*;

#[test]
fn owner_probe_and_non_windows_guard_are_constructible() -> anyhow::Result<()> {
    validate_process_tree_owner()?;
    #[cfg(not(windows))]
    let _guard = ProcessTreeOwnerGuard::assign(None)?;
    Ok(())
}
