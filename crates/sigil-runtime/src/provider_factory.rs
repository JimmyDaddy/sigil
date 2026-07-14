use super::*;

/// Builds the configured model provider for runtime entrypoints.
///
/// # Errors
///
/// Returns an error when the configured provider is unsupported or its provider-specific
/// configuration cannot be parsed or initialized.
pub fn build_provider(root_config: &RootConfig) -> Result<Box<dyn Provider>> {
    let timeouts = root_config.model_request.to_timeouts()?;
    match provider_config_key(&root_config.agent.provider) {
        "deepseek" => Ok(Box::new(DeepSeekProvider::new(
            resolve_deepseek_config(root_config)?,
            timeouts,
        )?)),
        "openai_compat" => Ok(Box::new(OpenAiCompatibleProvider::new(
            resolve_openai_compat_config(root_config)?,
            timeouts,
        )?)),
        "openai_responses" => Ok(Box::new(OpenAiResponsesProvider::new(
            resolve_openai_responses_config(root_config)?,
            timeouts,
        )?)),
        "anthropic" => Ok(Box::new(AnthropicProvider::new(
            resolve_anthropic_config(root_config)?,
            timeouts,
        )?)),
        "gemini" => Ok(Box::new(GeminiProvider::new(
            resolve_gemini_config(root_config)?,
            timeouts,
        )?)),
        other => Err(anyhow!("unsupported provider {other}")),
    }
}

/// Product-facing support state for one provider-neutral capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCapabilityStatus {
    Supported,
    Advanced,
    Unsupported,
}

impl ProviderCapabilityStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Advanced => "advanced",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One provider capability row suitable for diagnostics and TUI config surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilityRow {
    pub key: &'static str,
    pub label: &'static str,
    pub status: ProviderCapabilityStatus,
    pub detail: String,
}

/// Provider-neutral capability view derived from `ProviderCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilityView {
    pub provider_name: String,
    pub rows: Vec<ProviderCapabilityRow>,
}

/// Returns static provider capabilities for a configured provider name.
#[must_use]
pub fn provider_capabilities_for_name(provider_name: &str) -> Option<ProviderCapabilities> {
    match provider_config_key(provider_name) {
        "deepseek" => Some(deepseek_capabilities()),
        "openai_compat" => Some(openai_compatible_capabilities()),
        "openai_responses" => Some(openai_responses_capabilities()),
        "anthropic" => Some(anthropic_capabilities()),
        "gemini" => Some(gemini_capabilities()),
        _ => None,
    }
}

/// Builds a provider-neutral capability view for diagnostics and UI display.
#[must_use]
pub fn provider_capability_view(
    provider_name: &str,
    capabilities: &ProviderCapabilities,
) -> ProviderCapabilityView {
    let mut rows = Vec::new();
    rows.push(capability_row(
        "text_stream",
        "Streaming text",
        ProviderCapabilityStatus::Supported,
        "provider stream emits text deltas",
    ));
    rows.push(capability_row(
        "tool_calls",
        "Tool calls",
        if capabilities.supports_schema_constrained_tools || capabilities.supports_tool_stream {
            ProviderCapabilityStatus::Supported
        } else {
            ProviderCapabilityStatus::Unsupported
        },
        if capabilities.supports_schema_constrained_tools {
            "schema-constrained tools enabled"
        } else {
            "basic tool calls only"
        },
    ));
    rows.push(capability_row(
        "tool_args_stream",
        "Tool arg stream",
        status_for_bool(capabilities.supports_tool_stream),
        "incremental tool arguments",
    ));
    rows.push(capability_row(
        "reasoning_stream",
        "Reasoning stream",
        if capabilities.can_surface_reasoning_stream() {
            ProviderCapabilityStatus::Supported
        } else {
            ProviderCapabilityStatus::Unsupported
        },
        capabilities.reasoning_stream.as_str(),
    ));
    rows.push(capability_row(
        "reasoning_effort",
        "Reasoning effort",
        status_for_bool(capabilities.supports_reasoning_effort),
        "generic low/medium/high/max control",
    ));
    rows.push(capability_row(
        "reasoning_artifacts",
        "Reasoning artifacts",
        status_for_bool(capabilities.supports_reasoning_artifacts),
        "durable reasoning artifact handles",
    ));
    rows.push(capability_row(
        "structured_output",
        "Structured output",
        status_for_bool(capabilities.supports_structured_output),
        "provider-native structured response mode",
    ));
    rows.push(capability_row(
        "assistant_prefix_seed",
        "Assistant prefix seed",
        status_for_bool(capabilities.supports_assistant_prefix_seed),
        "assistant-prefix seed accepted",
    ));
    rows.push(capability_row(
        "background_tasks",
        "Background tasks",
        status_for_bool(capabilities.supports_background_tasks),
        "provider-managed async work",
    ));
    rows.push(capability_row(
        "agent_background_resume",
        "Agent background resume",
        status_for_bool(capabilities.supports_agent_background_resume),
        "provider-backed child thread resume",
    ));
    rows.push(capability_row(
        "agent_thread_usage",
        "Agent thread usage",
        status_for_bool(capabilities.supports_agent_thread_usage),
        "per-agent usage replay",
    ));
    rows.push(capability_row(
        "agent_result_replay",
        "Agent result replay",
        status_for_bool(capabilities.supports_agent_result_replay),
        "provider-backed child result replay",
    ));
    rows.push(capability_row(
        "response_handles",
        "Response handles",
        status_for_bool(capabilities.supports_response_handles),
        "provider resumable response handle",
    ));
    rows.push(capability_row(
        "cache_reporting",
        "Cache telemetry",
        if capabilities.exact_prefix_cache && capabilities.reports_cache_tokens {
            ProviderCapabilityStatus::Supported
        } else if capabilities.reports_cache_tokens {
            ProviderCapabilityStatus::Advanced
        } else {
            ProviderCapabilityStatus::Unsupported
        },
        if capabilities.exact_prefix_cache {
            "exact prefix cache tokens"
        } else if capabilities.reports_cache_tokens {
            "provider cache token reporting"
        } else {
            "not reported"
        },
    ));
    rows.push(capability_row(
        "system_fingerprint",
        "System fingerprint",
        status_for_bool(capabilities.supports_system_fingerprint),
        "system fingerprint telemetry",
    ));
    rows.push(capability_row(
        "infill",
        "Infill completion",
        status_for_bool(capabilities.supports_infill_completion),
        "provider-native infill completion",
    ));
    rows.push(capability_row(
        "tool_name_limit",
        "Tool name budget",
        status_for_bool(capabilities.tool_name_max_chars > 0),
        format!(
            "provider-visible tool names up to {} chars",
            capabilities.tool_name_max_chars
        ),
    ));

    ProviderCapabilityView {
        provider_name: provider_config_key(provider_name).to_owned(),
        rows,
    }
}

