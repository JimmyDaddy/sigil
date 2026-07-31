use super::*;

/// Builds the configured model provider for runtime entrypoints.
///
/// # Errors
///
/// Returns an error when the configured provider is unsupported or its provider-specific
/// configuration cannot be parsed or initialized.
pub fn build_provider(root_config: &RootConfig) -> Result<Box<dyn Provider>> {
    let root_config = root_config.clone();
    block_on_provider_future("build_provider_async", async move {
        build_provider_async(&root_config).await
    })
}

/// Resolves an exact connection credential asynchronously before constructing its provider.
pub async fn build_provider_async(root_config: &RootConfig) -> Result<Box<dyn Provider>> {
    let credential_store =
        crate::provider_connections::ConfiguredProviderCredentialStore::from_root_config(
            root_config,
        );
    let environment = crate::provider_connections::ProcessCredentialEnvironment;
    build_provider_with_credentials(root_config, &credential_store, &environment).await
}

/// Injected variant of [`build_provider_async`] for tests and non-native credential owners.
pub async fn build_provider_with_credentials(
    root_config: &RootConfig,
    credential_store: &dyn crate::provider_connections::ProviderCredentialStore,
    environment: &dyn crate::provider_connections::CredentialEnvironment,
) -> Result<Box<dyn Provider>> {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    if loaded.mode != crate::provider_connections::ConfigMode::V2 || !loaded.issues.is_empty() {
        anyhow::bail!(
            "connection_config_invalid: {}",
            loaded
                .issues
                .iter()
                .map(|issue| issue.code)
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    let model_ref = loaded
        .default_model
        .as_ref()
        .ok_or_else(|| anyhow!("model_route_not_configured"))?;
    build_provider_for_model_ref_with_credentials(
        root_config,
        model_ref,
        credential_store,
        environment,
    )
    .await
}

/// Builds the provider for one exact compound model identity.
///
/// This is the session-resume and fresh-session seam: a changed saved default cannot silently
/// replace the connection/model frozen in the durable session route.
pub async fn build_provider_for_model_ref_async(
    root_config: &RootConfig,
    model_ref: &sigil_kernel::ModelRef,
) -> Result<Box<dyn Provider>> {
    let credential_store =
        crate::provider_connections::ConfiguredProviderCredentialStore::from_root_config(
            root_config,
        );
    let environment = crate::provider_connections::ProcessCredentialEnvironment;
    build_provider_for_model_ref_with_credentials(
        root_config,
        model_ref,
        &credential_store,
        &environment,
    )
    .await
}

/// Synchronous compatibility wrapper for exact-route owners already running on a blocking thread.
pub fn build_provider_for_model_ref(
    root_config: &RootConfig,
    model_ref: &sigil_kernel::ModelRef,
) -> Result<Box<dyn Provider>> {
    let root_config = root_config.clone();
    let model_ref = model_ref.clone();
    block_on_provider_future("build_provider_for_model_ref_async", async move {
        build_provider_for_model_ref_async(&root_config, &model_ref).await
    })
}

/// Injected exact-route provider construction for tests and native owners.
pub async fn build_provider_for_model_ref_with_credentials(
    root_config: &RootConfig,
    model_ref: &sigil_kernel::ModelRef,
    credential_store: &dyn crate::provider_connections::ProviderCredentialStore,
    environment: &dyn crate::provider_connections::CredentialEnvironment,
) -> Result<Box<dyn Provider>> {
    use crate::provider_connections::{
        ConfigMode, ProviderFamily, ProviderProtocol, load_provider_connections,
        resolve_connection_credential,
    };

    let loaded = load_provider_connections(root_config);
    match loaded.mode {
        ConfigMode::V2 => {}
        ConfigMode::Invalid => {
            anyhow::bail!(
                "connection_config_invalid: {}",
                loaded
                    .issues
                    .iter()
                    .map(|issue| issue.code)
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
    }
    let blocking_issues = loaded
        .issues
        .iter()
        .filter(|issue| {
            issue.connection_id.is_none()
                || issue.connection_id.as_deref() == Some(model_ref.connection_id.as_str())
        })
        .map(|issue| issue.code)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        blocking_issues.is_empty(),
        "invalid default provider connection: {}",
        blocking_issues.join(",")
    );
    let connection = loaded
        .connections
        .get(&model_ref.connection_id)
        .ok_or_else(|| anyhow!("connection_not_found"))?;
    let credential = resolve_connection_credential(
        &connection.config,
        &connection.credential,
        credential_store,
        environment,
    )
    .await
    .map_err(anyhow::Error::new)?;
    let timeouts = root_config.model_request.to_timeouts()?;
    let api_key = credential
        .secret
        .as_ref()
        .map(|secret| secret.expose_secret().to_owned());

    match (connection.config.provider, connection.config.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => {
            let mut config: DeepSeekProviderConfig =
                exact_connection_provider_config(&connection.config, api_key)?;
            config.model = model_ref.model_id.clone();
            Ok(Box::new(DeepSeekProvider::new_exact(config, timeouts)?))
        }
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => {
            let mut config: OpenAiResponsesProviderConfig =
                exact_connection_provider_config(&connection.config, api_key)?;
            config.model = model_ref.model_id.clone();
            Ok(Box::new(OpenAiResponsesProvider::new_exact(
                config, timeouts,
            )?))
        }
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => {
            let mut config: OpenAiCompatibleProviderConfig =
                exact_connection_provider_config(&connection.config, api_key)?;
            config.model = model_ref.model_id.clone();
            Ok(Box::new(OpenAiCompatibleProvider::new_exact(
                config, timeouts,
            )?))
        }
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => {
            let mut config: AnthropicProviderConfig =
                exact_connection_provider_config(&connection.config, api_key)?;
            config.model = model_ref.model_id.clone();
            Ok(Box::new(AnthropicProvider::new_exact(config, timeouts)?))
        }
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => {
            let mut config: GeminiProviderConfig =
                exact_connection_provider_config(&connection.config, api_key)?;
            config.model = model_ref.model_id.clone();
            Ok(Box::new(GeminiProvider::new_exact(config, timeouts)?))
        }
        _ => Err(anyhow!("unsupported provider connection protocol")),
    }
}

pub(crate) fn exact_connection_provider_config<T>(
    connection: &crate::provider_connections::ProviderConnectionConfig,
    api_key: Option<String>,
) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut object = connection
        .options
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("provider connection options must be an object"))?;
    for reserved in [
        "id",
        "label",
        "provider",
        "protocol",
        "base_url",
        "model",
        "credential",
        "api_key",
        "__runtime_model",
    ] {
        object.remove(reserved);
    }
    object.insert(
        "base_url".to_owned(),
        serde_json::Value::String(connection.base_url.clone()),
    );
    if let Some(api_key) = api_key {
        object.insert("api_key".to_owned(), serde_json::Value::String(api_key));
    }
    serde_json::from_value(serde_json::Value::Object(object))
        .context("invalid provider-specific connection options")
}

pub(crate) fn validate_connection_provider_options(
    connection: &crate::provider_connections::ProviderConnectionConfig,
) -> Result<()> {
    use crate::provider_connections::{ProviderFamily, ProviderProtocol};

    match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => {
            exact_connection_provider_config::<DeepSeekProviderConfig>(connection, None)?;
        }
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => {
            exact_connection_provider_config::<OpenAiResponsesProviderConfig>(connection, None)?;
        }
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiChatCompletions)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => {
            exact_connection_provider_config::<OpenAiCompatibleProviderConfig>(connection, None)?;
        }
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => {
            exact_connection_provider_config::<AnthropicProviderConfig>(connection, None)?;
        }
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => {
            exact_connection_provider_config::<GeminiProviderConfig>(connection, None)?;
        }
        _ => return Err(anyhow!("unsupported provider connection protocol")),
    }
    Ok(())
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
    let root_config = root_config.clone();
    block_on_provider_future("build_role_provider_async", async move {
        build_role_provider_async(&root_config, role).await
    })
}

