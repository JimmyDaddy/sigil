use anyhow::Result;
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, OrchestrationHardInvariant, OrchestrationRouteDisabledEntry,
    Session,
};
use tempfile::tempdir;

use super::check_orchestration_route_disablement;
use crate::{
    OrchestrationRouteGuard,
    doctor::{DoctorReport, DoctorStatus},
};

#[test]
fn doctor_reports_only_the_exact_disabled_orchestration_route_and_build() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    std::fs::create_dir(&session_dir)?;
    let guard = OrchestrationRouteGuard::new("provider", "model", "build-1");
    let store = JsonlSessionStore::new(session_dir.join("session-1.jsonl"))?;
    let mut session = Session::load_from_store("provider", "model", store)?;
    session.append_control(ControlEntry::OrchestrationRouteDisabled(
        OrchestrationRouteDisabledEntry {
            route_fingerprint: guard.route_fingerprint().to_owned(),
            sigil_build: guard.sigil_build().to_owned(),
            invariant: OrchestrationHardInvariant::DuplicateSpawn,
            report_handle: "session:session-1:orchestration-invariant".to_owned(),
            disabled_at_ms: 1,
        },
    ))?;

    let mut report = DoctorReport::default();
    check_orchestration_route_disablement(&mut report, &session_dir, &guard);

    assert!(report.checks.iter().any(|check| {
        check.name == "orchestration:route"
            && check.status == DoctorStatus::Warn
            && check.message.contains("DuplicateSpawn")
            && check
                .message
                .contains("session:session-1:orchestration-invariant")
            && check
                .remediation
                .as_deref()
                .is_some_and(|value| value.contains("explicit /task"))
    }));

    let mut other_build_report = DoctorReport::default();
    check_orchestration_route_disablement(
        &mut other_build_report,
        &session_dir,
        &OrchestrationRouteGuard::new("provider", "model", "build-2"),
    );
    assert!(
        other_build_report
            .checks
            .iter()
            .all(|check| check.name != "orchestration:route")
    );
    Ok(())
}
