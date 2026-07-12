use anyhow::{Result, anyhow, bail};
use sigil_kernel::{McpServerConfig, McpServerTransportConfig};

/// User-visible transport choice for one root MCP entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum McpTransportDraft {
    #[default]
    Stdio,
    StreamableHttp,
}

#[derive(Debug, Clone)]
pub(crate) struct McpServerDraft {
    pub(crate) name: String,
    pub(crate) command: String,
    pub(crate) args_csv: String,
    pub(crate) startup_timeout_secs: String,
    pub(crate) inherit_env: Vec<String>,
    pub(crate) transport: McpTransportDraft,
    pub(crate) base_config: McpServerConfig,
}

impl McpServerDraft {
    pub(super) fn from_config(config: &McpServerConfig) -> Self {
        let (transport, command, args_csv, inherit_env) = match &config.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                inherit_env,
            } => (
                McpTransportDraft::Stdio,
                command.clone(),
                args.join(", "),
                inherit_env.clone(),
            ),
            McpServerTransportConfig::StreamableHttp(_) => (
                McpTransportDraft::StreamableHttp,
                String::new(),
                String::new(),
                Vec::new(),
            ),
        };
        Self {
            name: config.name.clone(),
            command,
            args_csv,
            startup_timeout_secs: config.startup_timeout_secs.to_string(),
            inherit_env,
            transport,
            base_config: config.clone(),
        }
    }

    pub(super) fn new_default(name: String) -> Self {
        let config = McpServerConfig {
            name,
            transport: McpServerTransportConfig::Stdio {
                command: "npx".to_owned(),
                args: Vec::new(),
                inherit_env: Vec::new(),
            },
            ..McpServerConfig::default()
        };
        Self::from_config(&config)
    }

    pub(super) fn to_config(&self, index: usize) -> Result<McpServerConfig> {
        let name = self.name.trim();
        if name.is_empty() {
            bail!("mcp server {} name cannot be empty", index + 1);
        }
        let startup_timeout_secs =
            self.startup_timeout_secs
                .trim()
                .parse::<u64>()
                .map_err(|error| {
                    anyhow!(
                        "mcp server {} startup_timeout_secs must be a positive integer: {error}",
                        index + 1
                    )
                })?;
        if startup_timeout_secs == 0 {
            bail!(
                "mcp server {} startup_timeout_secs must be greater than 0",
                index + 1
            );
        }

        let mut config = self.base_config.clone();
        config.name = name.to_owned();
        if self.transport == McpTransportDraft::Stdio {
            let command = self.command.trim();
            if command.is_empty() {
                bail!("mcp server {} command cannot be empty", index + 1);
            }
            let base_args = self
                .base_config
                .stdio()
                .map(|(_, args, _)| args.join(", "))
                .unwrap_or_default();
            let args = if self.args_csv == base_args {
                self.base_config
                    .stdio()
                    .map(|(_, args, _)| args.to_vec())
                    .unwrap_or_default()
            } else {
                self.args_csv
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            };
            config.transport = McpServerTransportConfig::Stdio {
                command: command.to_owned(),
                args,
                inherit_env: sigil_kernel::normalize_environment_variable_names(&self.inherit_env)?,
            };
        } else if config.streamable_http().is_none() {
            bail!(
                "mcp server {} select a configured streamable_http entry before saving",
                index + 1
            );
        }
        config.startup_timeout_secs = startup_timeout_secs;
        Ok(config)
    }

    /// Changes transport only after explicit confirmation because variant fields are exclusive.
    #[cfg(test)]
    pub(super) fn switch_transport(
        &mut self,
        transport: McpTransportDraft,
        confirmed: bool,
    ) -> Result<()> {
        if self.transport == transport {
            return Ok(());
        }
        if !confirmed {
            bail!("confirm transport change before clearing mutually exclusive MCP fields");
        }
        self.transport = transport;
        Ok(())
    }
}