fn block_on_provider_future<F>(async_builder: &'static str, future: F) -> Result<Box<dyn Provider>>
where
    F: std::future::Future<Output = Result<Box<dyn Provider>>> + Send + 'static,
{
    anyhow::ensure!(
        tokio::runtime::Handle::try_current().is_err(),
        "synchronous provider resolution cannot run inside an async runtime; use {async_builder}"
    );
    std::thread::Builder::new()
        .name("sigil-provider-resolution".to_owned())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build credential resolution runtime")?
                .block_on(future)
        })
        .context("failed to start credential resolution thread")?
        .join()
        .map_err(|_| anyhow!("credential resolution thread panicked"))?
}

/// Async credential-aware role-provider construction.
pub async fn build_role_provider_async(
    root_config: &RootConfig,
    role: AgentRole,
) -> Result<Box<dyn Provider>> {
    let credential_store =
        crate::provider_connections::ConfiguredProviderCredentialStore::from_root_config(
            root_config,
        );
    let environment = crate::provider_connections::ProcessCredentialEnvironment;
    build_role_provider_with_credentials(root_config, role, &credential_store, &environment).await
}

/// Injected async role-provider construction.
pub async fn build_role_provider_with_credentials(
    root_config: &RootConfig,
    role: AgentRole,
    credential_store: &dyn crate::provider_connections::ProviderCredentialStore,
    environment: &dyn crate::provider_connections::CredentialEnvironment,
) -> Result<Box<dyn Provider>> {
    let role_config = root_config.task.role_config(role);
    let mut resolved = root_config.clone();
    if let Some(connection) = role_config.connection.as_ref() {
        resolved.agent.connection = Some(connection.clone());
    }
    if let Some(model) = role_config.model.as_deref() {
        resolved.agent.model = model.to_owned();
    }
    build_provider_with_credentials(&resolved, credential_store, environment).await
}

