use super::*;

#[test]
fn approval_action_toggling_covers_all_variants() {
    assert_eq!(ApprovalAction::Allow.toggled(), ApprovalAction::Deny);
    assert_eq!(ApprovalAction::Deny.toggled(), ApprovalAction::Allow);
}

#[test]
fn approval_action_approval_flag_matches_allow_variant() {
    assert!(ApprovalAction::Allow.approved());
    assert!(!ApprovalAction::Deny.approved());
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
