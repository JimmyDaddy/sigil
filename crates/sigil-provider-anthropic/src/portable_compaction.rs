//! Exact input counting for the explicitly qualified official DeepSeek Messages route.

use anyhow::{Context, Result, ensure};
use sigil_kernel::provider::PortableCompactionRequestRole;
use sigil_kernel::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CacheMode, CacheUsageCapabilities, EffectiveTokenBudget,
    FrozenProviderRequestMaterial, InputTokenEvidence, PortableTargetRequestMaterial, Provider,
    ProviderContextCapabilities, ProviderStreamTimeoutState, RequestFitProof, SecretRedactor,
    TokenMeasurementBinding, TokenMeasurementScope, VersionedProfileIdentity,
    timeout_provider_request, timeout_provider_stream_next,
};

use crate::{
    AnthropicProvider,
    hosted_search::AnthropicHostedContinuationStore,
    provider::provider_timeout_error,
    request::{AnthropicCachePolicy, build_messages_request_with_continuations_at_endpoint},
};

/// Model key measured on the official endpoint for every frozen request; not an immutable snapshot.
pub const DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_MODEL: &str = "deepseek-flash";
/// Explicit output reservation used by this portable target profile.
pub const DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_OUTPUT_TOKENS: u32 = 32_768;
const MAX_COUNT_RESPONSE_BYTES: usize = 64 * 1024;

impl AnthropicProvider {
    pub(crate) fn deepseek_portable_profile(&self, model: &str) -> bool {
        self.config.base_url.trim_end_matches('/') == "https://api.deepseek.com/anthropic"
            && model == DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_MODEL
            && self.config.anthropic_version == "2023-06-01"
            && self
                .config
                .beta_headers
                .iter()
                .all(|header| header.trim().is_empty())
    }

    pub(crate) async fn deepseek_portable_target(
        &self,
        frozen: FrozenProviderRequestMaterial,
        role: PortableCompactionRequestRole,
    ) -> Result<PortableTargetRequestMaterial> {
        let request = frozen.request();
        ensure!(
            self.deepseek_portable_profile(&request.model_name),
            "exact Messages token proof requires the qualified official endpoint, model and headers"
        );
        ensure!(
            request.provider_name == self.name(),
            "Messages token proof provider mismatch"
        );
        ensure!(
            request.max_tokens == Some(DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_OUTPUT_TOKENS),
            "Messages token proof requires explicit portable output reservation"
        );
        ensure!(
            request.hosted_tools.is_empty()
                && request
                    .continuation_states
                    .iter()
                    .all(|state| state.provider_name == "anthropic"
                        && state.state_kind == crate::thinking_replay::STATE_KIND),
            "Messages token proof does not admit another provider continuation or hosted tools"
        );
        let mut body = build_messages_request_with_continuations_at_endpoint(
            request,
            self.config.max_tokens,
            &AnthropicHostedContinuationStore::default(),
            AnthropicCachePolicy::Disabled,
            &self.config.base_url,
        )?
        .body;
        crate::thinking_replay::apply(&mut body, request)?;
        let mut body =
            serde_json::to_value(body).context("failed to encode Messages count input")?;
        body.as_object_mut()
            .context("Messages count input must be an object")?
            .remove("stream");
        let response = timeout_provider_request(
            self.post_json_with_required_beta(
                &format!("{}/count_tokens", self.messages_url()),
                &body,
                &[],
            ),
            self.timeouts,
        )
        .await
        .map_err(|phase| {
            provider_timeout_error(phase, self.timeouts, self.name(), &request.model_name)
        })??;
        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut state = ProviderStreamTimeoutState::new(self.timeouts);
        let mut payload = Vec::new();
        loop {
            match timeout_provider_stream_next(&mut stream, self.timeouts, &mut state).await {
                Ok(Some(Ok(bytes))) => {
                    ensure!(
                        payload.len().saturating_add(bytes.len()) <= MAX_COUNT_RESPONSE_BYTES,
                        "Messages token count response exceeds its size limit"
                    );
                    payload.extend_from_slice(&bytes);
                }
                Ok(Some(Err(error))) => {
                    return Err(error).context("failed to read Messages token count");
                }
                Err(phase) => {
                    return Err(provider_timeout_error(
                        phase,
                        self.timeouts,
                        self.name(),
                        &request.model_name,
                    ));
                }
                Ok(None) => break,
            }
        }
        if !status.is_success() {
            let redactor = SecretRedactor::from_values([self.api_key()?]);
            return Err(crate::errors::classify_status(
                status.as_u16(),
                &redactor.redact_text(&String::from_utf8_lossy(&payload)),
            )
            .into());
        }
        #[derive(serde::Deserialize)]
        struct Count {
            input_tokens: u64,
        }
        let count: Count =
            serde_json::from_slice(&payload).context("invalid Messages token count response")?;
        let binding = token_binding();
        let proof = RequestFitProof {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input: InputTokenEvidence::Exact {
                tokens: count.input_tokens, material_fingerprint: frozen.fingerprint().to_owned(),
                measurement_scope: TokenMeasurementScope::RenderedTargetInput, binding: binding.clone(),
                // The API exposes a moving model key, not a stable model revision or system hash.
                provider_model_snapshot: None, provider_system_fingerprint: None,
            },
            budget: EffectiveTokenBudget {
                schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                budget_profile: VersionedProfileIdentity::from_content("deepseek-messages-portable-budget", 1,
                    b"model=deepseek-flash;context=1048576;output=32768;safety=8192;docs=2026-09-27"),
                context_window_tokens: 1_048_576, requested_output_tokens: u64::from(DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_OUTPUT_TOKENS),
                safety_buffer_tokens: 8_192,
            },
        };
        proof.input.validate_for(
            frozen.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &binding,
        )?;
        proof.budget.validate()?;
        if role == PortableCompactionRequestRole::Target {
            proof.validate_for(
                frozen.fingerprint(),
                TokenMeasurementScope::RenderedTargetInput,
                &binding,
            )?;
        }
        Ok(PortableTargetRequestMaterial::new(frozen, binding, proof))
    }
}

