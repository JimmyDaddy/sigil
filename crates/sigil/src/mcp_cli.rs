use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use sigil_kernel::{
    McpServerConfig, McpServerStartup, McpServerTransportConfig, McpServerTrustPolicy,
    McpStreamableHttpConfig, McpTrustClass, RootConfig, normalize_environment_variable_names,
};
use url::Url;

#[derive(Subcommand)]
pub(crate) enum McpCommand {
    /// Add one local stdio or remote Streamable HTTP MCP server.
    Add {
        /// Stable server name used in generated tool names.
        name: String,
        /// Remote Streamable HTTP endpoint. Omit for a stdio command.
        #[arg(long)]
        url: Option<String>,
        /// Environment variable containing a remote bearer token.
        #[arg(long = "bearer-token-env-var", requires = "url")]
        bearer_token_env_var: Option<String>,
        /// Environment variable name explicitly inherited by a stdio server.
        #[arg(long = "inherit-env")]
        inherit_env: Vec<String>,
        /// Fail strict/headless startup when this server is unavailable.
        #[arg(long, default_value_t = false)]
        required: bool,
        /// Start the server eagerly or defer activation until first use.
        #[arg(long, value_enum, default_value_t = McpStartupArg::Eager)]
        startup: McpStartupArg,
        /// Stdio executable followed by arguments. Put `--` before this command.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// List configured MCP servers without printing commands, arguments, or credential values.
    List {
        /// Emit a stable JSON array.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Inspect one MCP server without printing arguments or credential values.
    Get {
        /// Exact configured server name.
        name: String,
        /// Emit one stable JSON object.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Remove one MCP server by exact configured name.
    Remove {
        /// Exact configured server name.
        name: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum McpStartupArg {
    Eager,
    Lazy,
}

impl From<McpStartupArg> for McpServerStartup {
    fn from(value: McpStartupArg) -> Self {
        match value {
            McpStartupArg::Eager => Self::Eager,
            McpStartupArg::Lazy => Self::Lazy,
        }
    }
}

pub(crate) fn execute_mcp_command(config_path: &Path, command: McpCommand) -> Result<String> {
    match command {
        McpCommand::Add {
            name,
            url,
            bearer_token_env_var,
            mut inherit_env,
            required,
            startup,
            command,
        } => {
            validate_server_name(&name)?;
            let mut loaded = load_config_for_update(config_path)?;
            if loaded
                .config
                .mcp_servers
                .iter()
                .any(|server| server.name == name)
            {
                bail!("MCP server {name:?} is already configured");
            }
            inherit_env.sort();
            inherit_env.dedup();
            normalize_environment_variable_names(&inherit_env)
                .context("invalid --inherit-env value")?;
            if url.is_some() && !inherit_env.is_empty() {
                bail!("--inherit-env is only valid for a stdio MCP command");
            }
            let (transport, trust_class) = match (url, command.as_slice()) {
                (Some(url), []) => {
                    validate_remote_endpoint(&url, bearer_token_env_var.as_deref())?;
                    (
                        McpServerTransportConfig::StreamableHttp(McpStreamableHttpConfig {
                            url,
                            http_headers: BTreeMap::new(),
                            env_http_headers: BTreeMap::new(),
                            bearer_token_env_var,
                            oauth: None,
                            client_capabilities: BTreeSet::new(),
                        }),
                        McpTrustClass::ThirdParty,
                    )
                }
                (None, [executable, args @ ..]) => {
                    if bearer_token_env_var.is_some() {
                        bail!("--bearer-token-env-var requires --url");
                    }
                    if executable.trim().is_empty() {
                        bail!("stdio MCP command cannot be empty");
                    }
                    (
                        McpServerTransportConfig::Stdio {
                            command: executable.clone(),
                            args: args.to_vec(),
                            inherit_env,
                        },
                        McpTrustClass::SelfHosted,
                    )
                }
                (Some(_), _) => bail!("--url cannot be combined with a stdio command"),
                (None, []) => bail!("MCP add requires either --url or `-- <command> [args...]`"),
            };
            let server = McpServerConfig {
                name: name.clone(),
                transport,
                startup_timeout_secs: 10,
                required,
                startup: startup.into(),
                trust: McpServerTrustPolicy {
                    trust_class,
                    ..McpServerTrustPolicy::default()
                },
            };
            loaded.config.mcp_servers.push(server);
            loaded
                .config
                .mcp_servers
                .sort_by(|left, right| left.name.cmp(&right.name));
            publish_config_update(config_path, loaded)?;
            Ok(format!(
                "Added MCP server {}. It will be available on the next Sigil run.\n",
                quoted(&name)
            ))
        }
        McpCommand::List { json } => {
            let mut config = RootConfig::load(config_path).with_context(|| {
                format!("failed to load Sigil config at {}", config_path.display())
            })?;
            config
                .mcp_servers
                .sort_by(|left, right| left.name.cmp(&right.name));
            if json {
                let servers = config
                    .mcp_servers
                    .iter()
                    .map(mcp_server_summary)
                    .collect::<Vec<_>>();
                Ok(format!("{}\n", serde_json::to_string_pretty(&servers)?))
            } else if config.mcp_servers.is_empty() {
                Ok("No MCP servers configured.\n".to_owned())
            } else {
                let mut output = String::new();
                for server in &config.mcp_servers {
                    output.push_str(&format!(
                        "{}\t{}\tstartup={}\trequired={}\ttrust={}\tapproval={}\n",
                        quoted(&server.name),
                        server.transport_name(),
                        server.startup.as_str(),
                        server.required,
                        server.trust.trust_class.as_str(),
                        server.trust.approval_default.as_str(),
                    ));
                }
                Ok(output)
            }
        }
        McpCommand::Get { name, json } => {
            let config = RootConfig::load(config_path).with_context(|| {
                format!("failed to load Sigil config at {}", config_path.display())
            })?;
            let server = config
                .mcp_servers
                .iter()
                .find(|server| server.name == name)
                .with_context(|| format!("MCP server {name:?} is not configured"))?;
            if json {
                Ok(format!(
                    "{}\n",
                    serde_json::to_string_pretty(&mcp_server_detail(server))?
                ))
            } else {
                Ok(render_mcp_server_detail(server))
            }
        }
        McpCommand::Remove { name } => {
            let mut loaded = load_config_for_update(config_path)?;
            let original_len = loaded.config.mcp_servers.len();
            loaded
                .config
                .mcp_servers
                .retain(|server| server.name != name);
            if loaded.config.mcp_servers.len() == original_len {
                bail!("MCP server {name:?} is not configured");
            }
            publish_config_update(config_path, loaded)?;
            Ok(format!("Removed MCP server {}.\n", quoted(&name)))
        }
    }
}

fn validate_server_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("MCP server name cannot be empty");
    }
    if name.trim() != name {
        bail!("MCP server name cannot contain leading or trailing whitespace");
    }
    if name.starts_with("builtin:") {
        bail!("MCP server name uses reserved builtin: namespace");
    }
    Ok(())
}

fn validate_remote_endpoint(value: &str, bearer_token_env_var: Option<&str>) -> Result<()> {
    let endpoint = Url::parse(value).context("streamable_http MCP url is invalid")?;
    if !matches!(endpoint.scheme(), "https" | "http") {
        bail!("streamable_http MCP url must use https or http");
    }
    if endpoint.host_str().is_none() {
        bail!("streamable_http MCP url must include a host");
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        bail!("streamable_http MCP url cannot contain userinfo");
    }
    if endpoint.fragment().is_some() {
        bail!("streamable_http MCP url cannot contain a fragment");
    }
    if let Some(environment) = bearer_token_env_var {
        normalize_environment_variable_names(&[environment.to_owned()])
            .context("invalid --bearer-token-env-var value")?;
        if endpoint.scheme() != "https" {
            bail!("streamable_http MCP credentials require https");
        }
    }
    Ok(())
}

fn mcp_server_summary(server: &McpServerConfig) -> serde_json::Value {
    serde_json::json!({
        "name": server.name,
        "transport": server.transport_name(),
        "startup": server.startup.as_str(),
        "required": server.required,
        "trust_class": server.trust.trust_class.as_str(),
        "approval_default": server.trust.approval_default.as_str(),
        "allow_secrets": server.trust.allow_secrets,
    })
}

fn mcp_server_detail(server: &McpServerConfig) -> serde_json::Value {
    let mut detail = mcp_server_summary(server);
    let object = detail
        .as_object_mut()
        .expect("MCP summary projection is always an object");
    object.insert(
        "startup_timeout_secs".to_owned(),
        serde_json::json!(server.startup_timeout_secs),
    );
    object.insert(
        "pin_version".to_owned(),
        serde_json::json!(server.trust.pin_version),
    );
    object.insert(
        "transport_detail".to_owned(),
        match &server.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                inherit_env,
            } => serde_json::json!({
                "command": command,
                "argument_count": args.len(),
                "arguments": "redacted",
                "inherited_environment": inherit_env,
            }),
            McpServerTransportConfig::StreamableHttp(remote) => {
                let mut credential_environment = remote
                    .env_http_headers
                    .values()
                    .chain(remote.bearer_token_env_var.iter())
                    .cloned()
                    .collect::<Vec<_>>();
                credential_environment.sort();
                credential_environment.dedup();
                let header_names = remote
                    .http_headers
                    .keys()
                    .chain(remote.env_http_headers.keys())
                    .cloned()
                    .collect::<Vec<_>>();
                let oauth = remote.oauth.as_ref().map_or_else(
                    || serde_json::json!({"state": "off"}),
                    |oauth| {
                        serde_json::json!({
                            "state": "configured",
                            "client_registration": if oauth.client_id.is_some() {
                                "static"
                            } else {
                                "dynamic"
                            },
                            "scopes": oauth.scopes,
                        })
                    },
                );
                serde_json::json!({
                    "destination": safe_remote_destination(&remote.url),
                    "authorization": remote_authorization_kind(remote),
                    "credential_environment": credential_environment,
                    "header_names": header_names,
                    "oauth": oauth,
                    "client_capabilities": remote.client_capabilities.iter().map(|capability| {
                        match capability {
                            sigil_kernel::McpRemoteClientCapability::Roots => "roots",
                            sigil_kernel::McpRemoteClientCapability::ElicitationForm => "elicitation",
                        }
                    }).collect::<Vec<_>>(),
                })
            }
        },
    );
    detail
}

fn render_mcp_server_detail(server: &McpServerConfig) -> String {
    let mut output = format!(
        "Name: {}\nTransport: {}\nStartup: {}\nStartup timeout: {}s\nRequired: {}\nTrust: {}\nApproval: {}\nSecrets: {}\nPin: {}\n",
        quoted(&server.name),
        server.transport_name(),
        server.startup.as_str(),
        server.startup_timeout_secs,
        server.required,
        server.trust.trust_class.as_str(),
        server.trust.approval_default.as_str(),
        if server.trust.allow_secrets {
            "allowed"
        } else {
            "blocked"
        },
        if server.trust.pin_version {
            "required"
        } else {
            "off"
        },
    );
    match &server.transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            inherit_env,
        } => {
            output.push_str(&format!(
                "Command: {}\nArguments: {} configured (redacted)\nInherited environment: {}\n",
                quoted(command),
                args.len(),
                joined_or_none(inherit_env),
            ));
        }
        McpServerTransportConfig::StreamableHttp(remote) => {
            let mut credential_environment = remote
                .env_http_headers
                .values()
                .chain(remote.bearer_token_env_var.iter())
                .cloned()
                .collect::<Vec<_>>();
            credential_environment.sort();
            credential_environment.dedup();
            output.push_str(&format!(
                "Destination: {}\nAuthorization: {}\nCredential environment: {}\nOAuth: {}\n",
                safe_remote_destination(&remote.url),
                remote_authorization_kind(remote),
                joined_or_none(&credential_environment),
                remote.oauth.as_ref().map_or("off", |_| "configured"),
            ));
        }
    }
    output
}

