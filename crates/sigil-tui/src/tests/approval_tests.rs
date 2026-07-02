use super::*;

#[test]
fn approval_action_cycles_available_variants() {
    assert_eq!(
        ApprovalAction::AllowOnce.next(false, true),
        ApprovalAction::Deny
    );
    assert_eq!(
        ApprovalAction::Deny.next(false, true),
        ApprovalAction::AllowOnce
    );
    assert_eq!(
        ApprovalAction::AllowOnce.next(true, true),
        ApprovalAction::AllowSession
    );
    assert_eq!(
        ApprovalAction::AllowSession.next(true, true),
        ApprovalAction::Deny
    );
    assert_eq!(
        ApprovalAction::Deny.next(true, false),
        ApprovalAction::AllowSession
    );
    assert_eq!(
        ApprovalAction::AllowSession.normalized(false),
        ApprovalAction::AllowOnce
    );
}

#[test]
fn approval_action_approval_flag_matches_allow_variant() {
    assert!(ApprovalAction::AllowOnce.approved());
    assert!(ApprovalAction::AllowSession.approved());
    assert!(ApprovalAction::AllowSession.grants_session());
    assert!(!ApprovalAction::Deny.approved());
}

#[test]
fn approval_action_default_tracks_risk() {
    assert_eq!(
        ApprovalAction::default_for(sigil_kernel::PermissionRisk::Low, false),
        ApprovalAction::AllowOnce
    );
    assert_eq!(
        ApprovalAction::default_for(sigil_kernel::PermissionRisk::Medium, true),
        ApprovalAction::AllowOnce
    );
    assert_eq!(
        ApprovalAction::default_for(sigil_kernel::PermissionRisk::High, true),
        ApprovalAction::AllowOnce
    );
    assert_eq!(
        ApprovalAction::default_for(sigil_kernel::PermissionRisk::High, false),
        ApprovalAction::Deny
    );
    assert_eq!(
        ApprovalAction::default_for(sigil_kernel::PermissionRisk::Destructive, false),
        ApprovalAction::Deny
    );
}

#[test]
fn approval_diff_mode_cycles_and_labels_cover_all_variants() {
    assert_eq!(ApprovalDiffMode::Full.next(), ApprovalDiffMode::CurrentHunk);
    assert_eq!(
        ApprovalDiffMode::CurrentHunk.next(),
        ApprovalDiffMode::ChangedOnly
    );
    assert_eq!(ApprovalDiffMode::ChangedOnly.next(), ApprovalDiffMode::Full);

    assert_eq!(ApprovalDiffMode::Full.label(), "full");
    assert_eq!(ApprovalDiffMode::CurrentHunk.label(), "current-hunk");
    assert_eq!(ApprovalDiffMode::ChangedOnly.label(), "changed-only");
}

#[test]
fn approval_diagnostic_summary_is_clean_when_no_diagnostics() {
    assert!(ApprovalDiagnosticSummary::default().is_clean());

    assert!(
        !ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 0,
        }
        .is_clean()
    );

    assert!(
        !ApprovalDiagnosticSummary {
            errors: 0,
            warnings: 1,
        }
        .is_clean()
    );
}