pub(crate) fn deepseek_context_capabilities() -> ProviderContextCapabilities {
    ProviderContextCapabilities {
        cache_mode: CacheMode::ImplicitPrefix,
        cache_usage_fields: CacheUsageCapabilities {
            read_tokens: true,
            write_tokens: false,
            miss_tokens: true,
        },
        ..ProviderContextCapabilities::default()
    }
}

fn token_binding() -> TokenMeasurementBinding {
    TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "anthropic".to_owned(), model_name: DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_MODEL.to_owned(),
        wire_profile: VersionedProfileIdentity::from_content("deepseek-messages-wire", 1,
            b"endpoint=https://api.deepseek.com/anthropic;version=2023-06-01;beta=none;cache=implicit;messages-v1;count=same-body-minus-stream"),
        token_measurement_profile: VersionedProfileIdentity::from_content("deepseek-messages-server-count", 1,
            b"POST /anthropic/v1/messages/count_tokens;exact=input_tokens;default-thinking;fresh-per-frozen-request"),
        hosted_parity_profile: Some(VersionedProfileIdentity::from_content("deepseek-messages-count-parity", 1,
            b"2026-09-27:text,system-unicode,special-literals,default-thinking,tools-auto,tool-pair;count=sum(input+cache-read+cache-creation)")),
    }
}

/// Parses only the observed official validation envelope, including its token algebra.
/// The caller additionally binds the exact endpoint/model/headers and rejects truncated bodies.
pub(crate) fn context_window_rejection(body: &str, requested_output: u32) -> bool {
    let parse = || -> Option<()> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        let error = value.get("error")?;
        if error.get("type")?.as_str()? != "invalid_request_error"
            || error.get("code")?.as_str()? != "invalid_request_error"
        {
            return None;
        }
        let message = error.get("message")?.as_str()?;
        let rest = message.strip_prefix("This model's maximum context length is ")?;
        let (window, rest) = rest.split_once(" tokens. However, you requested ")?;
        let (total, rest) = rest.split_once(" tokens (")?;
        let (input, rest) = rest.split_once(" in the messages, ")?;
        let (output, suffix) = rest.split_once(
            " in the completion). Please reduce the length of the messages or completion.",
        )?;
        let window: u64 = window.parse().ok()?;
        let total: u64 = total.parse().ok()?;
        let input: u64 = input.parse().ok()?;
        let output: u64 = output.parse().ok()?;
        if window != 1_048_576
            || output != u64::from(requested_output)
            || input.checked_add(output)? != total
            || total <= window
        {
            return None;
        }
        if !suffix.is_empty() {
            let id = suffix.strip_prefix(" (request_id: ")?.strip_suffix(')')?;
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return None;
            }
        }
        Some(())
    };
    parse().is_some()
}

#[cfg(test)]
#[path = "tests/portable_compaction_tests.rs"]
mod tests;
