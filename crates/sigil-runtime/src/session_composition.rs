//! Bind runtime selection to the durable session before model or tool execution.

use anyhow::{Result, bail, ensure};
use sigil_kernel::{
    ControlEntry, OptionalCapability, RootConfig, RuntimeCompositionConfig, Session,
    SessionCompositionSnapshotV1, SessionLogEntry,
};

/// Ensures a long-lived host uses only the capabilities admitted by its current authority boot.
/// The caller supplies its effective configuration; this check performs no module initialization.
pub fn validate_boot_composition(
    config: &RootConfig,
    expected: &RuntimeCompositionConfig,
) -> Result<()> {
    ensure!(
        config.selected_capabilities() == expected.selected_capabilities(),
        "session capability composition differs from the current authority boot; restart the runtime before changing capabilities"
    );
    Ok(())
}

/// Checks an existing selection without initializing any optional capability. A session with
/// execution history but no binding belongs to an unsupported contract and cannot continue.
pub fn validate_session_composition(session: &Session, config: &RootConfig) -> Result<()> {
    validate_session_composition_snapshot(
        session,
        &SessionCompositionSnapshotV1::new(config.selected_capabilities()),
    )
}

pub(crate) fn validate_session_composition_snapshot(
    session: &Session,
    expected: &SessionCompositionSnapshotV1,
) -> Result<()> {
    let mut binding = None;
    let mut has_execution_history = false;
    for entry in session.entries() {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot)) => {
                snapshot.validate()?;
                ensure!(
                    binding.is_none(),
                    "session has duplicate composition bindings"
                );
                ensure!(
                    !has_execution_history,
                    "session composition binding follows execution history; start a new session"
                );
                binding = Some(snapshot);
            }
            SessionLogEntry::Assistant(_) | SessionLogEntry::ToolResultV3(_) => {
                has_execution_history = true;
            }
            SessionLogEntry::Control(
                ControlEntry::ToolExecution(_)
                | ControlEntry::TaskRun(_)
                | ControlEntry::TerminalTask(_),
            ) => has_execution_history = true,
            _ => {}
        }
    }
    match binding {
        Some(actual) => ensure!(
            actual.schema_version == expected.schema_version
                && actual.core_contract_version == expected.core_contract_version
                && execution_capabilities(&actual.capabilities)
                    == execution_capabilities(&expected.capabilities),
            "session capability composition differs from configuration; start a new session"
        ),
        None if has_execution_history => {
            bail!("session has no capability composition binding; start a new session")
        }
        None => {}
    }
    Ok(())
}

/// Returns capabilities that participate in the session's execution contract. Host maintenance
/// modules such as the updater may be added or removed while an existing session remains usable;
/// they do not change provider, tool, or durable execution semantics.
fn execution_capabilities(
    capabilities: &std::collections::BTreeSet<OptionalCapability>,
) -> std::collections::BTreeSet<OptionalCapability> {
    capabilities
        .iter()
        .copied()
        .filter(|capability| *capability != OptionalCapability::Updater)
        .collect()
}

/// Appends exactly one recovery-critical selection. Queued user input is permitted before the
/// first binding; model output and effect history are not inferred into a new contract.
pub fn bind_session_composition(session: &mut Session, config: &RootConfig) -> Result<()> {
    bind_session_composition_snapshot(
        session,
        SessionCompositionSnapshotV1::new(config.selected_capabilities()),
    )
}

pub(crate) fn bind_session_composition_snapshot(
    session: &mut Session,
    snapshot: SessionCompositionSnapshotV1,
) -> Result<()> {
    validate_session_composition_snapshot(session, &snapshot)?;
    if !session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionCompositionBound(_))
        )
    }) {
        session.append_control(ControlEntry::SessionCompositionBound(snapshot))?;
    }
    Ok(())
}

/// Child sessions created by optional runtimes retain the parent's already selected contract.
/// Low-level unbound sessions remain unbound and cannot enter product session resume.
pub(crate) fn inherit_session_composition(parent: &Session, child: &mut Session) -> Result<()> {
    if let Some(snapshot) = parent.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot)) => Some(snapshot),
        _ => None,
    }) {
        validate_session_composition_snapshot(parent, snapshot)?;
        bind_session_composition_snapshot(child, snapshot.clone())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/session_composition_tests.rs"]
mod tests;
