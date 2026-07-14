use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::{Value, json};
use sigil_kernel::{
    COMPACTION_TOKEN_PROOF_SCHEMA_VERSION, CompletionRequest, ContinuationTargetRequestFitV1,
    EffectiveTokenBudget, FrozenProviderRequestMaterial, InputTokenEvidence, ModelMessage,
    ModelRequestTimeouts, Provider, ProviderChunk, ReasoningEffort, RequestFitProof,
    TokenMeasurementScope, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolSpec,
};

use super::{
    BOS_TOKEN, DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT,
    DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS,
    DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS, DeepSeekV4FlashTokenCounter,
    MAX_REASONING_PREFIX, StrictToolsMode, default_deepseek_v4_flash_portable_target_budget,
    default_deepseek_v4_flash_portable_target_output_tokens,
    default_deepseek_v4_flash_token_binding, download_default_deepseek_v4_flash_tokenizer,
    render_v4_chat_prompt,
};

fn request(messages: Vec<ModelMessage>) -> CompletionRequest {
    CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages,
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(64),
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

fn render(request: &CompletionRequest) -> Result<String> {
    let prepared = crate::request::build_chat_request(
        request,
        None,
        StrictToolsMode::Auto,
        &crate::DeepSeekProviderQuirkProfile::default(),
    )?;
    render_v4_chat_prompt(&prepared.body)
}

fn strict_only_tool_schema_request() -> CompletionRequest {
    let mut request = request(vec![
        ModelMessage::system("You are a precise coding assistant."),
        ModelMessage::user("Use the available tool only when needed."),
    ]);
    request.tools.push(ToolSpec {
        name: "read_file".to_owned(),
        description: "Read UTF-8 text from a workspace file.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false,
        }),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    });
    request
}

fn prepared_tool_function(
    request: &CompletionRequest,
    strict_tools_mode: StrictToolsMode,
) -> Result<Value> {
    let prepared = crate::request::build_chat_request(
        request,
        None,
        strict_tools_mode,
        &crate::DeepSeekProviderQuirkProfile::default(),
    )?;
    let tool = prepared
        .body
        .tools
        .as_ref()
        .and_then(|tools| tools.first())
        .context("strict-only parity fixture did not produce a tool")?;
    tool.get("function")
        .cloned()
        .context("strict-only parity fixture tool has no function definition")
}

fn count_prepared_input(
    counter: &DeepSeekV4FlashTokenCounter,
    request: &CompletionRequest,
    strict_tools_mode: StrictToolsMode,
) -> Result<u64> {
    let prepared = crate::request::build_chat_request(
        request,
        None,
        strict_tools_mode,
        &crate::DeepSeekProviderQuirkProfile::default(),
    )?;
    counter.count_prepared_chat_input(&prepared.body)
}

fn hosted_parity_provider(strict_tools_mode: StrictToolsMode) -> Result<crate::DeepSeekProvider> {
    let mut config = crate::DeepSeekProviderConfig::default();
    // The default strict profile uses the beta endpoint. Point the non-strict control request at
    // that same endpoint so this experiment isolates the `function.strict` wire annotation.
    config.base_url = config.beta_base_url.clone();
    config.strict_tools_mode = strict_tools_mode;
    crate::DeepSeekProvider::new(config, ModelRequestTimeouts::default())
}

async fn hosted_prompt_usage(
    provider: &crate::DeepSeekProvider,
    request: CompletionRequest,
) -> Result<(u64, String)> {
    let chunks = provider.stream(request).await?.collect::<Vec<_>>().await;
    let usage = chunks
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .find_map(|chunk| match chunk {
            ProviderChunk::Usage(usage) => Some(usage),
            _ => None,
        })
        .context("DeepSeek strict-only parity fixture did not return terminal stream usage")?;
    let fingerprint = usage
        .system_fingerprint
        .context("DeepSeek strict-only parity fixture omitted its backend system fingerprint")?;
    Ok((usage.prompt_tokens, fingerprint))
}

