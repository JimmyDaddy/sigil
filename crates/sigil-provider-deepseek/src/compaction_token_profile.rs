use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, EffectiveTokenBudget, FrozenProviderRequestMaterial,
    InputTokenEvidence, RequestFitProof, TokenMeasurementBinding, TokenMeasurementScope,
    VersionedProfileIdentity,
};
use tokenizers::Tokenizer;

use crate::{
    DeepSeekProviderQuirkProfile, StrictToolsMode, models::DeepSeekChatCompletionRequest,
    request::build_chat_request,
};

/// The only DeepSeek model admitted by the first exact V2 compaction profile.
pub const DEFAULT_DEEPSEEK_V4_FLASH_MODEL: &str = "deepseek-v4-flash";
/// Immutable upstream revision that supplied both the tokenizer and canonical prompt encoder.
pub const DEFAULT_DEEPSEEK_V4_FLASH_REVISION: &str = "60d8d70770c6776ff598c94bb586a859a38244f1";
/// SHA-256 of DeepSeek's official V4 tokenizer artifact at the pinned revision.
pub const DEFAULT_DEEPSEEK_V4_FLASH_TOKENIZER_SHA256: &str =
    "8f9f37ca37fdc4f5fd36d5cf4d3b0e8392edb4e894fd10cc0d70b4957c8633cf";
/// SHA-256 of DeepSeek's official V4 canonical prompt encoder at the pinned revision.
pub const DEFAULT_DEEPSEEK_V4_FLASH_ENCODER_SHA256: &str =
    "bdbd57c132a1b3725042323d02b98b9d1df28e5f388f134399555d041f5055e0";
/// Backend fingerprint observed by every admitted hosted-parity corpus request.
///
/// A change requires a deliberate profile re-admission instead of silently reusing token proof
/// generated against a different hosted model backend.
pub const DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT: &str =
    "fp_8b330d02d0_prod0820_fp8_kvcache_20260402";
/// Hard cap for the public tokenizer artifact before checksum validation.
pub const MAX_DEEPSEEK_V4_FLASH_TOKENIZER_BYTES: usize = 16 * 1024 * 1024;
/// Explicit output reservation carried by the first portable V2 target request.
///
/// This is a Sigil product policy, not an inferred DeepSeek API default. It keeps the compaction
/// proof honest while reserving a useful coding-agent response budget below the documented model
/// maximum.
pub const DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS: u32 = 32_768;
/// Additional context reservation held back from the first portable V2 target request.
pub const DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS: u32 = 8_192;

const TOKENIZER_FILE_NAME: &str = "tokenizer.json";
const TOKENIZER_CACHE_LEAF: &str = "provider-profiles/deepseek-v4-flash";
const TOKENIZER_PROFILE_REVISION: u32 = 1;
const WIRE_PROFILE_REVISION: u32 = 2;
const HOSTED_PARITY_PROFILE_REVISION: u32 = 1;
const PORTABLE_TARGET_BUDGET_PROFILE_REVISION: u32 = 1;
const HOSTED_PARITY_VALIDATED_ON: &str = "2026-07-14";
const DEFAULT_API_ROUTE: &str = "https://api.deepseek.com";
const DEFAULT_BETA_API_ROUTE: &str = "https://api.deepseek.com/beta";
const HOSTED_PARITY_CORPUS: &str = "latin-baseline:12,cjk-emoji:11,tool-schema:296,assistant-tool-result:355,max-reasoning:87,strict-auto:296,strict-off:296";
const DEFAULT_DEEPSEEK_V4_FLASH_CONTEXT_WINDOW_TOKENS: u64 = 1_000_000;

const BOS_TOKEN: &str = "<｜begin▁of▁sentence｜>";
const EOS_TOKEN: &str = "<｜end▁of▁sentence｜>";
const THINKING_START_TOKEN: &str = "<think>";
const THINKING_END_TOKEN: &str = "</think>";
const USER_TOKEN: &str = "<｜User｜>";
const ASSISTANT_TOKEN: &str = "<｜Assistant｜>";

const MAX_REASONING_PREFIX: &str = "Reasoning Effort: Absolute maximum with no shortcuts permitted.\n\
You MUST be very thorough in your thinking and comprehensively decompose the problem to resolve the root cause, rigorously stress-testing your logic against all potential paths, edge cases, and adversarial scenarios.\n\
Explicitly write out your entire deliberation process, documenting every intermediate step, considered alternative, and rejected hypothesis to ensure absolutely no assumption is left unchecked.\n\n";

