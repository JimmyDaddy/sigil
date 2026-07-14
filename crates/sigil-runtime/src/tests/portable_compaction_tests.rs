use anyhow::Result;
use sigil_kernel::{CompletionRequest, FrozenProviderRequestMaterial, ModelMessage};

use super::{
    deepseek_v4_flash_portable_target_material, deepseek_v4_flash_portable_target_proof,
    is_deepseek_v4_flash_portable_target_profile, is_openai_responses_portable_target_profile,
    portable_compaction_target_output_tokens,
};

#[test]
fn portable_target_admission_requires_a_preinstalled_verified_tokenizer() -> Result<()> {
    let cache = tempfile::tempdir()?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        "portable-runtime-test-session",
        CompletionRequest {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            messages: vec![ModelMessage::user("continue the task")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(32_768),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;

    let error = deepseek_v4_flash_portable_target_material(cache.path(), frozen)
        .expect_err("a missing local tokenizer must keep compaction unavailable");
    assert!(
        error
            .to_string()
            .contains("verified DeepSeek V4 tokenizer is unavailable")
    );
    Ok(())
}

#[test]
fn portable_target_profile_is_explicit_and_proof_never_falls_back_to_a_default() -> Result<()> {
    assert!(is_deepseek_v4_flash_portable_target_profile(
        "deepseek",
        "deepseek-v4-flash"
    ));
    assert!(!is_deepseek_v4_flash_portable_target_profile(
        "deepseek",
        "deepseek-v4-pro"
    ));
    assert!(!is_deepseek_v4_flash_portable_target_profile(
        "openai_compat",
        "deepseek-v4-flash"
    ));
    assert!(is_openai_responses_portable_target_profile(
        "openai_responses",
        "gpt-4.1-2025-04-14"
    ));
    assert!(!is_openai_responses_portable_target_profile(
        "openai_responses",
        "gpt-4.1"
    ));
    assert!(!is_openai_responses_portable_target_profile(
        "openai_compat",
        "gpt-4.1-2025-04-14"
    ));

    let cache = tempfile::tempdir()?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        "portable-runtime-test-session",
        CompletionRequest {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            messages: vec![ModelMessage::user("continue the task")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;

    let error = deepseek_v4_flash_portable_target_proof(cache.path(), &frozen)
        .expect_err("an omitted output cap must never be inferred");
    assert!(
        error
            .to_string()
            .contains("requires explicit max_tokens=32768")
    );
    Ok(())
}

#[test]
fn portable_target_output_reservation_is_explicit_for_each_admitted_profile() {
    assert_eq!(
        portable_compaction_target_output_tokens("deepseek", "deepseek-v4-flash"),
        Some(32_768)
    );
    assert_eq!(
        portable_compaction_target_output_tokens("openai_responses", "gpt-4.1-2025-04-14"),
        Some(32_768)
    );
    assert_eq!(
        portable_compaction_target_output_tokens("openai_responses", "gpt-4.1"),
        None
    );
}
