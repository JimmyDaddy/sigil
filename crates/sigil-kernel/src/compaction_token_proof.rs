use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current schema for provider-neutral token-proof contracts.
pub const COMPACTION_TOKEN_PROOF_SCHEMA_VERSION: u16 = 1;

/// Lowest absolute saving required before portable compaction may change the active boundary.
pub const PORTABLE_COMPACTION_MINIMUM_SAVINGS_TOKENS: u64 = 64;
/// Lowest relative saving, in parts per million, required for portable compaction.
pub const PORTABLE_COMPACTION_MINIMUM_SAVINGS_RATIO_PPM: u32 = 50_000;

/// Immutable versioned identity of one provider, wire, tokenizer, or budget profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct VersionedProfileIdentity {
    pub profile_id: String,
    pub revision: u32,
    pub content_hash: String,
}

impl VersionedProfileIdentity {
    /// Creates a self-verifying profile identity from canonical profile content.
    #[must_use]
    pub fn from_content(profile_id: impl Into<String>, revision: u32, content: &[u8]) -> Self {
        Self {
            profile_id: profile_id.into(),
            revision,
            content_hash: format!("sha256:{:x}", Sha256::digest(content)),
        }
    }

    /// Validates stable, bounded profile identity fields.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity is absent, control-bearing, or has an unsupported hash.
    pub fn validate(&self) -> Result<()> {
        validate_bounded("profile id", &self.profile_id, 256)?;
        if self.revision == 0 {
            bail!("profile revision must be non-zero");
        }
        if self.content_hash.len() != 71
            || !self.content_hash.starts_with("sha256:")
            || !self.content_hash[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("profile content hash must be a sha256 digest");
        }
        Ok(())
    }
}

/// The material boundary whose input tokens were measured.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TokenMeasurementScope {
    RenderedTargetInput,
    RenderedSemanticCompressorInput,
}

/// Versioned provider/model profile against which token evidence is valid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TokenMeasurementBinding {
    pub schema_version: u16,
    pub provider_name: String,
    pub model_name: String,
    pub wire_profile: VersionedProfileIdentity,
    pub token_measurement_profile: VersionedProfileIdentity,
    pub hosted_parity_profile: Option<VersionedProfileIdentity>,
}

impl TokenMeasurementBinding {
    /// Validates the profile identity required to reuse token evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when a profile is malformed or the schema is unsupported.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != COMPACTION_TOKEN_PROOF_SCHEMA_VERSION {
            bail!("unsupported compaction token-proof schema version");
        }
        validate_bounded("token provider name", &self.provider_name, 256)?;
        validate_bounded("token model name", &self.model_name, 256)?;
        self.wire_profile.validate()?;
        self.token_measurement_profile.validate()?;
        if let Some(profile) = &self.hosted_parity_profile {
            profile.validate()?;
        }
        Ok(())
    }
}

/// Pre-send evidence for one rendered provider input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum InputTokenEvidence {
    /// Provider-local tokenizer evidence whose wire and hosted-parity profiles are both frozen.
    Exact {
        tokens: u64,
        material_fingerprint: String,
        measurement_scope: TokenMeasurementScope,
        binding: TokenMeasurementBinding,
        provider_model_snapshot: Option<String>,
        provider_system_fingerprint: Option<String>,
    },
    /// A proof-carrying upper bound. It is safe for fit admission but never treated as exact.
    ConservativeUpperBound {
        tokens_upper_bound: u64,
        material_fingerprint: String,
        measurement_scope: TokenMeasurementScope,
        binding: TokenMeasurementBinding,
    },
}

impl InputTokenEvidence {
    /// Returns the conservative input token value used by the fit inequality.
    #[must_use]
    pub fn admission_tokens(&self) -> u64 {
        match self {
            Self::Exact { tokens, .. } => *tokens,
            Self::ConservativeUpperBound {
                tokens_upper_bound, ..
            } => *tokens_upper_bound,
        }
    }

