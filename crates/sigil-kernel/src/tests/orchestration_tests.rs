use anyhow::Result;

use crate::{
    ControlEntry, JsonlSessionStore, OrchestrationHardInvariant, OrchestrationRouteDisabledEntry,
    OrchestrationRouteDisablementProjection, Session, SessionLogEntry,
};

fn disablement(
    route_byte: char,
    build: &str,
    invariant: OrchestrationHardInvariant,
) -> OrchestrationRouteDisabledEntry {
    OrchestrationRouteDisabledEntry {
        route_fingerprint: format!("sha256:{}", route_byte.to_string().repeat(64)),
        sigil_build: build.to_owned(),
        invariant,
        report_handle: "session:session-1:orchestration-invariant".to_owned(),
        disabled_at_ms: 1,
    }
}

#[test]
fn orchestration_route_disablement_is_exact_to_route_and_build() -> Result<()> {
    let disabled = disablement('a', "build-1", OrchestrationHardInvariant::DuplicateHandoff);
    disabled.validate()?;
    let projection = OrchestrationRouteDisablementProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(disabled.clone())),
        SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(disabled)),
    ]);

    assert!(projection.is_disabled(&format!("sha256:{}", "a".repeat(64)), "build-1"));
    assert!(!projection.is_disabled(&format!("sha256:{}", "b".repeat(64)), "build-1"));
    assert!(!projection.is_disabled(&format!("sha256:{}", "a".repeat(64)), "build-2"));
    Ok(())
}

#[test]
fn orchestration_route_disablement_validation_fails_closed() {
    let mut disabled = disablement(
        'a',
        "build-1",
        OrchestrationHardInvariant::UnknownEffectReplay,
    );
    disabled.route_fingerprint = "route".to_owned();
    assert!(disabled.validate().is_err());

    disabled = disablement(
        'a',
        "build-1",
        OrchestrationHardInvariant::UnknownEffectReplay,
    );
    disabled.report_handle.clear();
    assert!(disabled.validate().is_err());

    disabled = disablement(
        'a',
        "build-1",
        OrchestrationHardInvariant::UnknownEffectReplay,
    );
    disabled.disabled_at_ms = 0;
    assert!(disabled.validate().is_err());
}

#[test]
fn orchestration_route_disablement_round_trips_as_recovery_critical_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::load_from_store("provider", "model", store)?;
    let disabled = disablement(
        'a',
        "build-1",
        OrchestrationHardInvariant::DuplicateContinuation,
    );

    session.append_control(ControlEntry::OrchestrationRouteDisabled(disabled))?;

    let raw = std::fs::read_to_string(&path)?;
    assert!(raw.contains(r#""event_type":"orchestration_route_disabled""#));
    let reloaded = Session::load_from_store("provider", "model", JsonlSessionStore::new(&path)?)?;
    assert!(
        reloaded
            .orchestration_route_disablement_projection()
            .is_disabled(&format!("sha256:{}", "a".repeat(64)), "build-1")
    );
    Ok(())
}
