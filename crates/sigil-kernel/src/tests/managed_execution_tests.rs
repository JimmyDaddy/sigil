use super::*;

#[test]
fn historical_output_without_reader_evidence_is_incomplete() {
    let summary = BoundedOutputSummaryV1 {
        observed_bytes: 0,
        retained_bytes: 0,
        retained_payload: Vec::new(),
        content_digest: CanonicalHash::from_bytes([0; 32]),
        source: ManagedOutputSourceV1::Complete,
        truncated: false,
        artifact_ref: None,
    };
    let mut historical = serde_json::to_value(summary).expect("serialized receipt");
    historical
        .as_object_mut()
        .expect("summary object")
        .remove("source");
    let restored: BoundedOutputSummaryV1 = serde_json::from_value(historical).expect("old receipt");
    assert_eq!(restored.source, ManagedOutputSourceV1::Incomplete);
    assert!(!restored.truncated);
}

fn receipt(
    pipeline_outcome: PipelineOutcomeV1,
    verification_evidence: VerificationEvidenceV1,
) -> ExecutionCheckReceiptV1 {
    ExecutionCheckReceiptV1 {
        check_spec_hash: CanonicalHash::from_bytes([1; 32]),
        pipeline_outcome,
        verification_evidence,
        evidence_binding_hash: CanonicalHash::from_bytes([2; 32]),
        shell_profile_hash: CanonicalHash::from_bytes([3; 32]),
    }
}

#[test]
fn final_stage_only_is_never_verification_passed() {
    let receipt = receipt(
        PipelineOutcomeV1::FinalStageOnly { final_exit_code: 0 },
        VerificationEvidenceV1::Sufficient,
    );
    assert!(!receipt.verification_passed());
}

#[test]
fn all_stages_observed_requires_sufficient_evidence() {
    let digest = CanonicalHash::from_bytes([4; 32]);
    assert!(
        receipt(
            PipelineOutcomeV1::AllStagesObserved {
                stage_statuses_digest: digest,
            },
            VerificationEvidenceV1::Sufficient,
        )
        .verification_passed()
    );
    assert!(
        !receipt(
            PipelineOutcomeV1::AllStagesObserved {
                stage_statuses_digest: digest,
            },
            VerificationEvidenceV1::Insufficient {
                reason: VerificationEvidenceReasonV1::UpstreamStatusUnobserved,
            },
        )
        .verification_passed()
    );
}