const TOOLS_TEMPLATE: &str = "## Tools\n\n\
You have access to a set of tools to help answer the user's question. You can invoke tools by writing a \"<｜DSML｜tool_calls>\" block like the following:\n\n\
<｜DSML｜tool_calls>\n\
<｜DSML｜invoke name=\"$TOOL_NAME\">\n\
<｜DSML｜parameter name=\"$PARAMETER_NAME\" string=\"true|false\">$PARAMETER_VALUE</｜DSML｜parameter>\n\
...\n\
</｜DSML｜invoke>\n\
<｜DSML｜invoke name=\"$TOOL_NAME2\">\n\
...\n\
</｜DSML｜invoke>\n\
</｜DSML｜tool_calls>\n\n\
String parameters should be specified as is and set `string=\"true\"`. For all other types (numbers, booleans, arrays, objects), pass the value in JSON format and set `string=\"false\"`.\n\n\
If thinking_mode is enabled (triggered by <think>), you MUST output your complete reasoning inside <think>...</think> BEFORE any tool calls or final response.\n\n\
Otherwise, output directly after </think> with tool calls or final response.\n\n\
### Available Tool Schemas\n\n";

const TOOLS_TEMPLATE_SUFFIX: &str = "\n\nYou MUST strictly follow the above defined tool name and parameter schemas to invoke tool calls.\n";

/// Returns the immutable Hugging Face URL for the verified public tokenizer artifact.
#[must_use]
pub fn default_deepseek_v4_flash_tokenizer_url() -> String {
    format!(
        "https://huggingface.co/deepseek-ai/DeepSeek-V4-Flash/resolve/{DEFAULT_DEEPSEEK_V4_FLASH_REVISION}/{TOKENIZER_FILE_NAME}?download=true"
    )
}

/// Returns the cache path for the pinned public tokenizer under a user cache root.
#[must_use]
pub fn default_deepseek_v4_flash_tokenizer_cache_path(cache_root: &Path) -> PathBuf {
    cache_root
        .join(TOKENIZER_CACHE_LEAF)
        .join(DEFAULT_DEEPSEEK_V4_FLASH_REVISION)
        .join(TOKENIZER_FILE_NAME)
}

/// Provider-local exact token counter for the admitted DeepSeek V4 Flash wire profile.
///
/// The counter holds tokenizer state only in process. It never persists rendered prompts, request
/// bytes, or token IDs. It does not prove hosted token parity and must not be used to admit a
/// portable checkpoint until a verified hosted-parity corpus is installed.
pub struct DeepSeekV4FlashTokenCounter {
    tokenizer: Tokenizer,
}

/// Exact local admission result for the default DeepSeek V4 Flash portable target profile.
///
/// `ExceedsBudget` is safe to expose to a local pressure controller: it contains only measured
/// token counts and the versioned public budget, never rendered prompt material or tokenizer
/// output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepSeekV4FlashPortableTargetAdmission {
    ExactFit {
        binding: TokenMeasurementBinding,
        proof: Box<RequestFitProof>,
    },
    ExceedsBudget {
        input_tokens: u64,
        budget: EffectiveTokenBudget,
    },
}