#[test]
fn canonical_renderer_matches_the_official_basic_v4_prompt_shape() -> Result<()> {
    let mut request = request(vec![
        ModelMessage::system("You are a helpful assistant."),
        ModelMessage::user("Hello"),
    ]);
    request.reasoning_effort = Some(ReasoningEffort::Max);

    let rendered = render(&request)?;
    assert_eq!(
        rendered,
        format!(
            "{BOS_TOKEN}{MAX_REASONING_PREFIX}You are a helpful assistant.<｜User｜>Hello<｜Assistant｜><think>"
        )
    );
    Ok(())
}

#[test]
fn canonical_renderer_keeps_tool_argument_order_and_merges_tool_results() -> Result<()> {
    let assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "lookup".to_owned(),
            args_json: r#"{"z":true,"a":"keep-this-order"}"#.to_owned(),
        }],
    );
    let mut request = request(vec![
        ModelMessage::user("Find the answer."),
        assistant,
        ModelMessage::tool("call-1", r#"{"answer":42}"#),
        ModelMessage::user("Continue."),
    ]);
    request.tools.push(ToolSpec {
        name: "lookup".to_owned(),
        description: "Lookup a value.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {"z": {"type": "boolean"}, "a": {"type": "string"}},
            "required": ["z", "a"],
        }),
        category: ToolCategory::Search,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    });

    let rendered = render(&request)?;
    assert!(rendered.starts_with("<｜begin▁of▁sentence｜>\n\n## Tools\n"));
    assert!(rendered.contains("\"description\": \"Lookup a value.\""));
    assert!(!rendered.contains("\"strict\": true"));
    assert!(rendered.contains("\"additionalProperties\": false"));
    let z = rendered
        .find("name=\"z\" string=\"false\">true")
        .expect("z argument should be rendered");
    let a = rendered
        .find("name=\"a\" string=\"true\">keep-this-order")
        .expect("a argument should be rendered");
    assert!(z < a);
    assert!(rendered.contains("<tool_result>{\"answer\":42}</tool_result>\n\nContinue."));
    assert!(rendered.ends_with("<｜Assistant｜><think>"));
    Ok(())
}

#[test]
fn strict_only_parity_fixture_changes_only_the_transport_annotation() -> Result<()> {
    let request = strict_only_tool_schema_request();
    let mut strict_function = prepared_tool_function(&request, StrictToolsMode::Auto)?;
    let strict_object = strict_function
        .as_object_mut()
        .context("strict-only parity fixture function must be an object")?;
    assert_eq!(strict_object.remove("strict"), Some(Value::Bool(true)));

    assert_eq!(
        strict_function,
        prepared_tool_function(&request, StrictToolsMode::Off)?,
        "strict-only parity fixture must not vary schema or function content"
    );
    Ok(())
}