    /// Validates this evidence against the exact frozen request identity and expected profiles.
    ///
    /// # Errors
    ///
    /// Returns an error for missing material, scope/profile drift, or an exact claim without a
    /// frozen hosted-parity profile.
    pub fn validate_for(
        &self,
        expected_material_fingerprint: &str,
        expected_scope: TokenMeasurementScope,
        expected_binding: &TokenMeasurementBinding,
    ) -> Result<()> {
        expected_binding.validate()?;
        let (material_fingerprint, scope, binding) = match self {
            Self::Exact {
                material_fingerprint,
                measurement_scope,
                binding,
                ..
            }
            | Self::ConservativeUpperBound {
                material_fingerprint,
                measurement_scope,
                binding,
                ..
            } => (material_fingerprint, *measurement_scope, binding),
        };
        validate_bounded("token material fingerprint", material_fingerprint, 512)?;
        binding.validate()?;
        if material_fingerprint != expected_material_fingerprint {
            bail!("token evidence material fingerprint does not match frozen request");
        }
        if scope != expected_scope {
            bail!("token evidence measurement scope does not match request purpose");
        }
        if binding != expected_binding {
            bail!("token evidence provider or profile binding drifted");
        }
        match self {
            Self::Exact { binding, .. } if binding.hosted_parity_profile.is_none() => {
                bail!("exact token evidence requires a hosted-parity profile")
            }
            Self::ConservativeUpperBound { binding, .. }
                if binding.hosted_parity_profile.is_some() =>
            {
                bail!("conservative upper-bound evidence must not claim hosted parity")
            }
            Self::Exact { .. } | Self::ConservativeUpperBound { .. } => Ok(()),
        }
    }
}

/// The complete budget for a single input fit decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectiveTokenBudget {
    pub schema_version: u16,
    pub budget_profile: VersionedProfileIdentity,
    pub context_window_tokens: u64,
    pub requested_output_tokens: u64,
    pub safety_buffer_tokens: u64,
}

impl EffectiveTokenBudget {
    /// Validates a complete, non-overflowing request budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the context window is absent or the reserved output/safety budget
    /// already exhausts it.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != COMPACTION_TOKEN_PROOF_SCHEMA_VERSION {
            bail!("unsupported effective token-budget schema version");
        }
        self.budget_profile.validate()?;
        if self.context_window_tokens == 0 {
            bail!("token budget context window must be non-zero");
        }
        let reserved = self
            .requested_output_tokens
            .checked_add(self.safety_buffer_tokens)
            .ok_or_else(|| anyhow::anyhow!("token budget reservation overflowed"))?;
        if reserved >= self.context_window_tokens {
            bail!("token budget reservation exhausts context window");
        }
        Ok(())
    }

    /// Returns whether an input token count fits this complete output and safety reservation.
    ///
    /// # Errors
    ///
    /// Returns an error when the budget is invalid or the complete addition would overflow.
    pub fn admits_input_tokens(&self, input_tokens: u64) -> Result<bool> {
        self.validate()?;
        let total = input_tokens
            .checked_add(self.requested_output_tokens)
            .and_then(|value| value.checked_add(self.safety_buffer_tokens))
            .ok_or_else(|| anyhow::anyhow!("token fit calculation overflowed"))?;
        Ok(total <= self.context_window_tokens)
    }
}

/// Proof-carrying pre-send fit decision for one frozen provider input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RequestFitProof {
    pub schema_version: u16,
    pub input: InputTokenEvidence,
    pub budget: EffectiveTokenBudget,
}

impl RequestFitProof {
    /// Validates profile/material bindings and the exact overflow-safe fit inequality.
    ///
    /// # Errors
    ///
    /// Returns an error when evidence is unproved, stale, or cannot fit the complete request
    /// budget. This method deliberately does not estimate tokens from characters or bytes.
    pub fn validate_for(
        &self,
        expected_material_fingerprint: &str,
        expected_scope: TokenMeasurementScope,
        expected_binding: &TokenMeasurementBinding,
    ) -> Result<()> {
        if self.schema_version != COMPACTION_TOKEN_PROOF_SCHEMA_VERSION {
            bail!("unsupported request-fit proof schema version");
        }
        self.input.validate_for(
            expected_material_fingerprint,
            expected_scope,
            expected_binding,
        )?;
        self.budget.validate()?;
        if !self
            .budget
            .admits_input_tokens(self.input.admission_tokens())?
        {
            bail!("token evidence does not fit the effective request budget");
        }
        Ok(())
    }
}

