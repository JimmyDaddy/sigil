use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use sigil_kernel::{
    McpServerConfig, McpServerStartup, McpServerTransportConfig, McpServerTrustPolicy,
    McpStreamableHttpConfig, McpTrustClass, RootConfig,
};

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
            if url.is_some() && !inherit_env.is_empty() {
                bail!("--inherit-env is only valid for a stdio MCP command");
            }
            let (transport, trust_class) = match (url, command.as_slice()) {
                (Some(url), []) => (
                    McpServerTransportConfig::StreamableHttp(McpStreamableHttpConfig {
                        url,
                        http_headers: BTreeMap::new(),
                        env_http_headers: BTreeMap::new(),
                        bearer_token_env_var,
                        oauth: None,
                        client_capabilities: BTreeSet::new(),
                    }),
                    McpTrustClass::ThirdParty,
                ),
                (None, [executable, args @ ..]) => {
                    if bearer_token_env_var.is_some() {
                        bail!("--bearer-token-env-var requires --url");
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
                    .map(|server| {
                        serde_json::json!({
                            "name": server.name,
                            "transport": server.transport_name(),
                            "startup": server.startup.as_str(),
                            "required": server.required,
                            "trust_class": server.trust.trust_class.as_str(),
                            "approval_default": server.trust.approval_default.as_str(),
                            "allow_secrets": server.trust.allow_secrets,
                        })
                    })
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
    if fs::read(path).ok().as_deref() != Some(loaded.source_bytes.as_slice()) {
        bail!("Sigil config changed before publication; retry the command");
    }
    loaded.config.save(path)
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"<invalid>\"".to_owned())
}

#[cfg(test)]
#[path = "tests/mcp_cli_tests.rs"]
mod tests;