impl DeepSeekV4FlashTokenCounter {
    /// Loads the counter only from the exact official tokenizer artifact pinned by this profile.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact is oversized, has a different checksum, or cannot be
    /// parsed as a Hugging Face tokenizer.
    pub fn from_official_tokenizer_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_DEEPSEEK_V4_FLASH_TOKENIZER_BYTES {
            bail!("DeepSeek V4 tokenizer artifact exceeds the configured size limit");
        }
        let digest = sha256_hex(bytes);
        if digest != DEFAULT_DEEPSEEK_V4_FLASH_TOKENIZER_SHA256 {
            bail!("DeepSeek V4 tokenizer artifact checksum does not match the pinned profile");
        }
        let tokenizer = Tokenizer::from_bytes(bytes).map_err(|error| {
            anyhow!("failed to parse the verified DeepSeek V4 tokenizer artifact: {error}")
        })?;
        Ok(Self { tokenizer })
    }

    /// Reads and validates the pinned artifact from an existing local cache path.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be read or its content is not the pinned artifact.
    pub fn from_official_tokenizer_path(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| {
            format!(
                "failed to read DeepSeek V4 tokenizer from {}",
                path.display()
            )
        })?;
        Self::from_official_tokenizer_bytes(&bytes)
    }

    /// Counts the complete admitted DeepSeek chat input represented by this frozen request.
    ///
    /// This is deliberately not a generic Chat Completions estimator: it supports only the
    /// default V4 Flash profile and fails closed for hosted tools or another provider/model.
    ///
    /// # Errors
    ///
    /// Returns an error when the frozen request cannot use this profile or when canonical prompt
    /// rendering/tokenization fails.
    pub fn count_frozen_target_input(&self, frozen: &FrozenProviderRequestMaterial) -> Result<u64> {
        let request = frozen.request();
        validate_default_profile_request(
            request.provider_name.as_str(),
            request.model_name.as_str(),
        )?;
        if !request.hosted_tools.is_empty() {
            bail!("DeepSeek V4 exact compaction profile does not support hosted tool requests");
        }
        let prepared = build_chat_request(
            request,
            None,
            StrictToolsMode::Auto,
            &DeepSeekProviderQuirkProfile::default(),
        )?;
        self.count_prepared_chat_input(&prepared.body)
    }

    /// Produces exact input evidence for one frozen DeepSeek V4 Flash continuation request.
    ///
    /// The evidence is tied to the target material fingerprint and the admitted hosted-parity
    /// corpus. It contains no raw prompt or tokenizer output.
    ///
    /// # Errors
    ///
    /// Returns an error when the frozen request falls outside the admitted profile or cannot be
    /// rendered and tokenized exactly.
    pub fn exact_target_input_evidence(
        &self,
        frozen: &FrozenProviderRequestMaterial,
    ) -> Result<InputTokenEvidence> {
        let binding = default_deepseek_v4_flash_token_binding();
        let evidence = InputTokenEvidence::Exact {
            tokens: self.count_frozen_target_input(frozen)?,
            material_fingerprint: frozen.fingerprint().to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: Some(format!(
                "{DEFAULT_DEEPSEEK_V4_FLASH_MODEL}@{DEFAULT_DEEPSEEK_V4_FLASH_REVISION}"
            )),
            provider_system_fingerprint: Some(
                DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT.to_owned(),
            ),
        };
        evidence.validate_for(
            frozen.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &binding,
        )?;
        Ok(evidence)
    }

    /// Produces a proven exact fit decision for one frozen DeepSeek V4 Flash continuation request.
    ///
    /// Callers must supply the complete effective context/output/safety budget. This provider
    /// profile intentionally does not infer a budget from request defaults or UI configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is outside the admitted profile, the complete budget is
    /// invalid, or the exact input cannot fit the supplied budget.
    pub fn exact_target_request_fit(
        &self,
        frozen: &FrozenProviderRequestMaterial,
        budget: EffectiveTokenBudget,
    ) -> Result<RequestFitProof> {
        let binding = default_deepseek_v4_flash_token_binding();
        let proof = RequestFitProof {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input: self.exact_target_input_evidence(frozen)?,
            budget,
        };
        proof.validate_for(
            frozen.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &binding,
        )?;
        Ok(proof)
    }

    /// Produces the admitted portable-compaction target proof for the default V4 Flash profile.
    ///
    /// The frozen request must carry the explicit output cap returned by
    /// [`default_deepseek_v4_flash_portable_target_output_tokens`]. This deliberately rejects an
    /// omitted `max_tokens` value instead of guessing a provider-side default.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is outside the admitted profile, omits or changes the
    /// portable target output cap, or cannot fit the complete explicit budget.
    pub fn exact_default_portable_target_request_fit(
        &self,
        frozen: &FrozenProviderRequestMaterial,
    ) -> Result<(TokenMeasurementBinding, RequestFitProof)> {
        match self.default_portable_target_request_admission(frozen)? {
            DeepSeekV4FlashPortableTargetAdmission::ExactFit { binding, proof } => {
                Ok((binding, *proof))
            }
            DeepSeekV4FlashPortableTargetAdmission::ExceedsBudget { .. } => {
                bail!("token evidence does not fit the effective request budget")
            }
        }
    }

    /// Classifies a frozen request against the default V4 Flash portable target profile.
    ///
    /// The request must carry the profile's explicit output cap. A non-fitting result returns the
    /// measured token count and complete versioned budget so callers can decide whether a
    /// separate compacted target should be preflighted; it never returns an invalid fit proof.
    ///
    /// # Errors
    ///
    /// Returns an error when the frozen request is outside the admitted provider/model/wire
    /// profile, omits the explicit output cap, or cannot be rendered and tokenized exactly.
    pub fn default_portable_target_request_admission(
        &self,
        frozen: &FrozenProviderRequestMaterial,
    ) -> Result<DeepSeekV4FlashPortableTargetAdmission> {
        if frozen.request().max_tokens
            != Some(DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS)
        {
            bail!(
                "portable DeepSeek V4 target request must set max_tokens to {}",
                DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS
            );
        }
        let binding = default_deepseek_v4_flash_token_binding();
        let budget = default_deepseek_v4_flash_portable_target_budget();
        let input = self.exact_target_input_evidence(frozen)?;
        let input_tokens = input.admission_tokens();
        if !budget.admits_input_tokens(input_tokens)? {
            return Ok(DeepSeekV4FlashPortableTargetAdmission::ExceedsBudget {
                input_tokens,
                budget,
            });
        }
        let proof = RequestFitProof {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input,
            budget,
        };
        proof.validate_for(
            frozen.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &binding,
        )?;
        Ok(DeepSeekV4FlashPortableTargetAdmission::ExactFit {
            binding,
            proof: Box::new(proof),
        })
    }

    fn count_prepared_chat_input(&self, request: &DeepSeekChatCompletionRequest) -> Result<u64> {
        let rendered = render_v4_chat_prompt(request)?;
        let encoding = self
            .tokenizer
            .encode_fast(rendered, false)
            .map_err(|error| {
                anyhow!("failed to tokenize canonical DeepSeek V4 chat input: {error}")
            })?;
        u64::try_from(encoding.len()).context("DeepSeek V4 token count exceeds u64")
    }
}