fn status_for_bool(supported: bool) -> ProviderCapabilityStatus {
    if supported {
        ProviderCapabilityStatus::Supported
    } else {
        ProviderCapabilityStatus::Unsupported
    }
}

fn capability_row(
    key: &'static str,
    label: &'static str,
    status: ProviderCapabilityStatus,
    detail: impl Into<String>,
) -> ProviderCapabilityRow {
    ProviderCapabilityRow {
        key,
        label,
        status,
        detail: detail.into(),
    }
}

/// Builds the configured model provider for one task role.
///
/// # Errors
///
/// Returns an error when the resolved role provider is unsupported or malformed.
pub fn build_role_provider(root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
    let role_config = root_config.task.role_config(role);
    let resolved = root_config_with_role_agent(root_config, role_config);
    build_provider(&resolved)
}

/// Parses the DeepSeek provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.deepseek]` is missing or malformed.
pub fn load_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("deepseek")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.deepseek] in sigil.toml"))?;
    let mut config: DeepSeekProviderConfig = serde_json::from_value(provider_config_value)
        .context("invalid deepseek provider config")?;
    config.model = root_config.agent.model.clone();
    Ok(config)
}

/// Parses the OpenAI-compatible provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.openai_compat]` is missing or malformed.
pub fn load_openai_compat_config(
    root_config: &RootConfig,
) -> Result<OpenAiCompatibleProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("openai_compat")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.openai_compat] in sigil.toml"))?;
    let mut config: OpenAiCompatibleProviderConfig = serde_json::from_value(provider_config_value)
        .context("invalid openai_compat provider config")?;
    config.model = root_config.agent.model.clone();
    Ok(config)
}

/// Parses the OpenAI Responses provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.openai_responses]` is missing or malformed.
pub fn load_openai_responses_config(
    root_config: &RootConfig,
) -> Result<OpenAiResponsesProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("openai_responses")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.openai_responses] in sigil.toml"))?;
    let mut config: OpenAiResponsesProviderConfig =
        serde_json::from_value(provider_config_value)
            .context("invalid openai_responses provider config")?;
    config.model = root_config.agent.model.clone();
    Ok(config)
}

/// Parses the Anthropic provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.anthropic]` is missing or malformed.
pub fn load_anthropic_config(root_config: &RootConfig) -> Result<AnthropicProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("anthropic")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.anthropic] in sigil.toml"))?;
    let mut config: AnthropicProviderConfig = serde_json::from_value(provider_config_value)
        .context("invalid anthropic provider config")?;
    config.model = root_config.agent.model.clone();
    Ok(config)
}

