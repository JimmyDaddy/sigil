use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permission::PermissionConfig;

/// Root runtime configuration shared by the TUI, CLI, kernel, and adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RootConfig {
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub session: SessionConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub permission: PermissionConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub compaction: CompactionConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl RootConfig {
    /// Loads and parses a TOML configuration file from `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// Serializes the config to TOML and writes it to `path`, creating parent directories first.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let rendered =
            toml::to_string_pretty(self).context("failed to serialize root config to toml")?;
        fs::write(path, rendered)
            .with_context(|| format!("failed to write config at {}", path.display()))
    }
}

/// Returns the standard per-user config directory for termquill.
///
/// # Errors
///
/// Returns an error when the current platform does not expose a usable home or config directory.
pub fn default_user_config_dir() -> Result<PathBuf> {
    if cfg!(target_os = "windows") {
        if let Some(app_data) = env::var_os("APPDATA") {
            return Ok(PathBuf::from(app_data).join("termquill"));
        }
        return Err(anyhow::anyhow!(
            "missing APPDATA for termquill config directory"
        ));
    }

    if cfg!(target_os = "macos") {
        let home = env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("missing HOME for termquill config directory"))?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("termquill"));
    }

    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("termquill"));
    }

    let home = env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("missing HOME for termquill config directory"))?;
    Ok(PathBuf::from(home).join(".config").join("termquill"))
}

/// Returns the standard per-user config file path for termquill.
///
/// # Errors
///
/// Returns an error when the current platform does not expose a usable config directory.
pub fn default_user_config_path() -> Result<PathBuf> {
    Ok(default_user_config_dir()?.join("termquill.toml"))
}

/// Resolves the config path that entrypoints should prefer on startup.
///
/// Explicit paths always win. Otherwise a local `termquill.toml` inside `cwd` wins over the
/// per-user config directory, so repository-local development keeps working naturally.
///
/// # Errors
///
/// Returns an error when the implicit per-user config directory cannot be determined.
pub fn preferred_config_path(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let local = cwd.join("termquill.toml");
    if local.exists() {
        return Ok(local);
    }

    default_user_config_path()
}

/// Resolves the effective workspace root for one launch.
///
/// Relative paths normally stay anchored to the config file location. The default `"."`
/// is treated specially so user-level configs can follow the directory where the user
/// launched termquill instead of pinning every session to the config folder.
pub fn resolve_workspace_root(
    config_path: &Path,
    launch_cwd: &Path,
    configured_root: &str,
) -> PathBuf {
    let trimmed = configured_root.trim();
    let requested = if trimmed.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(trimmed)
    };

    if requested.is_absolute() {
        return requested;
    }
    if requested == Path::new(".") {
        return launch_cwd.to_path_buf();
    }

    let base = config_path.parent().unwrap_or_else(|| Path::new("."));
    base.join(requested)
}

/// Workspace-level configuration used to resolve confinement and relative paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace_root")]
    pub root: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: default_workspace_root(),
        }
    }
}

/// Session persistence configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionConfig {
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            log_dir: default_log_dir(),
        }
    }
}

/// Default agent execution parameters shared across entrypoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentConfig {
    pub provider: String,
    pub model: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default = "default_timeout_secs")]
    pub tool_timeout_secs: u64,
}

/// Workspace memory boot configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MemoryConfig {
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
        }
    }
}

/// Context compaction configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CompactionConfig {
    #[serde(default = "default_compaction_enabled")]
    pub enabled: bool,
    #[serde(default = "default_soft_threshold_ratio")]
    pub soft_threshold_ratio: f32,
    #[serde(default = "default_hard_threshold_ratio")]
    pub hard_threshold_ratio: f32,
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
    #[serde(default = "default_tail_messages")]
    pub tail_messages: usize,
}

/// Threshold state derived from the latest provider-reported prompt size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionThresholdStatus {
    Off,
    NotAvailable,
    Ready,
    Soft,
    Hard,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_compaction_enabled(),
            soft_threshold_ratio: default_soft_threshold_ratio(),
            hard_threshold_ratio: default_hard_threshold_ratio(),
            context_window_tokens: None,
            tail_messages: default_tail_messages(),
        }
    }
}

impl CompactionConfig {
    /// Classifies the latest prompt token count against the configured compaction thresholds.
    pub fn threshold_status(&self, prompt_tokens: u64) -> CompactionThresholdStatus {
        if !self.enabled {
            return CompactionThresholdStatus::Off;
        }

        let Some(window) = self.context_window_tokens else {
            return CompactionThresholdStatus::NotAvailable;
        };
        if window == 0 {
            return CompactionThresholdStatus::NotAvailable;
        }

        let ratio = prompt_tokens as f32 / window as f32;
        if ratio >= self.hard_threshold_ratio {
            CompactionThresholdStatus::Hard
        } else if ratio >= self.soft_threshold_ratio {
            CompactionThresholdStatus::Soft
        } else {
            CompactionThresholdStatus::Ready
        }
    }
}

impl CompactionThresholdStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::NotAvailable => "n/a",
            Self::Ready => "ready",
            Self::Soft => "soft",
            Self::Hard => "hard",
        }
    }
}

/// External MCP server process configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
}

fn default_workspace_root() -> String {
    ".".to_owned()
}

fn default_log_dir() -> String {
    ".termquill/sessions".to_owned()
}

fn default_max_turns() -> usize {
    8
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_startup_timeout_secs() -> u64 {
    10
}

fn default_memory_enabled() -> bool {
    true
}

fn default_compaction_enabled() -> bool {
    true
}

fn default_soft_threshold_ratio() -> f32 {
    0.5
}

fn default_hard_threshold_ratio() -> f32 {
    0.8
}

fn default_tail_messages() -> usize {
    6
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