/// Downloads the public pinned artifact into the supplied user cache root and validates it.
///
/// The request contains no session, prompt, model API key, or workspace data. Product surfaces
/// must still obtain any required user egress consent before calling this helper.
///
/// # Errors
///
/// Returns an error when a pre-existing cache entry is invalid, the public download is too large,
/// its checksum differs, or the cache cannot be atomically populated.
pub async fn download_default_deepseek_v4_flash_tokenizer(
    client: &reqwest::Client,
    cache_root: &Path,
) -> Result<PathBuf> {
    let target = default_deepseek_v4_flash_tokenizer_cache_path(cache_root);
    if target.exists() {
        DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&target)?;
        return Ok(target);
    }

    let response = client
        .get(default_deepseek_v4_flash_tokenizer_url())
        .send()
        .await
        .context("failed to download the pinned DeepSeek V4 tokenizer artifact")?
        .error_for_status()
        .context("DeepSeek V4 tokenizer artifact download returned an error status")?;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed while reading the DeepSeek V4 tokenizer artifact")?;
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| anyhow!("DeepSeek V4 tokenizer artifact size overflowed"))?;
        if next_len > MAX_DEEPSEEK_V4_FLASH_TOKENIZER_BYTES {
            bail!("DeepSeek V4 tokenizer artifact exceeds the configured size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let _counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_bytes(&bytes)?;

    let parent = target
        .parent()
        .context("DeepSeek V4 tokenizer cache path has no parent directory")?;
    tokio::fs::create_dir_all(parent).await.with_context(|| {
        format!(
            "failed to create DeepSeek V4 tokenizer cache at {}",
            parent.display()
        )
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch")?
        .as_nanos();
    let temporary = parent.join(format!(
        ".{TOKENIZER_FILE_NAME}.partial-{}-{nonce}",
        std::process::id()
    ));
    tokio::fs::write(&temporary, &bytes)
        .await
        .with_context(|| {
            format!(
                "failed to stage DeepSeek V4 tokenizer at {}",
                temporary.display()
            )
        })?;
    match tokio::fs::rename(&temporary, &target).await {
        Ok(()) => {}
        Err(_error) if target.exists() => {
            let _ = tokio::fs::remove_file(&temporary).await;
            DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&target)?;
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error).with_context(|| {
                format!(
                    "failed to atomically install DeepSeek V4 tokenizer at {}",
                    target.display()
                )
            });
        }
    }
    DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&target)?;
    Ok(target)
}

