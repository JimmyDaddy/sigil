use anyhow::{Result, anyhow, bail};
use sigil_kernel::McpServerConfig;

#[derive(Debug, Clone)]
pub(crate) struct McpServerDraft {
    pub(crate) name: String,
    pub(crate) command: String,
    pub(crate) args_csv: String,
    pub(crate) startup_timeout_secs: String,
    pub(crate) inherit_env: Vec<String>,
    pub(crate) base_config: McpServerConfig,
}

impl McpServerDraft {
    pub(super) fn from_config(config: &McpServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            command: config.command.clone(),
            args_csv: config.args.join(", "),
            startup_timeout_secs: config.startup_timeout_secs.to_string(),
            inherit_env: config.inherit_env.clone(),
            base_config: config.clone(),
        }
    }

    pub(super) fn new_default(name: String) -> Self {
        let config = McpServerConfig {
            name,
            command: "npx".to_owned(),
            ..McpServerConfig::default()
        };
        Self::from_config(&config)
    }

    pub(super) fn to_config(&self, index: usize) -> Result<McpServerConfig> {
        let name = self.name.trim();
        if name.is_empty() {
            bail!("mcp server {} name cannot be empty", index + 1);
        }
        let command = self.command.trim();
        if command.is_empty() {
            bail!("mcp server {} command cannot be empty", index + 1);
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
        config.command = command.to_owned();
        config.args = if self.args_csv == self.base_config.args.join(", ") {
            self.base_config.args.clone()
        } else {
            self.args_csv
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        };
        config.inherit_env = sigil_kernel::normalize_environment_variable_names(&self.inherit_env)?;
        config.startup_timeout_secs = startup_timeout_secs;
        Ok(config)
    }
}
