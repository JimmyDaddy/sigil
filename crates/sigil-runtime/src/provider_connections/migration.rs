use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use sigil_kernel::{
    CONFIG_VERSION_V2, ConnectionId, ModelRef, RoleModelConfig, RootConfig, SecretString,
};

use super::{
    ConfigMode, ConnectionConfigIssue, CredentialRefConfig, LoadedConnection, LoadedCredentialRef,
    LoadedProviderConnections, ProviderConnectionConfig, ProviderFamily, ProviderProtocol,
    provider_connection_template,
};

pub fn load_provider_connections(root_config: &RootConfig) -> LoadedProviderConnections {
    match root_config.config_version {
        Some(CONFIG_VERSION_V2)
            if !root_config.providers.is_empty()
                || !root_config.agent.provider.trim().is_empty() =>
        {
            let mut loaded = load_v2_connections(root_config);
            loaded.mode = ConfigMode::Mixed;
            loaded
        }
        Some(CONFIG_VERSION_V2) => load_v2_connections(root_config),
        Some(version) => LoadedProviderConnections {
            mode: ConfigMode::UnsupportedFuture,
            default_model: None,
            connections: BTreeMap::new(),
            issues: vec![ConnectionConfigIssue {
                connection_id: None,
                code: "unsupported_config_version",
                message: format!("unsupported config_version {version}"),
            }],
        },
        None if root_config.connections.is_empty() && root_config.agent.connection.is_none() => {
            load_legacy_connection(root_config)
        }
        None => LoadedProviderConnections {
            mode: ConfigMode::Mixed,
            default_model: None,
            connections: BTreeMap::new(),
            issues: vec![ConnectionConfigIssue {
                connection_id: None,
                code: "mixed_config_schema",
                message: "config_version = 2 is required for [connections] and agent.connection"
                    .to_owned(),
            }],
        },
    }
}

pub fn materialize_v2_root_config(
    root_config: &RootConfig,
    connections: &BTreeMap<ConnectionId, ProviderConnectionConfig>,
    default_model: &ModelRef,
) -> Result<RootConfig> {
    anyhow::ensure!(
        connections.contains_key(&default_model.connection_id),
        "default model references a missing connection"
    );
    let mut raw_connections = BTreeMap::new();
    for (id, connection) in connections {
        anyhow::ensure!(
            id == &connection.id,
            "connection registry key does not match connection identity"
        );
        raw_connections.insert(id.to_string(), connection.to_raw()?);
    }

    let mut next = root_config.clone();
    migrate_role_route(
        &mut next.task.planner,
        root_config,
        connections,
        "task.planner",
    )?;
    migrate_role_route(
        &mut next.task.executor,
        root_config,
        connections,
        "task.executor",
    )?;
    migrate_role_route(
        &mut next.task.subagent_read,
        root_config,
        connections,
        "task.subagent_read",
    )?;
    migrate_role_route(
        &mut next.task.subagent_write,
        root_config,
        connections,
        "task.subagent_write",
    )?;
    next.config_version = Some(CONFIG_VERSION_V2);
    next.agent.provider.clear();
    next.agent.connection = Some(default_model.connection_id.clone());
    next.agent.model = default_model.model_id.clone();
    next.providers.clear();
    next.connections = raw_connections;
    Ok(next)
}