/// Returns the provider/model/wire/tokenizer/hosted-parity identity for the admitted profile.
///
/// The parity profile binds the routes, backend fingerprint, and full public corpus verified
/// against DeepSeek streamed `prompt_tokens`. Any material change needs a new revision.
#[must_use]
pub fn default_deepseek_v4_flash_token_binding() -> TokenMeasurementBinding {
    TokenMeasurementBinding {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "deepseek".to_owned(),
        model_name: DEFAULT_DEEPSEEK_V4_FLASH_MODEL.to_owned(),
        wire_profile: VersionedProfileIdentity::from_content(
            "deepseek-v4-flash-chat-wire",
            WIRE_PROFILE_REVISION,
            format!(
                "source=deepseek-ai/DeepSeek-V4-Flash@{DEFAULT_DEEPSEEK_V4_FLASH_REVISION};encoder_sha256={DEFAULT_DEEPSEEK_V4_FLASH_ENCODER_SHA256};thinking=enabled;drop_thinking=true;tools=dsml-v1;strict=function.strict transport-only"
            )
            .as_bytes(),
        ),
        token_measurement_profile: VersionedProfileIdentity::from_content(
            "deepseek-v4-flash-tokenizer",
            TOKENIZER_PROFILE_REVISION,
            format!(
                "source=deepseek-ai/DeepSeek-V4-Flash@{DEFAULT_DEEPSEEK_V4_FLASH_REVISION};tokenizer_sha256={DEFAULT_DEEPSEEK_V4_FLASH_TOKENIZER_SHA256}"
            )
            .as_bytes(),
        ),
        hosted_parity_profile: Some(VersionedProfileIdentity::from_content(
            "deepseek-v4-flash-hosted-parity",
            HOSTED_PARITY_PROFILE_REVISION,
            format!(
                "provider=deepseek;model={DEFAULT_DEEPSEEK_V4_FLASH_MODEL};default_route={DEFAULT_API_ROUTE};beta_route={DEFAULT_BETA_API_ROUTE};stream_usage=include_usage;system_fingerprint={DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT};corpus={HOSTED_PARITY_CORPUS};validated_on={HOSTED_PARITY_VALIDATED_ON}"
            )
            .as_bytes(),
        )),
    }
}

/// Returns the complete, explicit budget for the first portable V2 target request.
///
/// This profile is intentionally provider-local: it binds the documented DeepSeek V4 Flash
/// context window and Sigil's explicit target output/safety reservations. It is not a claim about
/// an omitted provider `max_tokens` default.
#[must_use]
pub fn default_deepseek_v4_flash_portable_target_budget() -> EffectiveTokenBudget {
    EffectiveTokenBudget {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        budget_profile: VersionedProfileIdentity::from_content(
            "deepseek-v4-flash-portable-target-budget",
            PORTABLE_TARGET_BUDGET_PROFILE_REVISION,
            format!(
                "provider=deepseek;model={DEFAULT_DEEPSEEK_V4_FLASH_MODEL};context_window_tokens={DEFAULT_DEEPSEEK_V4_FLASH_CONTEXT_WINDOW_TOKENS};requested_output_tokens={DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS};safety_buffer_tokens={DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS}"
            )
            .as_bytes(),
        ),
        context_window_tokens: DEFAULT_DEEPSEEK_V4_FLASH_CONTEXT_WINDOW_TOKENS,
        requested_output_tokens: u64::from(
            DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS,
        ),
        safety_buffer_tokens: u64::from(
            DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS,
        ),
    }
}

/// Returns the explicit output cap carried by the first portable V2 target request.
#[must_use]
pub const fn default_deepseek_v4_flash_portable_target_output_tokens() -> u32 {
    DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS
}

fn validate_default_profile_request(provider_name: &str, model_name: &str) -> Result<()> {
    if provider_name != "deepseek" {
        bail!("DeepSeek V4 exact compaction profile requires the deepseek provider");
    }
    if model_name != DEFAULT_DEEPSEEK_V4_FLASH_MODEL {
        bail!(
            "DeepSeek V4 exact compaction profile requires model {DEFAULT_DEEPSEEK_V4_FLASH_MODEL}"
        );
    }
    Ok(())
}