/// Resolves the active DeepSeek connection options from the shared root config.
pub fn load_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    use crate::provider_connections::{ProviderFamily, ProviderProtocol};
    let (connection, model) = active_connection(root_config)?;
    anyhow::ensure!(
        matches!(
            (connection.provider, connection.protocol),
            (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek)
        ),
        "active connection is not DeepSeek"
    );
    let mut config: DeepSeekProviderConfig = exact_connection_provider_config(&connection, None)?;
    config.model = model;
    Ok(config)
}

/// Resolves the active OpenAI-compatible connection options.
pub fn load_openai_compat_config(
    root_config: &RootConfig,
) -> Result<OpenAiCompatibleProviderConfig> {
    use crate::provider_connections::{ProviderFamily, ProviderProtocol};
    let (connection, model) = active_connection(root_config)?;
    anyhow::ensure!(
        matches!(
            (connection.provider, connection.protocol),
            (
                ProviderFamily::Custom | ProviderFamily::OpenAi,
                ProviderProtocol::OpenAiChatCompletions
            )
        ),
        "active connection is not OpenAI-compatible"
    );
    let mut config: OpenAiCompatibleProviderConfig =
        exact_connection_provider_config(&connection, None)?;
    config.model = model;
    Ok(config)
}

/// Resolves the active OpenAI Responses connection options.
pub fn load_openai_responses_config(
    root_config: &RootConfig,
) -> Result<OpenAiResponsesProviderConfig> {
    use crate::provider_connections::{ProviderFamily, ProviderProtocol};
    let (connection, model) = active_connection(root_config)?;
    anyhow::ensure!(
        matches!(
            (connection.provider, connection.protocol),
            (
                ProviderFamily::OpenAi | ProviderFamily::Custom,
                ProviderProtocol::OpenAiResponses
            )
        ),
        "active connection is not OpenAI Responses"
    );
    let mut config: OpenAiResponsesProviderConfig =
        exact_connection_provider_config(&connection, None)?;
    config.model = model;
    Ok(config)
}