fn load_v2_connections(root_config: &RootConfig) -> LoadedProviderConnections {
    let mut connections = BTreeMap::new();
    let mut issues = Vec::new();
    if !root_config.providers.is_empty() || !root_config.agent.provider.trim().is_empty() {
        issues.push(ConnectionConfigIssue {
            connection_id: None,
            code: "mixed_config_schema",
            message: "V2 config cannot include legacy provider fields".to_owned(),
        });
    }

    for (raw_id, value) in &root_config.connections {
        let id = match ConnectionId::new(raw_id.clone()) {
            Ok(id) => id,
            Err(error) => {
                issues.push(ConnectionConfigIssue {
                    connection_id: Some(safe_issue_identity(raw_id)),
                    code: "invalid_connection_id",
                    message: error.to_string(),
                });
                continue;
            }
        };
        match ProviderConnectionConfig::from_raw(id.clone(), value.clone()) {
            Ok(config) => {
                let credential = LoadedCredentialRef::Config(config.credential.clone());
                connections.insert(id, LoadedConnection { config, credential });
            }
            Err(error) => issues.push(ConnectionConfigIssue {
                connection_id: Some(id.to_string()),
                code: "invalid_connection",
                message: format!("{error:#}"),
            }),
        }
    }

    let default_model = match root_config.agent.connection.clone() {
        Some(connection_id) => {
            match ModelRef::new(connection_id.clone(), root_config.agent.model.clone()) {
                Ok(model_ref) if connections.contains_key(&connection_id) => Some(model_ref),
                Ok(_) => {
                    issues.push(ConnectionConfigIssue {
                        connection_id: Some(connection_id.to_string()),
                        code: "connection_not_found",
                        message: "default model references a missing or invalid connection"
                            .to_owned(),
                    });
                    None
                }
                Err(error) => {
                    issues.push(ConnectionConfigIssue {
                        connection_id: Some(connection_id.to_string()),
                        code: "invalid_model_id",
                        message: error.to_string(),
                    });
                    None
                }
            }
        }
        None => {
            issues.push(ConnectionConfigIssue {
                connection_id: None,
                code: "model_route_not_configured",
                message: "V2 config requires agent.connection".to_owned(),
            });
            None
        }
    };

    LoadedProviderConnections {
        mode: ConfigMode::V2,
        default_model,
        connections,
        issues,
    }
}

fn load_legacy_connection(root_config: &RootConfig) -> LoadedProviderConnections {
    let active_provider = root_config.agent.provider.trim();
    let mut connections = BTreeMap::new();
    let mut issues = Vec::new();
    let mut default_model = None;

    for provider_name in root_config.providers.keys() {
        match project_legacy_connection(root_config, provider_name) {
            Ok((connection, provider_default_model)) => {
                let id = connection.config.id.clone();
                if provider_name == active_provider {
                    match ModelRef::new(id.clone(), root_config.agent.model.clone()) {
                        Ok(model_ref) => default_model = Some(model_ref),
                        Err(error) => issues.push(ConnectionConfigIssue {
                            connection_id: Some(id.to_string()),
                            code: "invalid_model_id",
                            message: error.to_string(),
                        }),
                    }
                } else if let Err(error) = ModelRef::new(id.clone(), provider_default_model) {
                    issues.push(ConnectionConfigIssue {
                        connection_id: Some(id.to_string()),
                        code: "invalid_model_id",
                        message: error.to_string(),
                    });
                }
                connections.insert(id, connection);
            }
            Err(error) => issues.push(ConnectionConfigIssue {
                connection_id: Some(safe_issue_identity(provider_name)),
                code: "invalid_legacy_provider",
                message: format!("{error:#}"),
            }),
        }
    }
    if default_model.is_none() && !root_config.providers.contains_key(active_provider) {
        issues.push(ConnectionConfigIssue {
            connection_id: Some(safe_issue_identity(active_provider)),
            code: "invalid_legacy_provider",
            message: format!("missing [providers.{active_provider}]"),
        });
    }

    LoadedProviderConnections {
        mode: ConfigMode::LegacyV1,
        default_model,
        connections,
        issues,
    }
}

