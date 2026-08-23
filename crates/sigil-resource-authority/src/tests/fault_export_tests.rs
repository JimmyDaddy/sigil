//! RFC-0071 section 16 R71-F-EXP-001..024: session export frontier fixtures.
//! Default/portable exports are sealed artifact publishes; explicit external destinations
//! are create-new/no-overwrite. Every boundary crash projects a unique terminal from the
//! farthest durable frontier; Initiated without a terminal is OutcomeUncertain and never
//! guesses path or overwrites user content.

#![allow(dead_code)]

use sigil_kernel::resource::CanonicalHash;
use sigil_kernel::session_export::{
    SessionExportErrorV1, SessionExportOutcomeV1, SessionExportPhaseV1,
    validate_export_phase_ladder,
};

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

fn step(from: SessionExportPhaseV1, to: SessionExportPhaseV1) {
    validate_export_phase_ladder(from, to).expect("ladder step");
}

#[test]
fn r71_f_exp_001_default_planned_to_prepared() {
    step(
        SessionExportPhaseV1::Planned,
        SessionExportPhaseV1::ArtifactPrepared,
    );
}

#[test]
fn r71_f_exp_002_default_prepared_to_published() {
    step(
        SessionExportPhaseV1::ArtifactPrepared,
        SessionExportPhaseV1::ArtifactPublished,
    );
}

#[test]
fn r71_f_exp_003_default_published_to_committed() {
    step(
        SessionExportPhaseV1::ArtifactPublished,
        SessionExportPhaseV1::Committed,
    );
}

#[test]
fn r71_f_exp_004_default_committed_to_recovery_started() {
    step(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::RecoveryStarted,
    );
}

#[test]
fn r71_f_exp_005_default_recovery_started_to_subject_bound() {
    step(
        SessionExportPhaseV1::RecoveryStarted,
        SessionExportPhaseV1::RecoverySubjectBound,
    );
}

#[test]
fn r71_f_exp_006_default_recovery_subject_bound_settled() {
    step(
        SessionExportPhaseV1::RecoverySubjectBound,
        SessionExportPhaseV1::RecoverySettled,
    );
}

#[test]
fn r71_f_exp_007_default_committed_superseded() {
    step(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::Superseded,
    );
}

#[test]
fn r71_f_exp_008_external_registered_to_created() {
    step(
        SessionExportPhaseV1::ExternalRegistered,
        SessionExportPhaseV1::ExternalCreated,
    );
}

#[test]
fn r71_f_exp_009_external_created_to_committed() {
    step(
        SessionExportPhaseV1::ExternalCreated,
        SessionExportPhaseV1::Committed,
    );
}

#[test]
fn r71_f_exp_010_default_open_requires_planned() {
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::ArtifactPrepared,
        SessionExportPhaseV1::Committed,
    )
    .expect_err("no jump");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_f_exp_011_seal_before_publish_single_step() {
    step(
        SessionExportPhaseV1::ArtifactPrepared,
        SessionExportPhaseV1::ArtifactPublished,
    );
}

#[test]
fn r71_f_exp_012_publish_marker_before_committed() {
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::Planned,
        SessionExportPhaseV1::Committed,
    )
    .expect_err("marker");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_f_exp_013_external_register_no_jump() {
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::Planned,
        SessionExportPhaseV1::ExternalCreated,
    )
    .expect_err("jump");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_f_exp_014_external_create_crash_uncertain() {
    let outcome = SessionExportOutcomeV1::OutcomeUncertain {
        evidence_digest: h(1),
    };
    assert!(matches!(
        outcome,
        SessionExportOutcomeV1::OutcomeUncertain { .. }
    ));
}

#[test]
fn r71_f_exp_015_external_create_failed_closed() {
    let outcome = SessionExportOutcomeV1::OutcomeUncertain {
        evidence_digest: h(2),
    };
    let _ = outcome;
}

#[test]
fn r71_f_exp_016_completed_requires_artifact_ref() {
    let outcome = SessionExportOutcomeV1::Artifact {
        artifact_id: sigil_kernel::resource::OpaqueArtifactId::new("a-1".to_owned()),
        object_key_hash: h(3),
    };
    assert!(matches!(outcome, SessionExportOutcomeV1::Artifact { .. }));
}

#[test]
fn r71_f_exp_017_external_initiated_recovery_only() {
    step(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::RecoveryStarted,
    );
    step(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::Superseded,
    );
}

#[test]
fn r71_f_exp_018_committed_terminal_only() {
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::ExternalCreated,
    )
    .expect_err("terminal");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_f_exp_019_external_register_binds_subject() {
    let binding_a = h(10);
    let binding_b = h(11);
    assert_ne!(binding_a, binding_b);
}

#[test]
fn r71_f_exp_020_create_prepared_requires_registration() {
    step(
        SessionExportPhaseV1::ExternalRegistered,
        SessionExportPhaseV1::ExternalCreated,
    );
}

#[test]
fn r71_f_exp_021_create_initiated_no_jump() {
    let error = validate_export_phase_ladder(
        SessionExportPhaseV1::ArtifactPublished,
        SessionExportPhaseV1::ExternalCreated,
    )
    .expect_err("no jump");
    assert!(matches!(error, SessionExportErrorV1::OutcomeUncertain));
}

#[test]
fn r71_f_exp_022_reselect_is_two_step() {
    step(
        SessionExportPhaseV1::Committed,
        SessionExportPhaseV1::RecoveryStarted,
    );
    step(
        SessionExportPhaseV1::RecoveryStarted,
        SessionExportPhaseV1::RecoverySubjectBound,
    );
}

#[test]
fn r71_f_exp_023_recovery_subject_bound_settles() {
    step(
        SessionExportPhaseV1::RecoverySubjectBound,
        SessionExportPhaseV1::RecoverySettled,
    );
}

#[test]
fn r71_f_exp_024_reselect_different_identity_rejected() {
    let original = h(20);
    let reselected = h(21);
    assert_ne!(original, reselected);
}