/// Resolves the active Anthropic connection options.
pub fn load_anthropic_config(root_config: &RootConfig) -> Result<AnthropicProviderConfig> {
    use crate::provider_connections::{ProviderFamily, ProviderProtocol};
    let (connection, model) = active_connection(root_config)?;
    anyhow::ensure!(
        matches!(
            (connection.provider, connection.protocol),
            (
                ProviderFamily::Anthropic,
                ProviderProtocol::AnthropicMessages
            )
        ),
        "active connection is not Anthropic"
    );
    let mut config: AnthropicProviderConfig = exact_connection_provider_config(&connection, None)?;
    config.model = model;
    Ok(config)
}

/// Resolves the active Gemini connection options.
pub fn load_gemini_config(root_config: &RootConfig) -> Result<GeminiProviderConfig> {
    use crate::provider_connections::{ProviderFamily, ProviderProtocol};
    let (connection, model) = active_connection(root_config)?;
    anyhow::ensure!(
        matches!(
            (connection.provider, connection.protocol),
            (
                ProviderFamily::Gemini,
                ProviderProtocol::GeminiGenerateContent
            )
        ),
        "active connection is not Gemini"
    );
    let mut config: GeminiProviderConfig = exact_connection_provider_config(&connection, None)?;
    config.model = model;
    Ok(config)
}

fn active_connection(
    root_config: &RootConfig,
) -> Result<(
    crate::provider_connections::ProviderConnectionConfig,
    String,
)> {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    anyhow::ensure!(
        loaded.mode == crate::provider_connections::ConfigMode::V2 && loaded.issues.is_empty(),
        "connection_config_invalid"
    );
    let model = loaded
        .default_model
        .as_ref()
        .ok_or_else(|| anyhow!("model_route_not_configured"))?;
    let connection = loaded
        .connections
        .get(&model.connection_id)
        .ok_or_else(|| anyhow!("connection_not_found"))?;
    Ok((connection.config.clone(), model.model_id.clone()))
}

/// Source used for a resolved runtime secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    Environment(&'static str),
    Session,
}

/// A resolved secret value and the storage layer it came from.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretResolution {
    pub value: String,
    pub source: SecretSource,
}

impl std::fmt::Debug for SecretResolution {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretResolution")
            .field("value", &"[REDACTED]")
            .field("source", &self.source)
            .finish()
    }
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
    _config: &DeepSeekProviderConfig,
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
    None
}

#[must_use]
pub fn resolve_openai_compat_api_key(
    config: &OpenAiCompatibleProviderConfig,
) -> Option<SecretResolution> {
    resolve_openai_compat_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_openai_compat_api_key_with_session(
    _config: &OpenAiCompatibleProviderConfig,
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
    None
}

#[must_use]
pub fn resolve_openai_responses_api_key(
    config: &OpenAiResponsesProviderConfig,
) -> Option<SecretResolution> {
    resolve_openai_responses_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_openai_responses_api_key_with_session(
    _config: &OpenAiResponsesProviderConfig,
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
    None
}

#[must_use]
pub fn resolve_anthropic_api_key(config: &AnthropicProviderConfig) -> Option<SecretResolution> {
    resolve_anthropic_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_anthropic_api_key_with_session(
    _config: &AnthropicProviderConfig,
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
    None
}

#[must_use]
pub fn resolve_gemini_api_key(config: &GeminiProviderConfig) -> Option<SecretResolution> {
    resolve_gemini_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_gemini_api_key_with_session(
    _config: &GeminiProviderConfig,
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
    None
}

#[must_use]
pub fn secret_redactor_for_root_config(root_config: &RootConfig) -> SecretRedactor {
    let mut redactor = SecretRedactor::empty();
    let connections = crate::provider_connections::load_provider_connections(root_config);
    for connection in connections.connections.values() {
        if let crate::provider_connections::CredentialRefConfig::Environment { name } =
            &connection.config.credential
            && let Ok(value) = std::env::var(name)
        {
            let value = value.trim();
            if !value.is_empty() {
                redactor.add_secret(value);
            }
        }
    }
    redactor
}

fn read_secret_env(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[must_use]
pub fn provider_config_key(provider: &str) -> &str {
    provider.trim()
}