fn project_legacy_connection(
    root_config: &RootConfig,
    provider_name: &str,
) -> Result<(LoadedConnection, String)> {
    let raw = root_config
        .providers
        .get(provider_name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing [providers.{provider_name}]"))?;
    let mut object = raw
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("legacy provider config must be an object"))?;
    let base_url = take_optional_string(&mut object, "base_url")?;
    let inline_api_key = take_optional_string(&mut object, "api_key")?;
    object.remove("__runtime_model");

    let (id, family, protocol, label) = legacy_identity(provider_name, base_url.as_deref())?;
    let id = ConnectionId::new(id)?;
    let (template, provider_default_model) =
        provider_connection_template(family, protocol, id.clone(), label)?;
    let environment_name = match &template.credential {
        CredentialRefConfig::Environment { name } => name.clone(),
        _ => anyhow::bail!("legacy provider template must use an environment reference"),
    };
    let mut options = template
        .options
        .as_object()
        .cloned()
        .context("provider template options must be an object")?;
    options.extend(object);
    let mut config = ProviderConnectionConfig {
        base_url: base_url.unwrap_or(template.base_url),
        options: Value::Object(options),
        ..template
    };
    config.credential = CredentialRefConfig::Environment {
        name: environment_name,
    };
    config.validate()?;

    let credential = inline_api_key
        .filter(|value| !value.trim().is_empty())
        .map(|value| LoadedCredentialRef::LegacyInline(SecretString::new(value)))
        .unwrap_or_else(|| LoadedCredentialRef::Config(config.credential.clone()));
    Ok((
        LoadedConnection { config, credential },
        provider_default_model,
    ))
}

fn migrate_role_route(
    role: &mut RoleModelConfig,
    root_config: &RootConfig,
    connections: &BTreeMap<ConnectionId, ProviderConnectionConfig>,
    field: &str,
) -> Result<()> {
    if let Some(connection) = role.connection.as_ref() {
        anyhow::ensure!(
            role.provider.is_none(),
            "{field} cannot contain both provider and connection"
        );
        anyhow::ensure!(
            connections.contains_key(connection),
            "{field}.connection references a missing connection"
        );
        return Ok(());
    }
    let Some(provider_name) = role.provider.take() else {
        return Ok(());
    };
    let raw = root_config
        .providers
        .get(provider_name.trim())
        .with_context(|| format!("{field}.provider references a missing legacy provider"))?;
    let base_url = raw
        .as_object()
        .and_then(|object| object.get("base_url"))
        .and_then(Value::as_str);
    let (id, family, protocol, label) = legacy_identity(provider_name.trim(), base_url)?;
    let id = ConnectionId::new(id)?;
    anyhow::ensure!(
        connections.contains_key(&id),
        "{field}.provider projection is missing connection {id}"
    );
    if role.model.is_none() {
        let (_, default_model) = provider_connection_template(family, protocol, id.clone(), label)?;
        role.model = Some(default_model);
    }
    role.connection = Some(id);
    Ok(())
}

fn legacy_identity(
    provider_name: &str,
    base_url: Option<&str>,
) -> Result<(&'static str, ProviderFamily, ProviderProtocol, &'static str)> {
    match provider_name {
        "deepseek" => Ok((
            "deepseek-default",
            ProviderFamily::DeepSeek,
            ProviderProtocol::DeepSeek,
            "DeepSeek",
        )),
        "openai_compat" => Ok((
            "openai-compatible-default",
            ProviderFamily::Custom,
            ProviderProtocol::OpenAiChatCompletions,
            "OpenAI-compatible",
        )),
        "openai_responses" if is_official_openai_endpoint(base_url) => Ok((
            "openai-default",
            ProviderFamily::OpenAi,
            ProviderProtocol::OpenAiResponses,
            "OpenAI",
        )),
        "openai_responses" => Ok((
            "openai-responses-default",
            ProviderFamily::Custom,
            ProviderProtocol::OpenAiResponses,
            "OpenAI-compatible Responses",
        )),
        "anthropic" => Ok((
            "anthropic-default",
            ProviderFamily::Anthropic,
            ProviderProtocol::AnthropicMessages,
            "Anthropic",
        )),
        "gemini" => Ok((
            "gemini-default",
            ProviderFamily::Gemini,
            ProviderProtocol::GeminiGenerateContent,
            "Google Gemini",
        )),
        other => anyhow::bail!("unsupported legacy provider {other}"),
    }
}

fn is_official_openai_endpoint(base_url: Option<&str>) -> bool {
    base_url
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/')
        .eq_ignore_ascii_case("https://api.openai.com/v1")
}

fn take_optional_string(object: &mut Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(anyhow::anyhow!("legacy provider {key} must be a string")),
    }
    .with_context(|| format!("invalid legacy provider field {key}"))
}

fn safe_issue_identity(raw: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(raw.as_bytes());
    let short_hash = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("!invalid-{short_hash}")
}