fn safe_remote_destination(value: &str) -> String {
    Url::parse(value)
        .ok()
        .map(|endpoint| format!("{}/", endpoint.origin().ascii_serialization()))
        .unwrap_or_else(|| "invalid".to_owned())
}

fn remote_authorization_kind(config: &McpStreamableHttpConfig) -> &'static str {
    if config.oauth.is_some() {
        "oauth"
    } else if config.bearer_token_env_var.is_some() {
        "bearer_env"
    } else if config
        .env_http_headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("authorization"))
    {
        "header_env"
    } else {
        "none"
    }
}

fn joined_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

struct LoadedConfigForUpdate {
    config: RootConfig,
    source_bytes: Vec<u8>,
}

fn load_config_for_update(path: &Path) -> Result<LoadedConfigForUpdate> {
    if !path.is_file() {
        bail!(
            "Sigil config does not exist at {}; finish Quick Setup or pass --config first",
            path.display()
        );
    }
    let source_bytes =
        fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    RootConfig::load(path)
        .with_context(|| format!("failed to validate Sigil config at {}", path.display()))?;
    let config =
        toml::from_str(std::str::from_utf8(&source_bytes).context("Sigil config is not UTF-8")?)
            .with_context(|| format!("failed to parse {}", path.display()))?;
    if fs::read(path).ok().as_deref() != Some(source_bytes.as_slice()) {
        bail!("Sigil config changed while it was being loaded; retry the command");
    }
    Ok(LoadedConfigForUpdate {
        config,
        source_bytes,
    })
}

fn publish_config_update(path: &Path, loaded: LoadedConfigForUpdate) -> Result<()> {
    loaded
        .config
        .save_if_source_bytes_unchanged(path, &loaded.source_bytes)
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"<invalid>\"".to_owned())
}

#[cfg(test)]
#[path = "tests/mcp_cli_tests.rs"]
mod tests;