#[test]
fn admitted_profile_rejects_unverified_tokenizer_bytes_and_binds_hosted_parity() -> Result<()> {
    let error = match DeepSeekV4FlashTokenCounter::from_official_tokenizer_bytes(b"not a tokenizer")
    {
        Ok(_) => anyhow::bail!("unverified tokenizer bytes must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("checksum"));

    let binding = default_deepseek_v4_flash_token_binding();
    binding.validate()?;
    assert_eq!(
        binding.schema_version,
        COMPACTION_TOKEN_PROOF_SCHEMA_VERSION
    );
    assert_eq!(
        binding
            .hosted_parity_profile
            .as_ref()
            .map(|profile| profile.profile_id.as_str()),
        Some("deepseek-v4-flash-hosted-parity")
    );

    let proof = RequestFitProof {
        schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: InputTokenEvidence::Exact {
            tokens: 1,
            material_fingerprint: "hmac-sha256:unadmitted".to_owned(),
            measurement_scope: TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: None,
            provider_system_fingerprint: None,
        },
        budget: EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: sigil_kernel::VersionedProfileIdentity::from_content(
                "deepseek-v4-flash-budget-test",
                1,
                b"admitted-profile",
            ),
            context_window_tokens: 1_000_000,
            requested_output_tokens: 64,
            safety_buffer_tokens: 128,
        },
    };
    proof.validate_for(
        "hmac-sha256:unadmitted",
        TokenMeasurementScope::RenderedTargetInput,
        &binding,
    )?;
    Ok(())
}

#[test]
fn portable_target_budget_is_explicit_and_matches_its_frozen_request_cap() -> Result<()> {
    let budget = default_deepseek_v4_flash_portable_target_budget();
    budget.validate()?;

    assert_eq!(budget.context_window_tokens, 1_000_000);
    assert_eq!(
        budget.requested_output_tokens,
        u64::from(DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS)
    );
    assert_eq!(
        budget.safety_buffer_tokens,
        u64::from(DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_SAFETY_BUFFER_TOKENS)
    );
    assert_eq!(
        default_deepseek_v4_flash_portable_target_output_tokens(),
        DEFAULT_DEEPSEEK_V4_FLASH_PORTABLE_TARGET_OUTPUT_TOKENS
    );
    assert!(
        budget.context_window_tokens > budget.requested_output_tokens + budget.safety_buffer_tokens
    );
    Ok(())
}

#[tokio::test]
#[ignore = "downloads the pinned public DeepSeek tokenizer artifact"]
async fn pinned_official_tokenizer_produces_exact_frozen_target_proof() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let client = reqwest::Client::builder().build()?;
    let tokenizer_path = download_default_deepseek_v4_flash_tokenizer(&client, temp.path()).await?;
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        "token-profile-test-session",
        request(vec![ModelMessage::user("用 Rust 解释所有权🙂")]),
    )?;
    let proof = counter.exact_target_request_fit(
        &frozen,
        EffectiveTokenBudget {
            schema_version: COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: sigil_kernel::VersionedProfileIdentity::from_content(
                "deepseek-v4-flash-test-budget",
                1,
                b"context=1000000;output=64;safety=128",
            ),
            context_window_tokens: 1_000_000,
            requested_output_tokens: 64,
            safety_buffer_tokens: 128,
        },
    )?;
    let target_fit = ContinuationTargetRequestFitV1 {
        material_fingerprint: frozen.fingerprint().to_owned(),
        binding: default_deepseek_v4_flash_token_binding(),
        proof,
    };
    target_fit.validate_for_frozen_request("token-profile-test-session", &frozen)?;
    match target_fit.proof.input {
        InputTokenEvidence::Exact {
            tokens,
            provider_system_fingerprint,
            ..
        } => {
            assert!(tokens > 0);
            assert_eq!(
                provider_system_fingerprint.as_deref(),
                Some(DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT)
            );
        }
        InputTokenEvidence::ConservativeUpperBound { .. } => {
            anyhow::bail!("admitted DeepSeek profile must produce exact evidence");
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "uses the configured DeepSeek key with synthetic public parity fixtures"]
async fn hosted_v4_prompt_usage_matches_the_verified_local_canonical_tokenizer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let client = reqwest::Client::builder().build()?;
    let tokenizer_path = download_default_deepseek_v4_flash_tokenizer(&client, temp.path()).await?;
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)?;
    let provider = crate::DeepSeekProvider::new(
        crate::DeepSeekProviderConfig::default(),
        ModelRequestTimeouts::default(),
    )?;

    for (fixture_id, mut request) in hosted_parity_requests() {
        request.max_tokens = Some(1);
        let frozen =
            FrozenProviderRequestMaterial::freeze("hosted-parity-session", request.clone())?;
        let local_tokens = counter.count_frozen_target_input(&frozen)?;
        let stream = provider.stream(request).await?;
        let chunks = stream.collect::<Vec<_>>().await;
        let usage = chunks
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .find_map(|chunk| match chunk {
                ProviderChunk::Usage(usage) => Some(usage),
                _ => None,
            })
            .context("DeepSeek parity fixture did not return terminal stream usage")?;
        println!(
            "DeepSeek V4 parity fixture {fixture_id}: prompt_tokens={} system_fingerprint={}",
            usage.prompt_tokens,
            usage.system_fingerprint.as_deref().unwrap_or("<missing>")
        );
        assert_eq!(
            local_tokens, usage.prompt_tokens,
            "DeepSeek V4 canonical token mismatch for parity fixture {fixture_id}"
        );
        assert!(
            usage.system_fingerprint.as_deref()
                == Some(DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT),
            "DeepSeek parity fixture {fixture_id} backend fingerprint drifted"
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "uses the configured DeepSeek key to isolate the strict-tool transport annotation"]
async fn hosted_v4_strict_tool_annotation_matches_the_standard_tool_prompt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let client = reqwest::Client::builder().build()?;
    let tokenizer_path = download_default_deepseek_v4_flash_tokenizer(&client, temp.path()).await?;
    let counter = DeepSeekV4FlashTokenCounter::from_official_tokenizer_path(&tokenizer_path)?;
    let mut request = strict_only_tool_schema_request();
    request.max_tokens = Some(1);

    let strict_local = count_prepared_input(&counter, &request, StrictToolsMode::Auto)?;
    let standard_local = count_prepared_input(&counter, &request, StrictToolsMode::Off)?;
    assert_eq!(
        strict_local, standard_local,
        "canonical prompt counting must exclude the strict transport annotation"
    );

    let (strict_hosted, strict_fingerprint) = hosted_prompt_usage(
        &hosted_parity_provider(StrictToolsMode::Auto)?,
        request.clone(),
    )
    .await?;
    let (standard_hosted, standard_fingerprint) =
        hosted_prompt_usage(&hosted_parity_provider(StrictToolsMode::Off)?, request).await?;
    println!(
        "DeepSeek V4 strict-only parity: strict_local={strict_local} strict_hosted={strict_hosted} standard_local={standard_local} standard_hosted={standard_hosted} system_fingerprint={strict_fingerprint}"
    );
    assert_eq!(
        strict_fingerprint, standard_fingerprint,
        "strict-only parity control requests must use the same hosted backend"
    );
    assert_eq!(
        strict_fingerprint, DEFAULT_DEEPSEEK_V4_FLASH_HOSTED_SYSTEM_FINGERPRINT,
        "strict-only parity backend fingerprint drifted"
    );
    assert_eq!(
        strict_hosted, standard_hosted,
        "hosted strict annotation must not change the model prompt token count"
    );
    assert_eq!(
        strict_hosted, standard_local,
        "hosted strict request must match the canonical prompt without the transport annotation"
    );
    Ok(())
}

