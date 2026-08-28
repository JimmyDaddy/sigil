use super::*;

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