/// Durable before/after economics proof for one portable checkpoint activation.
///
/// The after side is the `RequestFitProof` stored beside this record. The before side names the
/// separately frozen pre-activation request and carries its exact provider-local token evidence.
/// No rendered request content is persisted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PortableCompactionEconomicsV1 {
    pub schema_version: u16,
    pub before_input: InputTokenEvidence,
    pub minimum_savings_tokens: u64,
    pub minimum_savings_ratio_ppm: u32,
    pub savings_tokens: u64,
    pub savings_ratio_ppm: u32,
}

impl PortableCompactionEconomicsV1 {
    /// Derives one checked-integer savings proof from exact frozen before/after evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when either side is not exact, profile bindings drift, savings are zero or
    /// negative, or either configured minimum is not met.
    pub fn from_before_and_after(
        before_input: InputTokenEvidence,
        before_material_fingerprint: &str,
        after_proof: &RequestFitProof,
        binding: &TokenMeasurementBinding,
        minimum_savings_tokens: u64,
        minimum_savings_ratio_ppm: u32,
    ) -> Result<Self> {
        if minimum_savings_ratio_ppm > 1_000_000 {
            bail!("portable compaction minimum savings ratio exceeds one million ppm");
        }
        before_input.validate_for(
            before_material_fingerprint,
            TokenMeasurementScope::RenderedTargetInput,
            binding,
        )?;
        if !matches!(before_input, InputTokenEvidence::Exact { .. }) {
            bail!("portable compaction before evidence must be exact");
        }
        after_proof.validate_for(
            after_proof.input.material_fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            binding,
        )?;
        if !matches!(after_proof.input, InputTokenEvidence::Exact { .. }) {
            bail!("portable compaction after evidence must be exact");
        }
        let before_tokens = before_input.admission_tokens();
        let after_tokens = after_proof.input.admission_tokens();
        let savings_tokens = before_tokens
            .checked_sub(after_tokens)
            .context("portable compaction produced zero or negative token savings")?;
        if savings_tokens == 0 {
            bail!("portable compaction produced zero token savings");
        }
        if savings_tokens < minimum_savings_tokens {
            bail!("portable compaction savings are below the minimum token threshold");
        }
        let savings_ratio_ppm = savings_tokens
            .checked_mul(1_000_000)
            .context("portable compaction savings ratio overflowed")?
            .checked_div(before_tokens)
            .context("portable compaction before token count is zero")?;
        if savings_ratio_ppm < u64::from(minimum_savings_ratio_ppm) {
            bail!("portable compaction savings are below the minimum ratio threshold");
        }
        Ok(Self {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            before_input,
            minimum_savings_tokens,
            minimum_savings_ratio_ppm,
            savings_tokens,
            savings_ratio_ppm: u32::try_from(savings_ratio_ppm)
                .context("portable compaction savings ratio exceeds u32")?,
        })
    }

    /// Revalidates this persisted economics record against the frozen target proof.
    pub fn validate_for_after(
        &self,
        after_material_fingerprint: &str,
        after_proof: &RequestFitProof,
        binding: &TokenMeasurementBinding,
    ) -> Result<()> {
        if self.schema_version != COMPACTION_TOKEN_PROOF_SCHEMA_VERSION {
            bail!("unsupported portable compaction economics schema version");
        }
        let before_material_fingerprint = self.before_input.material_fingerprint();
        let rebuilt = Self::from_before_and_after(
            self.before_input.clone(),
            before_material_fingerprint,
            after_proof,
            binding,
            self.minimum_savings_tokens,
            self.minimum_savings_ratio_ppm,
        )?;
        if rebuilt.savings_tokens != self.savings_tokens
            || rebuilt.savings_ratio_ppm != self.savings_ratio_ppm
            || after_proof.input.material_fingerprint() != after_material_fingerprint
        {
            bail!("portable compaction economics evidence does not match its before/after proof");
        }
        Ok(())
    }
}

impl InputTokenEvidence {
    /// Returns the process-local frozen request fingerprint carried by this evidence.
    #[must_use]
    pub fn material_fingerprint(&self) -> &str {
        match self {
            Self::Exact {
                material_fingerprint,
                ..
            }
            | Self::ConservativeUpperBound {
                material_fingerprint,
                ..
            } => material_fingerprint,
        }
    }
}

fn validate_bounded(field: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        bail!("{field} must be non-empty, bounded, and control-free");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/compaction_token_proof_tests.rs"]
mod tests;
