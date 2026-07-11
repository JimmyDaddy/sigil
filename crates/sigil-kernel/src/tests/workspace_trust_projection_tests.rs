use std::{fs, path::Path};

use anyhow::Result;

use crate::{
    ControlEntry, SessionLogEntry, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    stable_workspace_id, workspace_trust_from_entries,
};

fn trust_decision(
    workspace_root: &Path,
    trust: WorkspaceTrust,
    sequence: usize,
) -> Result<SessionLogEntry> {
    Ok(SessionLogEntry::Control(
        ControlEntry::WorkspaceTrustDecision(WorkspaceTrustDecisionEntry {
            workspace_id: stable_workspace_id(workspace_root)?,
            workspace_trust_snapshot_id: format!("trust-snapshot-{sequence}"),
            trust,
            decided_by_event_id: Some(format!("trust-event-{sequence}")),
            reason: Some("workspace trust projection test".to_owned()),
        }),
    ))
}

fn workspace(root: &Path, name: &str) -> Result<std::path::PathBuf> {
    let workspace = root.join(name);
    fs::create_dir(&workspace)?;
    Ok(workspace)
}

#[test]
fn workspace_trust_projection_defaults_to_unknown_without_a_match() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = workspace(temp.path(), "current")?;

    assert_eq!(
        workspace_trust_from_entries(&[], &workspace)?,
        WorkspaceTrust::Unknown
    );
    Ok(())
}

#[test]
fn workspace_trust_projection_ignores_cross_workspace_decisions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let current = workspace(temp.path(), "current")?;
    let other = workspace(temp.path(), "other")?;
    let entries = vec![trust_decision(&other, WorkspaceTrust::Trusted, 1)?];

    assert_eq!(
        workspace_trust_from_entries(&entries, &current)?,
        WorkspaceTrust::Unknown
    );
    Ok(())
}

#[test]
fn workspace_trust_projection_uses_latest_matching_restricted_or_denied_decision() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let current = workspace(temp.path(), "current")?;
    let other = workspace(temp.path(), "other")?;
    let mut entries = vec![
        trust_decision(&current, WorkspaceTrust::Trusted, 1)?,
        trust_decision(&other, WorkspaceTrust::Denied, 2)?,
        trust_decision(&current, WorkspaceTrust::Restricted, 3)?,
    ];

    assert_eq!(
        workspace_trust_from_entries(&entries, &current)?,
        WorkspaceTrust::Restricted
    );

    entries.push(trust_decision(&current, WorkspaceTrust::Denied, 4)?);
    assert_eq!(
        workspace_trust_from_entries(&entries, &current)?,
        WorkspaceTrust::Denied
    );
    Ok(())
}
