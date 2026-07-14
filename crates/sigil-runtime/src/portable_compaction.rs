use std::path::Path;

use anyhow::{Context, Result, bail};
use sigil_kernel::{
    FrozenProviderRequestMaterial, PortableTargetRequestMaterial, RequestFitProof,
    TokenMeasurementBinding,
};
use sigil_provider_deepseek::{
    DEFAULT_DEEPSEEK_V4_FLASH_MODEL, DeepSeekV4FlashPortableTargetAdmission,
    DeepSeekV4FlashTokenCounter, default_deepseek_v4_flash_portable_target_output_tokens,
    default_deepseek_v4_flash_tokenizer_cache_path,
};
use sigil_provider_openai_responses::{
    OPENAI_RESPONSES_PORTABLE_TARGET_MODEL, OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS,
};

/// Returns the explicit output cap used by the admitted DeepSeek V4 portable target request.
#[must_use]
pub const fn deepseek_v4_flash_portable_target_output_tokens() -> u32 {
    default_deepseek_v4_flash_portable_target_output_tokens()
}

/// Returns whether a request identity is admitted by the first exact portable-target profile.
#[must_use]
pub fn is_deepseek_v4_flash_portable_target_profile(provider_name: &str, model_name: &str) -> bool {
    provider_name == "deepseek" && model_name == DEFAULT_DEEPSEEK_V4_FLASH_MODEL
}

/// Returns whether a request identity is the only OpenAI Responses profile that may use the
/// server-count overflow-recovery path.
#[must_use]
pub fn is_openai_responses_portable_target_profile(provider_name: &str, model_name: &str) -> bool {
    provider_name == "openai_responses" && model_name == OPENAI_RESPONSES_PORTABLE_TARGET_MODEL
}

/// Returns the explicit output reservation required by an admitted portable target profile.
///
/// A value here only materializes an explicit target request. It does not imply local admission
/// or authorize provider I/O; the caller must still obtain that profile's own exact proof.
#[must_use]
pub fn portable_compaction_target_output_tokens(
    provider_name: &str,
    model_name: &str,
) -> Option<u32> {
    if is_deepseek_v4_flash_portable_target_profile(provider_name, model_name) {
        Some(deepseek_v4_flash_portable_target_output_tokens())
    } else if is_openai_responses_portable_target_profile(provider_name, model_name) {
        Some(OPENAI_RESPONSES_PORTABLE_TARGET_OUTPUT_TOKENS)
    } else {
        None
    }
}

/// Result of an exact local DeepSeek V4 Flash portable-target pressure assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepSeekV4FlashPortableTargetPressure {
    ExactFit {
        binding: TokenMeasurementBinding,
        proof: Box<RequestFitProof>,
    },
    ExceedsBudget {
        input_tokens: u64,
        budget: sigil_kernel::EffectiveTokenBudget,
    },
}

/// Classifies a frozen request against the local default DeepSeek V4 Flash target profile.
///
/// This only opens the checksum-pinned tokenizer already present under `cache_root`. It never
/// downloads an artifact or contacts a provider. An exceeded budget remains a normal local
/// pressure outcome rather than an invalid proof.
///
/// # Errors
///
/// Returns an error when the request is outside the admitted profile, lacks the explicit output
/// reservation, or the verified tokenizer is unavailable.
pub fn deepseek_v4_flash_portable_target_pressure(
    cache_root: &Path,
    frozen_request: &FrozenProviderRequestMaterial,
) -> Result<DeepSeekV4FlashPortableTargetPressure> {
    let request = frozen_request.request();
    if !is_deepseek_v4_flash_portable_target_profile(
        request.provider_name.as_str(),
        request.model_name.as_str(),
    ) {
        bail!("local exact portable target proof is unavailable for this provider/model");
    }
    if request.max_tokens != Some(deepseek_v4_flash_portable_target_output_tokens()) {
        bail!(
            "local exact portable target proof requires explicit max_tokens={}",
            deepseek_v4_flash_portable_target_output_tokens()
        );
    }
    let tokenizer_path = default_deepseek_v4_flash_tokenizer_cache_path(cache_root);
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)
        .with_context(|| {
            format!(
                "verified DeepSeek V4 tokenizer is unavailable at {}",
                tokenizer_path.display()
            )
        })?;
    match counter.default_portable_target_request_admission(frozen_request)? {
        DeepSeekV4FlashPortableTargetAdmission::ExactFit { binding, proof } => {
            Ok(DeepSeekV4FlashPortableTargetPressure::ExactFit { binding, proof })
        }
        DeepSeekV4FlashPortableTargetAdmission::ExceedsBudget {
            input_tokens,
            budget,
        } => Ok(DeepSeekV4FlashPortableTargetPressure::ExceedsBudget {
            input_tokens,
            budget,
        }),
    }
}

/// Proves a frozen request against the local default DeepSeek V4 Flash portable target profile.
///
/// This only opens the checksum-pinned tokenizer already present under `cache_root`. It never
/// downloads an artifact or contacts a provider. The returned binding and proof are both tied to
/// the supplied frozen request, including its explicit output reservation.
///
/// # Errors
///
/// Returns an error when the local verified tokenizer is unavailable or the frozen request cannot
/// satisfy the explicit default DeepSeek portable-compaction target budget.
pub fn deepseek_v4_flash_portable_target_proof(
    cache_root: &Path,
    frozen_request: &FrozenProviderRequestMaterial,
) -> Result<(TokenMeasurementBinding, RequestFitProof)> {
    match deepseek_v4_flash_portable_target_pressure(cache_root, frozen_request)? {
        DeepSeekV4FlashPortableTargetPressure::ExactFit { binding, proof } => Ok((binding, *proof)),
        DeepSeekV4FlashPortableTargetPressure::ExceedsBudget { .. } => {
            bail!("token evidence does not fit the effective request budget")
        }
    }
}

/// Builds the admitted local DeepSeek V4 Flash target material for portable compaction.
///
/// This only opens the checksum-pinned tokenizer already present under `cache_root`. It never
/// downloads an artifact, contacts a provider, or exposes a tokenizer setup action through the
/// compaction confirmation flow.
///
/// # Errors
///
/// Returns an error when the local verified tokenizer is unavailable or the frozen request cannot
/// satisfy the explicit default DeepSeek portable-compaction target budget.
pub fn deepseek_v4_flash_portable_target_material(
    cache_root: &Path,
    frozen_request: FrozenProviderRequestMaterial,
) -> Result<PortableTargetRequestMaterial> {
    let (binding, proof) = deepseek_v4_flash_portable_target_proof(cache_root, &frozen_request)?;
    Ok(PortableTargetRequestMaterial::new(
        frozen_request,
        binding,
        proof,
    ))
}

#[cfg(test)]
#[path = "tests/portable_compaction_tests.rs"]
mod tests;
