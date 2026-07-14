use anyhow::Result;

use super::*;

fn profile(profile_id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(profile_id, 1, profile_id.as_bytes())
}

fn exact_binding() -> TokenMeasurementBinding {
    TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4".to_owned(),
        wire_profile: profile("deepseek-chat-wire"),
        token_measurement_profile: profile("deepseek-tokenizer"),
        hosted_parity_profile: Some(profile("deepseek-hosted-parity")),
    }
}

fn budget() -> EffectiveTokenBudget {
    EffectiveTokenBudget {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        budget_profile: profile("deepseek-budget"),
        context_window_tokens: 100,
        requested_output_tokens: 20,
        safety_buffer_tokens: 10,
    }
}

#[test]
fn exact_token_proof_requires_matching_material_scope_and_hosted_parity() -> Result<()> {
    let binding = exact_binding();
    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: InputTokenEvidence::Exact {
            tokens: 70,
            material_fingerprint: "hmac-sha256:material".to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: Some("2026-07-14".to_owned()),
            provider_system_fingerprint: Some("fp-test".to_owned()),
        },
        budget: budget(),
    };

    proof.validate_for(
        "hmac-sha256:material",
        TokenMeasurementScope::RenderedTargetInput,
        &binding,
    )?;
    assert!(
        proof
            .validate_for(
                "hmac-sha256:other",
                TokenMeasurementScope::RenderedTargetInput,
                &binding,
            )
            .is_err()
    );
    assert!(
        proof
            .validate_for(
                "hmac-sha256:material",
                TokenMeasurementScope::RenderedSemanticCompressorInput,
                &binding,
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn effective_budget_reports_fit_without_reconstructing_provider_defaults() -> Result<()> {
    let budget = budget();

    assert!(budget.admits_input_tokens(70)?);
    assert!(!budget.admits_input_tokens(71)?);
    Ok(())
}

#[test]
fn upper_bound_proof_rejects_hosted_parity_and_budget_overflow() {
    let mut binding = exact_binding();
    binding.hosted_parity_profile = None;
    let mut proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: InputTokenEvidence::ConservativeUpperBound {
            tokens_upper_bound: 70,
            material_fingerprint: "hmac-sha256:material".to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
        },
        budget: budget(),
    };
    assert!(
        proof
            .validate_for(
                "hmac-sha256:material",
                TokenMeasurementScope::RenderedTargetInput,
                &binding,
            )
            .is_ok()
    );

    proof.budget.safety_buffer_tokens = 11;
    assert!(
        proof
            .validate_for(
                "hmac-sha256:material",
                TokenMeasurementScope::RenderedTargetInput,
                &binding,
            )
            .is_err()
    );

    let mut parity_binding = binding.clone();
    parity_binding.hosted_parity_profile = Some(profile("invalid-for-upper-bound"));
    proof.input = InputTokenEvidence::ConservativeUpperBound {
        tokens_upper_bound: 60,
        material_fingerprint: "hmac-sha256:material".to_owned(),
        measurement_scope: TokenMeasurementScope::RenderedTargetInput,
        binding: parity_binding.clone(),
    };
    proof.budget.safety_buffer_tokens = 10;
    assert!(
        proof
            .validate_for(
                "hmac-sha256:material",
                TokenMeasurementScope::RenderedTargetInput,
                &parity_binding,
            )
            .is_err()
    );
}
