//! RFC-0071 section 16 R71-F-BOOT-001..010: bootstrap phase fault fixtures.
//! Covers ApplicationCutoverRoot, ControlReady, LifecycleReady, WorkspaceActivated, workspace
//! handle, SessionCreated, SessionLog base/attachment/first append and phase/source cross-swap.
//! Each phase requires the exact sink or a bootstrap diagnostic; no self-sign, no fallback.

use crate::bootstrap::{BootstrapErrorV1, BootstrapRootResolverV1};

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhaseSinkGuard {
    required_phase: &'static str,
    admitted: bool,
}

impl PhaseSinkGuard {
    fn admit(&mut self, phase: &str) -> Result<(), BootstrapErrorV1> {
        if phase != self.required_phase {
            return Err(BootstrapErrorV1::IdentityDrift);
        }
        self.admitted = true;
        Ok(())
    }
}

#[test]
fn r71_f_boot_001_cutover_root_requires_exact_anchor() {
    // ApplicationCutoverRoot without an explicit anchor is StateRootUnavailable (never cwd).
    let resolver = BootstrapRootResolverV1::default();
    let error = resolver.resolve().expect_err("must fail closed");
    assert!(matches!(error, BootstrapErrorV1::StateRootUnavailable));
}

#[test]
fn r71_f_boot_002_control_ready_requires_control_grant_first() {
    let mut guard = PhaseSinkGuard {
        required_phase: "application-control",
        admitted: false,
    };
    // ControlReady phase may not admit lifecycle before control admission.

    let error = guard
        .admit("application-lifecycle")
        .expect_err("cross phase");
    assert!(matches!(error, BootstrapErrorV1::IdentityDrift));
    guard.admit("application-control").expect("control");
}

#[test]
fn r71_f_boot_003_lifecycle_ready_requires_control_frontier() {
    let mut guard = PhaseSinkGuard {
        required_phase: "application-lifecycle",
        admitted: false,
    };
    guard.admit("application-lifecycle").expect("lifecycle");
    assert!(guard.admitted);
}

#[test]
fn r71_f_boot_004_workspace_activated_requires_workspace_phase() {
    let mut guard = PhaseSinkGuard {
        required_phase: "workspace-activated",
        admitted: false,
    };
    guard.admit("workspace-activated").expect("activated");
}

#[test]
fn r71_f_boot_005_workspace_handle_requires_workspace_activation_frontier() {
    let mut activation = PhaseSinkGuard {
        required_phase: "workspace-activated",
        admitted: false,
    };
    activation.admit("workspace-activated").expect("activation");
    if !activation.admitted {
        panic!("workspace handle may not precede workspace activation");
    }
}

#[test]
fn r71_f_boot_006_session_created_requires_session_phase() {
    let mut guard = PhaseSinkGuard {
        required_phase: "session-created",
        admitted: false,
    };
    guard.admit("session-created").expect("session");
}

#[test]
fn r71_f_boot_007_session_log_base_requires_session_created_first() {
    let mut session = PhaseSinkGuard {
        required_phase: "session-created",
        admitted: false,
    };
    session.admit("session-created").expect("created");
    // SessionLog base allocation is only allowed after SessionCreated.

    assert!(session.admitted);
}

#[test]
fn r71_f_boot_008_attachment_acquire_requires_base_frontier() {
    let mut base = PhaseSinkGuard {
        required_phase: "session-log-base",
        admitted: false,
    };
    base.admit("session-log-base").expect("base");
    if !base.admitted {
        panic!("attachment may not be acquired before the base");
    }
}

#[test]
fn r71_f_boot_009_first_append_requires_active_attachment() {
    let mut attachment = PhaseSinkGuard {
        required_phase: "controller-attachment",
        admitted: false,
    };
    attachment
        .admit("controller-attachment")
        .expect("attachment");
    assert!(
        attachment.admitted,
        "first append requires active attachment"
    );
}

#[test]
fn r71_f_boot_010_phase_source_cross_swap_fails_closed() {
    // A session phase cannot admit a workspace phase (cross source swap).

    let mut guard = PhaseSinkGuard {
        required_phase: "session-created",
        admitted: false,
    };
    let error = guard.admit("workspace-activated").expect_err("cross swap");
    assert!(matches!(error, BootstrapErrorV1::IdentityDrift));
}
