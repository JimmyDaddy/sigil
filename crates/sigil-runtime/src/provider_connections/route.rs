use anyhow::Result;
use sigil_kernel::{ModelRef, ResolvedModelRoute, RootConfig};

use super::{
    ConfigMode, ProviderConnectionConfig, ProviderFamily, ProviderProtocol,
    connection_semantic_fingerprint, load_provider_connections,
};

#[derive(Debug, thiserror::Error)]
pub enum ResolvedRouteError {
    #[error("model_route_not_configured")]
    NotConfigured,
    #[error("connection_not_found")]
    ConnectionNotFound,
    #[error("connection_config_invalid")]
    ConnectionConfigInvalid,
    #[error("session_route_drift")]
    SemanticDrift,
}

pub fn resolve_default_model_route(
    root_config: &RootConfig,
) -> std::result::Result<(String, ResolvedModelRoute), ResolvedRouteError> {
    let loaded = load_provider_connections(root_config);
    if loaded.mode != ConfigMode::V2 {
        return Err(ResolvedRouteError::ConnectionConfigInvalid);
    }
    let model_ref = loaded
        .default_model
        .as_ref()
        .ok_or(ResolvedRouteError::NotConfigured)?;
    resolve_model_route(root_config, model_ref)
}

pub fn resolve_model_route(
    root_config: &RootConfig,
    model_ref: &ModelRef,
) -> std::result::Result<(String, ResolvedModelRoute), ResolvedRouteError> {
    let loaded = load_provider_connections(root_config);
    if loaded.mode != ConfigMode::V2 {
        return Err(ResolvedRouteError::ConnectionConfigInvalid);
    }
    if loaded.issues.iter().any(|issue| {
        issue.connection_id.is_none()
            || issue.connection_id.as_deref() == Some(model_ref.connection_id.as_str())
    }) {
        return Err(ResolvedRouteError::ConnectionConfigInvalid);
    }
    let connection = loaded
        .connections
        .get(&model_ref.connection_id)
        .ok_or(ResolvedRouteError::ConnectionNotFound)?;
    let route = ResolvedModelRoute::new(
        model_ref.clone(),
        connection.config.provider.as_str(),
        connection.config.protocol.as_str(),
        connection_semantic_fingerprint(&connection.config),
    )
    .map_err(|_| ResolvedRouteError::ConnectionConfigInvalid)?;
    Ok((runtime_provider_name(&connection.config).to_owned(), route))
}

pub fn validate_persisted_model_route(
    root_config: &RootConfig,
    persisted: &ResolvedModelRoute,
) -> std::result::Result<String, ResolvedRouteError> {
    let (provider_name, current) = resolve_model_route(root_config, &persisted.model_ref)?;
    if current.provider_family != persisted.provider_family
        || current.protocol != persisted.protocol
        || current.semantic_fingerprint != persisted.semantic_fingerprint
    {
        return Err(ResolvedRouteError::SemanticDrift);
    }
    Ok(provider_name)
}

#[must_use]
pub fn runtime_provider_name(connection: &ProviderConnectionConfig) -> &'static str {
    match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => "deepseek",
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => "openai_responses",
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => "openai_compat",
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => "anthropic",
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => "gemini",
        _ => "unsupported",
    }
}

pub fn ensure_route_is_current(root_config: &RootConfig, route: &ResolvedModelRoute) -> Result<()> {
    validate_persisted_model_route(root_config, route)
        .map(|_| ())
        .map_err(anyhow::Error::new)
}