fn render_v4_chat_prompt(request: &DeepSeekChatCompletionRequest) -> Result<String> {
    if request.messages.is_empty() {
        bail!("DeepSeek V4 canonical prompt requires at least one message");
    }
    let mut messages = request.messages.clone();
    if let Some(tools) = request.tools.clone() {
        attach_tools_to_canonical_messages(&mut messages, tools)?;
    }
    let messages = merge_tool_messages(messages)?;
    let messages = sort_tool_results_by_call_order(messages)?;
    let effective_drop_thinking = !messages.iter().any(message_has_tools);
    let messages = if effective_drop_thinking {
        drop_earlier_thinking(&messages)?
    } else {
        messages
    };
    let last_user_index = find_last_user_index(&messages)?;
    let mut prompt = String::from(BOS_TOKEN);
    for index in 0..messages.len() {
        render_message(
            &mut prompt,
            index,
            &messages,
            effective_drop_thinking,
            request.reasoning_effort.as_deref(),
            last_user_index,
        )?;
    }
    Ok(prompt)
}

fn attach_tools_to_canonical_messages(messages: &mut Vec<Value>, tools: Vec<Value>) -> Result<()> {
    let function_tools = tools
        .into_iter()
        .map(|tool| {
            let mut function = tool
                .get("function")
                .cloned()
                .context("DeepSeek tool payload is missing its function definition")?;
            // The beta endpoint validates this transport annotation but does not inject it into
            // the hosted model prompt. Keeping it here over-counts strict tool requests.
            if let Some(function) = function.as_object_mut() {
                function.remove("strict");
            }
            Ok(function)
        })
        .collect::<Result<Vec<_>>>()?;
    for message in messages.iter_mut() {
        if role_of(message)? == "system" {
            message["tools"] = Value::Array(function_tools);
            return Ok(());
        }
    }
    messages.insert(
        0,
        serde_json::json!({
            "role": "system",
            "content": "",
            "tools": function_tools,
        }),
    );
    Ok(())
}

fn merge_tool_messages(messages: Vec<Value>) -> Result<Vec<Value>> {
    let mut merged = Vec::with_capacity(messages.len());
    for message in messages {
        match role_of(&message)? {
            "tool" => {
                let tool_block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": message.get("tool_call_id").and_then(Value::as_str).unwrap_or_default(),
                    "content": message.get("content").and_then(Value::as_str).unwrap_or_default(),
                });
                let append_to_previous = merged.last().is_some_and(|previous: &Value| {
                    role_of(previous).is_ok_and(|role| role == "user")
                        && previous.get("content_blocks").is_some()
                });
                if append_to_previous {
                    let previous = merged
                        .last_mut()
                        .context("merged tool result has no previous user message")?;
                    previous
                        .get_mut("content_blocks")
                        .and_then(Value::as_array_mut)
                        .context("merged user content blocks must be an array")?
                        .push(tool_block);
                } else {
                    merged.push(serde_json::json!({
                        "role": "user",
                        "content_blocks": [tool_block],
                    }));
                }
            }
            "user" => {
                let content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let text_block = serde_json::json!({ "type": "text", "text": content });
                let append_to_previous = merged.last().is_some_and(|previous: &Value| {
                    role_of(previous).is_ok_and(|role| role == "user")
                        && previous.get("content_blocks").is_some()
                        && previous.get("task").is_none()
                });
                if append_to_previous {
                    let previous = merged
                        .last_mut()
                        .context("merged user text has no previous user message")?;
                    previous
                        .get_mut("content_blocks")
                        .and_then(Value::as_array_mut)
                        .context("merged user content blocks must be an array")?
                        .push(text_block);
                } else {
                    let mut new_message = serde_json::json!({
                        "role": "user",
                        "content": content,
                        "content_blocks": [text_block],
                    });
                    for field in ["task", "wo_eos", "mask"] {
                        if let Some(value) = message.get(field) {
                            new_message[field] = value.clone();
                        }
                    }
                    merged.push(new_message);
                }
            }
            "system" | "assistant" => merged.push(message),
            other => bail!("DeepSeek V4 canonical prompt does not support message role {other}"),
        }
    }
    Ok(merged)
}