fn hosted_parity_requests() -> Vec<(&'static str, CompletionRequest)> {
    let mut tool_schema = request(vec![
        ModelMessage::system("You are a precise coding assistant."),
        ModelMessage::user("Use the available tool only when needed."),
    ]);
    tool_schema.tools.push(ToolSpec {
        name: "read_file".to_owned(),
        description: "Read UTF-8 text from a workspace file.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
        }),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    });

    let assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-public-fixture".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
        }],
    );
    let mut tool_result = request(vec![
        ModelMessage::user("Read the file."),
        assistant,
        ModelMessage::tool("call-public-fixture", "pub fn example() {}"),
        ModelMessage::user("State the function name only."),
    ]);
    tool_result.tools = tool_schema.tools.clone();

    let mut max_reasoning = request(vec![ModelMessage::user("Return exactly: OK")]);
    max_reasoning.reasoning_effort = Some(ReasoningEffort::Max);

    vec![
        (
            "latin-baseline",
            request(vec![
                ModelMessage::system("You are concise."),
                ModelMessage::user("Reply with OK."),
            ]),
        ),
        (
            "cjk-emoji",
            request(vec![ModelMessage::user("用 Rust 解释所有权🙂")]),
        ),
        ("tool-schema", tool_schema),
        ("assistant-tool-result", tool_result),
        ("max-reasoning", max_reasoning),
    ]
}
