use super::*;

#[test]
fn r71_output_sequence_must_be_monotonic_and_single_eof() {
    validate_frame_sequence(None, 1, false, false).expect("first");
    validate_frame_sequence(Some(1), 2, true, false).expect("second/eof");
    let error = validate_frame_sequence(Some(2), 3, false, true).expect_err("duplicate eof");
    assert!(matches!(error, OutputSupervisionErrorV1::DuplicateEof));
}

#[test]
fn r71_output_frame_loss_is_detected() {
    let error = validate_frame_sequence(Some(1), 3, false, false).expect_err("gap");
    assert!(matches!(error, OutputSupervisionErrorV1::FrameLoss));
}

#[test]
fn r71_backpressure_beyond_deadline_fails_typed() {
    let error = backpressure_allowed(Duration::from_millis(1000), 2000).expect_err("too long");
    assert!(matches!(
        error,
        OutputSupervisionErrorV1::BackpressureTimeout
    ));
    backpressure_allowed(Duration::from_millis(1000), 500).expect("within");
}