fn sort_tool_results_by_call_order(mut messages: Vec<Value>) -> Result<Vec<Value>> {
    let mut call_order = Vec::<String>::new();
    for message in &mut messages {
        match role_of(message)? {
            "assistant" => {
                call_order.clear();
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in tool_calls {
                        if let Some(id) = call.get("id").and_then(Value::as_str)
                            && !id.is_empty()
                        {
                            call_order.push(id.to_owned());
                        }
                    }
                }
            }
            "user" if !call_order.is_empty() => {
                let Some(blocks) = message
                    .get_mut("content_blocks")
                    .and_then(Value::as_array_mut)
                else {
                    continue;
                };
                let mut tool_blocks = blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(Value::as_str) == Some("tool_result")
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if tool_blocks.len() < 2 {
                    continue;
                }
                tool_blocks.sort_by_key(|block| {
                    let id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    call_order
                        .iter()
                        .position(|known| known == id)
                        .unwrap_or(usize::MAX)
                });
                let mut sorted_index = 0;
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                        *block = tool_blocks[sorted_index].clone();
                        sorted_index += 1;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(messages)
}

fn drop_earlier_thinking(messages: &[Value]) -> Result<Vec<Value>> {
    let last_user_index = find_last_user_index(messages)?;
    let mut rendered = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        match role_of(message)? {
            "system" | "user" => rendered.push(message.clone()),
            "assistant" if index >= last_user_index => rendered.push(message.clone()),
            "assistant" => {
                let mut message = message.clone();
                if let Some(object) = message.as_object_mut() {
                    object.remove("reasoning_content");
                }
                rendered.push(message);
            }
            other => bail!("DeepSeek V4 canonical prompt does not support message role {other}"),
        }
    }
    Ok(rendered)
}

fn render_message(
    prompt: &mut String,
    index: usize,
    messages: &[Value],
    drop_thinking: bool,
    reasoning_effort: Option<&str>,
    last_user_index: usize,
) -> Result<()> {
    let message = messages
        .get(index)
        .context("DeepSeek V4 canonical prompt message index is out of range")?;
    let role = role_of(message)?;
    if index == 0 && reasoning_effort == Some("max") {
        prompt.push_str(MAX_REASONING_PREFIX);
    }
    match role {
        "system" => {
            prompt.push_str(string_field(message, "content")?.unwrap_or_default());
            if let Some(tools) = message.get("tools").and_then(Value::as_array) {
                prompt.push_str("\n\n");
                prompt.push_str(TOOLS_TEMPLATE);
                for (tool_index, tool) in tools.iter().enumerate() {
                    if tool_index > 0 {
                        prompt.push('\n');
                    }
                    prompt.push_str(&python_json(tool)?);
                }
                prompt.push_str(TOOLS_TEMPLATE_SUFFIX);
            }
        }
        "user" => {
            prompt.push_str(USER_TOKEN);
            let blocks = message
                .get("content_blocks")
                .and_then(Value::as_array)
                .context("DeepSeek V4 user message is missing canonical content blocks")?;
            for (block_index, block) in blocks.iter().enumerate() {
                if block_index > 0 {
                    prompt.push_str("\n\n");
                }
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        prompt.push_str(string_field(block, "text")?.unwrap_or_default())
                    }
                    Some("tool_result") => {
                        prompt.push_str("<tool_result>");
                        prompt.push_str(string_field(block, "content")?.unwrap_or_default());
                        prompt.push_str("</tool_result>");
                    }
                    Some(kind) => write!(prompt, "[Unsupported {kind}]")?,
                    None => bail!("DeepSeek V4 content block is missing its type"),
                }
            }
        }
        "assistant" => render_assistant_message(
            prompt,
            message,
            messages,
            index,
            drop_thinking,
            last_user_index,
        )?,
        other => bail!("DeepSeek V4 canonical prompt does not support message role {other}"),
    }

    let is_last = index + 1 == messages.len();
    let next_is_assistant_or_reminder = !is_last
        && matches!(
            role_of(&messages[index + 1])?,
            "assistant" | "latest_reminder"
        );
    if !is_last && !next_is_assistant_or_reminder {
        return Ok(());
    }
    if role == "user" {
        prompt.push_str(ASSISTANT_TOKEN);
        if drop_thinking && index < last_user_index {
            prompt.push_str(THINKING_END_TOKEN);
        } else {
            prompt.push_str(THINKING_START_TOKEN);
        }
    }
    Ok(())
}