/// Parses the Gemini provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.gemini]` is missing or malformed.
pub fn load_gemini_config(root_config: &RootConfig) -> Result<GeminiProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("gemini")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.gemini] in sigil.toml"))?;
    let mut config: GeminiProviderConfig =
        serde_json::from_value(provider_config_value).context("invalid gemini provider config")?;
    config.model = root_config.agent.model.clone();
    Ok(config)
}

/// Source used for a resolved runtime secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    Environment(&'static str),
    ConfigPlaintext,
    Session,
}

/// A resolved secret value and the storage layer it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretResolution {
    pub value: String,
    pub source: SecretSource,
}

/// Resolves DeepSeek configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    load_deepseek_config(root_config)?.resolved()
}

/// Resolves OpenAI-compatible configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_openai_compat_config(
    root_config: &RootConfig,
) -> Result<OpenAiCompatibleProviderConfig> {
    load_openai_compat_config(root_config)?.resolved()
}

/// Resolves OpenAI Responses configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_openai_responses_config(
    root_config: &RootConfig,
) -> Result<OpenAiResponsesProviderConfig> {
    load_openai_responses_config(root_config)?.resolved()
}

/// Resolves Anthropic configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_anthropic_config(root_config: &RootConfig) -> Result<AnthropicProviderConfig> {
    load_anthropic_config(root_config)?.resolved()
}

/// Resolves Gemini configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_gemini_config(root_config: &RootConfig) -> Result<GeminiProviderConfig> {
    load_gemini_config(root_config)?.resolved()
}

#[must_use]
pub fn resolve_deepseek_api_key(config: &DeepSeekProviderConfig) -> Option<SecretResolution> {
    resolve_deepseek_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_deepseek_api_key_with_session(
    config: &DeepSeekProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    if let Some(value) = read_secret_env(SIGIL_API_KEY_ENV) {
        return Some(SecretResolution {
            value,
            source: SecretSource::Environment(SIGIL_API_KEY_ENV),
        });
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_openai_compat_api_key(
    config: &OpenAiCompatibleProviderConfig,
) -> Option<SecretResolution> {
    resolve_openai_compat_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_openai_compat_api_key_with_session(
    config: &OpenAiCompatibleProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    if let Some(value) = read_secret_env(OPENAI_COMPATIBLE_API_KEY_ENV) {
        return Some(SecretResolution {
            value,
            source: SecretSource::Environment(OPENAI_COMPATIBLE_API_KEY_ENV),
        });
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_openai_responses_api_key(
    config: &OpenAiResponsesProviderConfig,
) -> Option<SecretResolution> {
    resolve_openai_responses_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_openai_responses_api_key_with_session(
    config: &OpenAiResponsesProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    if let Some(value) = read_secret_env(OPENAI_RESPONSES_API_KEY_ENV) {
        return Some(SecretResolution {
            value,
            source: SecretSource::Environment(OPENAI_RESPONSES_API_KEY_ENV),
        });
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_anthropic_api_key(config: &AnthropicProviderConfig) -> Option<SecretResolution> {
    resolve_anthropic_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_anthropic_api_key_with_session(
    config: &AnthropicProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    if let Some(value) = read_secret_env(SIGIL_ANTHROPIC_API_KEY_ENV) {
        return Some(SecretResolution {
            value,
            source: SecretSource::Environment(SIGIL_ANTHROPIC_API_KEY_ENV),
        });
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_gemini_api_key(config: &GeminiProviderConfig) -> Option<SecretResolution> {
    resolve_gemini_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_gemini_api_key_with_session(
    config: &GeminiProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    if let Some(value) = read_secret_env(SIGIL_GEMINI_API_KEY_ENV) {
        return Some(SecretResolution {
            value,
            source: SecretSource::Environment(SIGIL_GEMINI_API_KEY_ENV),
        });
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn secret_redactor_for_root_config(root_config: &RootConfig) -> SecretRedactor {
    let mut redactor = SecretRedactor::empty();
    if let Ok(config) = load_deepseek_config(root_config)
        && let Some(api_key) = resolve_deepseek_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_openai_compat_config(root_config)
        && let Some(api_key) = resolve_openai_compat_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_openai_responses_config(root_config)
        && let Some(api_key) = resolve_openai_responses_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_anthropic_config(root_config)
        && let Some(api_key) = resolve_anthropic_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_gemini_config(root_config)
        && let Some(api_key) = resolve_gemini_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    redactor
}

fn read_secret_env(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn root_config_with_role_agent(
    root_config: &RootConfig,
    role_config: &RoleModelConfig,
) -> RootConfig {
    let mut resolved = root_config.clone();
    if let Some(provider) = role_config.provider.as_deref() {
        resolved.agent.provider = provider.to_owned();
    }
    if let Some(model) = role_config.model.as_deref() {
        resolved.agent.model = model.to_owned();
    }
    resolved
}

#[must_use]
pub fn provider_config_key(provider: &str) -> &str {
    provider.trim()
}
