use super::*;

#[test]
fn r71_export_ladder_allows_artifact_publish_and_blocks_duplicate() {
    validate_export_phase_ladder(
        SessionExportPhaseV1::Planned,
        SessionExportPhaseV1::ArtifactPrepared,
    )
    .expect("step");
    validate_export_phase_ladder(
        SessionExportPhaseV1::ArtifactPublished,
        SessionExportPhaseV1::Committed,
    )
    .expect("commit");
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::ArtifactPublished,
    )
    .expect_err("duplicate");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_export_external_ladder_is_create_new_no_overwrite() {
    validate_export_phase_ladder(
        SessionExportPhaseV1::ExternalCreated,
        SessionExportPhaseV1::Committed,
    )
    .expect("commit");
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::Planned,
        SessionExportPhaseV1::Committed,
    )
    .expect_err("no jump");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_export_recovery_requires_exact_steps() {
    validate_export_phase_ladder(
        SessionExportPhaseV1::RecoveryStarted,
        SessionExportPhaseV1::RecoverySubjectBound,
    )
    .expect("subject bound");
    validate_export_phase_ladder(
        SessionExportPhaseV1::RecoverySubjectBound,
        SessionExportPhaseV1::RecoverySettled,
    )
    .expect("settled");
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::RecoverySettled,
    )
    .expect_err("no jump");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}