fn render_assistant_message(
    prompt: &mut String,
    message: &Value,
    messages: &[Value],
    index: usize,
    drop_thinking: bool,
    last_user_index: usize,
) -> Result<()> {
    let previous_has_task = index
        .checked_sub(1)
        .and_then(|previous| messages.get(previous))
        .is_some_and(|previous| previous.get("task").is_some());
    if !previous_has_task && (!drop_thinking || index > last_user_index) {
        prompt.push_str(string_field(message, "reasoning_content")?.unwrap_or_default());
        prompt.push_str(THINKING_END_TOKEN);
    }
    prompt.push_str(string_field(message, "content")?.unwrap_or_default());
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array)
        && !tool_calls.is_empty()
    {
        prompt.push_str("\n\n<｜DSML｜tool_calls>\n");
        for tool_call in tool_calls {
            let function = tool_call
                .get("function")
                .context("DeepSeek assistant tool call is missing function data")?;
            let name = string_field(function, "name")?
                .context("DeepSeek assistant tool call is missing its function name")?;
            let arguments = string_field(function, "arguments")?
                .context("DeepSeek assistant tool call is missing its function arguments")?;
            writeln!(prompt, "<｜DSML｜invoke name=\"{name}\">")?;
            let arguments = serde_json::from_str::<OrderedJson>(arguments).unwrap_or_else(|_| {
                OrderedJson::Object(vec![(
                    "arguments".to_owned(),
                    OrderedJson::String(arguments.to_owned()),
                )])
            });
            let OrderedJson::Object(arguments) = arguments else {
                bail!("DeepSeek assistant tool-call arguments must be a JSON object");
            };
            for (key, value) in arguments {
                let string = matches!(value, OrderedJson::String(_));
                let value = if string {
                    let OrderedJson::String(value) = value else {
                        bail!("DeepSeek tool-call string parameter did not decode as a string");
                    };
                    value
                } else {
                    python_json_ordered(&value)?
                };
                writeln!(
                    prompt,
                    "<｜DSML｜parameter name=\"{key}\" string=\"{}\">{value}</｜DSML｜parameter>",
                    if string { "true" } else { "false" }
                )?;
            }
            prompt.push_str("</｜DSML｜invoke>\n");
        }
        prompt.push_str("</｜DSML｜tool_calls>");
    }
    prompt.push_str(EOS_TOKEN);
    Ok(())
}

fn find_last_user_index(messages: &[Value]) -> Result<usize> {
    for (index, message) in messages.iter().enumerate().rev() {
        if role_of(message)? == "user" {
            return Ok(index);
        }
    }
    bail!("DeepSeek V4 canonical prompt requires a user message")
}

fn message_has_tools(message: &Value) -> bool {
    message.get("tools").is_some_and(|tools| !tools.is_null())
}

fn role_of(message: &Value) -> Result<&str> {
    message
        .get("role")
        .and_then(Value::as_str)
        .context("DeepSeek canonical message is missing a string role")
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match value.get(field) {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => bail!("DeepSeek canonical field {field} must be a string or null"),
    }
}

fn python_json(value: &Value) -> Result<String> {
    let mut output = String::new();
    write_python_json_value(&mut output, value)?;
    Ok(output)
}

fn write_python_json_value(output: &mut String, value: &Value) -> Result<()> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&value.to_string()),
        Value::String(value) => output.push_str(&serde_json::to_string(value)?),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                write_python_json_value(output, value)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push_str(": ");
                write_python_json_value(output, value)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn python_json_ordered(value: &OrderedJson) -> Result<String> {
    let mut output = String::new();
    write_python_json_ordered(&mut output, value)?;
    Ok(output)
}

fn write_python_json_ordered(output: &mut String, value: &OrderedJson) -> Result<()> {
    match value {
        OrderedJson::Null => output.push_str("null"),
        OrderedJson::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        OrderedJson::Number(value) => output.push_str(&value.to_string()),
        OrderedJson::String(value) => output.push_str(&serde_json::to_string(value)?),
        OrderedJson::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                write_python_json_ordered(output, value)?;
            }
            output.push(']');
        }
        OrderedJson::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(", ");
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push_str(": ");
                write_python_json_ordered(output, value)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Clone)]
enum OrderedJson {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl<'de> Deserialize<'de> for OrderedJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OrderedJsonVisitor)
    }
}

struct OrderedJsonVisitor;

impl<'de> Visitor<'de> for OrderedJsonVisitor {
    type Value = OrderedJson;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(OrderedJson::Number)
            .ok_or_else(|| E::custom("JSON number must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::String(value))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::Null)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(OrderedJson::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(OrderedJson::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some((key, value)) = map.next_entry()? {
            values.push((key, value));
        }
        Ok(OrderedJson::Object(values))
    }
}

#[cfg(test)]
#[path = "tests/compaction_token_profile_tests.rs"]
mod tests;
