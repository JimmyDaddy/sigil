use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    permission::{ApprovalMode, PermissionConfig},
    provider::ReasoningEffort,
    task::AgentRole,
};

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
    pub skills: SkillConfig,
    #[serde(default)]
    pub compaction: CompactionConfig,
    #[serde(default)]
    pub code_intelligence: CodeIntelligenceConfig,
    #[serde(default)]
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub task: TaskConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

/// Local code intelligence configuration.
///
/// This config is parsed by the shared root config so entrypoints preserve it while
/// `sigil-code-intel` owns the actual LSP lifecycle and language analysis behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CodeIntelligenceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub startup: CodeIntelStartup,
    #[serde(default = "default_code_intel_timeout_ms")]
    pub default_timeout_ms: u64,
    #[serde(default = "default_code_intel_max_results")]
    pub max_results: usize,
    #[serde(default = "default_code_intel_max_payload_bytes")]
    pub max_payload_bytes: usize,
    #[serde(default)]
    pub discovery: CodeIntelligenceDiscoveryConfig,
    #[serde(default)]
    pub servers: Vec<LanguageServerConfig>,
}

impl Default for CodeIntelligenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            startup: CodeIntelStartup::default(),
            default_timeout_ms: default_code_intel_timeout_ms(),
            max_results: default_code_intel_max_results(),
            max_payload_bytes: default_code_intel_max_payload_bytes(),
            discovery: CodeIntelligenceDiscoveryConfig::default(),
            servers: Vec::new(),
        }
    }
}

/// Automatic language server discovery controls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeIntelligenceDiscoveryConfig {
    #[serde(default = "default_code_intel_discovery_enabled")]
    pub enabled: bool,
    #[serde(default = "default_code_intel_discovery_report_missing")]
    pub report_missing: bool,
}

impl Default for CodeIntelligenceDiscoveryConfig {
    fn default() -> Self {
        Self {
            enabled: default_code_intel_discovery_enabled(),
            report_missing: default_code_intel_discovery_report_missing(),
        }
    }
}

/// Terminal integration controls for interactive entrypoints.
pub const DEFAULT_TERMINAL_SCROLL_SENSITIVITY: u16 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalConfig {
    #[serde(default = "default_terminal_mouse_capture")]
    pub mouse_capture: bool,
    #[serde(default = "default_terminal_osc52_clipboard")]
    pub osc52_clipboard: bool,
    #[serde(default = "default_terminal_scroll_sensitivity")]
    pub scroll_sensitivity: u16,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            mouse_capture: default_terminal_mouse_capture(),
            osc52_clipboard: default_terminal_osc52_clipboard(),
            scroll_sensitivity: default_terminal_scroll_sensitivity(),
        }
    }
}

/// TUI appearance preferences shared by interactive entrypoints.
///
/// Theme choices are user-interface preferences only. They must not affect session history,
/// provider-visible request material, tool approval audit entries, or cache-stable state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AppearanceConfig {
    #[serde(default)]
    pub theme: ThemeId,
    #[serde(default)]
    pub syntax_theme: SyntaxThemeId,
    #[serde(default, skip_serializing_if = "ThemeColorOverrides::is_empty")]
    pub colors: ThemeColorOverrides,
}

/// Stable identifiers for built-in TUI themes.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeId {
    #[default]
    SigilDark,
    SolarizedDark,
    SolarizedLight,
    GruvboxDark,
    Nord,
    HighContrastDark,
}

impl ThemeId {
    pub const ALL: [Self; 6] = [
        Self::SigilDark,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::GruvboxDark,
        Self::Nord,
        Self::HighContrastDark,
    ];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SigilDark => "sigil_dark",
            Self::SolarizedDark => "solarized_dark",
            Self::SolarizedLight => "solarized_light",
            Self::GruvboxDark => "gruvbox_dark",
            Self::Nord => "nord",
            Self::HighContrastDark => "high_contrast_dark",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::SigilDark => "Sigil Dark",
            Self::SolarizedDark => "Solarized Dark",
            Self::SolarizedLight => "Solarized Light",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::Nord => "Nord",
            Self::HighContrastDark => "High Contrast Dark",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|theme| *theme == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// Stable identifiers for syntax highlighting themes used by TUI markdown/code previews.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SyntaxThemeId {
    #[default]
    Auto,
    CatppuccinMocha,
    CatppuccinLatte,
    SolarizedDark,
    SolarizedLight,
    GruvboxDark,
    GruvboxLight,
    Nord,
    OneHalfDark,
    OneHalfLight,
    Monokai,
}

impl SyntaxThemeId {
    pub const ALL: [Self; 11] = [
        Self::Auto,
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::GruvboxDark,
        Self::GruvboxLight,
        Self::Nord,
        Self::OneHalfDark,
        Self::OneHalfLight,
        Self::Monokai,
    ];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::CatppuccinMocha => "catppuccin_mocha",
            Self::CatppuccinLatte => "catppuccin_latte",
            Self::SolarizedDark => "solarized_dark",
            Self::SolarizedLight => "solarized_light",
            Self::GruvboxDark => "gruvbox_dark",
            Self::GruvboxLight => "gruvbox_light",
            Self::Nord => "nord",
            Self::OneHalfDark => "one_half_dark",
            Self::OneHalfLight => "one_half_light",
            Self::Monokai => "monokai",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::SolarizedDark => "Solarized Dark",
            Self::SolarizedLight => "Solarized Light",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::GruvboxLight => "Gruvbox Light",
            Self::Nord => "Nord",
            Self::OneHalfDark => "One Half Dark",
            Self::OneHalfLight => "One Half Light",
            Self::Monokai => "Monokai",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|theme| *theme == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn resolved_for_theme(self, theme: ThemeId) -> Self {
        if self != Self::Auto {
            return self;
        }
        match theme {
            ThemeId::SigilDark => Self::CatppuccinMocha,
            ThemeId::SolarizedDark => Self::SolarizedDark,
            ThemeId::SolarizedLight => Self::SolarizedLight,
            ThemeId::GruvboxDark => Self::GruvboxDark,
            ThemeId::Nord => Self::Nord,
            ThemeId::HighContrastDark => Self::OneHalfDark,
        }
    }
}

/// Raw user-provided semantic color overrides.
///
/// Values stay as strings here so the kernel remains independent from any terminal renderer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ThemeColorOverrides {
    values: BTreeMap<String, String>,
}

impl ThemeColorOverrides {
    pub fn new(values: BTreeMap<String, String>) -> Self {
        Self { values }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.values.insert(key.into(), value.into())
    }

    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.values.remove(key)
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// Code intelligence service startup strategy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeIntelStartup {
    Off,
    #[default]
    Lazy,
    Eager,
}

impl CodeIntelStartup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lazy => "lazy",
            Self::Eager => "eager",
        }
    }
}

/// One configured language server process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct LanguageServerConfig {
    pub name: String,
    #[serde(default)]
    pub languages: Vec<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub file_extensions: Vec<String>,
    #[serde(default)]
    pub initialization_options: Value,
    #[serde(default = "default_lsp_trust_required")]
    pub trust_required: bool,
    #[serde(default = "default_lsp_startup_timeout_ms")]
    pub startup_timeout_ms: u64,
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

/// Returns the standard per-user config directory for sigil.
///
/// # Errors
///
/// Returns an error when the current platform does not expose a usable home or config directory.
pub fn default_user_config_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = env::var_os("APPDATA") {
            return Ok(PathBuf::from(app_data).join("sigil"));
        }
        Err(anyhow::anyhow!(
            "missing APPDATA for sigil config directory"
        ))
    }

    #[cfg(target_os = "macos")]
    {
        let home = env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("missing HOME for sigil config directory"))?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("sigil"))
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg).join("sigil"));
        }

        let home = env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("missing HOME for sigil config directory"))?;
        Ok(PathBuf::from(home).join(".config").join("sigil"))
    }
}

/// Returns the standard per-user config file path for sigil.
///
/// # Errors
///
/// Returns an error when the current platform does not expose a usable config directory.
pub fn default_user_config_path() -> Result<PathBuf> {
    Ok(default_user_config_dir()?.join("sigil.toml"))
}

/// Resolves the config path that entrypoints should prefer on startup.
///
/// Explicit paths always win. Otherwise a local `sigil.toml` inside `cwd` wins over the
/// per-user config directory, so repository-local development keeps working naturally.
///
/// # Errors
///
/// Returns an error when the implicit per-user config directory cannot be determined.
pub fn preferred_config_path(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let local = cwd.join("sigil.toml");
    if local.exists() {
        return Ok(local);
    }

    default_user_config_path()
}

/// Resolves the effective workspace root for one launch.
///
/// Relative paths normally stay anchored to the config file location. The default `"."`
/// is treated specially so user-level configs can follow the directory where the user
/// launched sigil instead of pinning every session to the config folder.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,
    #[serde(default = "default_timeout_secs")]
    pub tool_timeout_secs: u64,
}

/// Planner/executor task mode configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskConfig {
    #[serde(default = "default_task_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub default_mode: TaskMode,
    #[serde(default)]
    pub planner: RoleModelConfig,
    #[serde(default)]
    pub executor: RoleModelConfig,
    #[serde(default)]
    pub subagent_read: RoleModelConfig,
    #[serde(default)]
    pub subagent_write: RoleModelConfig,
    #[serde(default = "default_max_plan_steps")]
    pub max_plan_steps: usize,
    #[serde(default = "default_max_replans")]
    pub max_replans: usize,
    #[serde(default = "default_max_child_sessions")]
    pub max_child_sessions: usize,
    /// Deprecated compatibility flag. Current readonly fan-out is controlled by
    /// `max_parallel_readonly`.
    #[serde(default)]
    pub allow_parallel_readonly_subagents: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel_readonly: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel_write: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_background_threads: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_fanout_per_turn: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_tokens_per_task: Option<u64>,
    #[serde(default = "default_allow_write_subagents")]
    pub allow_write_subagents: bool,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            enabled: default_task_enabled(),
            default_mode: TaskMode::default(),
            planner: RoleModelConfig::default(),
            executor: RoleModelConfig::default(),
            subagent_read: RoleModelConfig::default(),
            subagent_write: RoleModelConfig::default(),
            max_plan_steps: default_max_plan_steps(),
            max_replans: default_max_replans(),
            max_child_sessions: default_max_child_sessions(),
            allow_parallel_readonly_subagents: false,
            max_parallel_readonly: None,
            max_parallel_write: None,
            max_background_threads: None,
            max_spawn_fanout_per_turn: None,
            max_agent_tokens_per_task: None,
            allow_write_subagents: default_allow_write_subagents(),
        }
    }
}

impl TaskConfig {
    /// Returns the role-specific model and tool configuration.
    pub fn role_config(&self, role: AgentRole) -> &RoleModelConfig {
        match role {
            AgentRole::Planner => &self.planner,
            AgentRole::Executor => &self.executor,
            AgentRole::SubagentRead => &self.subagent_read,
            AgentRole::SubagentWrite => &self.subagent_write,
        }
    }
}

/// Default launch mode for user prompts.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    #[default]
    Chat,
    Plan,
}

impl TaskMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Plan => "plan",
        }
    }
}

/// Optional model/runtime overrides for one task role.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct RoleModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub tools: ToolAllowlistConfig,
}

/// Tool names and prefixes visible to one task role.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolAllowlistConfig {
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub prefixes: Vec<String>,
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

/// Skill discovery configuration shared by runtime entrypoints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SkillConfig {
    #[serde(default = "default_skill_enabled")]
    pub enabled: bool,
    #[serde(default = "default_skill_workspace_dir")]
    pub workspace_dir: String,
    #[serde(default = "default_skill_workspace_agents_dir")]
    pub workspace_agents_dir: String,
    #[serde(default = "default_skill_user_skills")]
    pub user_skills: bool,
    #[serde(default = "default_skill_user_agents")]
    pub user_agents: bool,
    #[serde(default = "default_skill_compatibility_sources")]
    pub compatibility_sources: Vec<String>,
}

impl Default for SkillConfig {
    fn default() -> Self {
        Self {
            enabled: default_skill_enabled(),
            workspace_dir: default_skill_workspace_dir(),
            workspace_agents_dir: default_skill_workspace_agents_dir(),
            user_skills: default_skill_user_skills(),
            user_agents: default_skill_user_agents(),
            compatibility_sources: default_skill_compatibility_sources(),
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
    /// Fallback model window used only when provider/model metadata cannot resolve one.
    #[serde(
        default,
        rename = "fallback_context_window_tokens",
        alias = "context_window_tokens",
        skip_serializing_if = "Option::is_none"
    )]
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_mcp_server_required")]
    pub required: bool,
    #[serde(default)]
    pub startup: McpServerStartup,
    #[serde(default)]
    pub trust: McpServerTrustPolicy,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            startup_timeout_secs: default_startup_timeout_secs(),
            required: default_mcp_server_required(),
            startup: McpServerStartup::default(),
            trust: McpServerTrustPolicy::default(),
        }
    }
}

/// MCP server startup strategy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStartup {
    #[default]
    Eager,
    Lazy,
}

impl McpServerStartup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eager => "eager",
            Self::Lazy => "lazy",
        }
    }
}

/// Trust class used to interpret MCP data egress and approval defaults.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpTrustClass {
    Official,
    #[default]
    SelfHosted,
    ThirdParty,
}

impl McpTrustClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::SelfHosted => "self_hosted",
            Self::ThirdParty => "third_party",
        }
    }
}

/// Per-server MCP trust policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpServerTrustPolicy {
    #[serde(default)]
    pub trust_class: McpTrustClass,
    #[serde(default)]
    pub approval_default: ApprovalMode,
    #[serde(default = "default_mcp_egress_logging")]
    pub egress_logging: bool,
    #[serde(default)]
    pub allow_secrets: bool,
    #[serde(default)]
    pub pin_version: bool,
    #[serde(default)]
    pub pinned: Option<McpServerPinnedIdentity>,
}

impl Default for McpServerTrustPolicy {
    fn default() -> Self {
        Self {
            trust_class: McpTrustClass::default(),
            approval_default: ApprovalMode::Ask,
            egress_logging: default_mcp_egress_logging(),
            allow_secrets: false,
            pin_version: false,
            pinned: None,
        }
    }
}

/// Expected MCP server identity used when `pin_version = true`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpServerPinnedIdentity {
    pub command_fingerprint: String,
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
}

fn default_workspace_root() -> String {
    ".".to_owned()
}

fn default_log_dir() -> String {
    ".sigil/sessions".to_owned()
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_task_enabled() -> bool {
    true
}

fn default_max_plan_steps() -> usize {
    12
}

fn default_max_replans() -> usize {
    2
}

fn default_max_child_sessions() -> usize {
    8
}

fn default_allow_write_subagents() -> bool {
    true
}

fn default_startup_timeout_secs() -> u64 {
    10
}

fn default_mcp_server_required() -> bool {
    true
}

fn default_mcp_egress_logging() -> bool {
    true
}

fn default_code_intel_timeout_ms() -> u64 {
    5_000
}

fn default_code_intel_max_results() -> usize {
    100
}

fn default_code_intel_max_payload_bytes() -> usize {
    64 * 1024
}

fn default_code_intel_discovery_enabled() -> bool {
    true
}

fn default_code_intel_discovery_report_missing() -> bool {
    true
}

fn default_terminal_mouse_capture() -> bool {
    true
}

fn default_terminal_osc52_clipboard() -> bool {
    true
}

fn default_terminal_scroll_sensitivity() -> u16 {
    DEFAULT_TERMINAL_SCROLL_SENSITIVITY
}

fn default_lsp_trust_required() -> bool {
    true
}

fn default_lsp_startup_timeout_ms() -> u64 {
    10_000
}

fn default_memory_enabled() -> bool {
    true
}

fn default_skill_enabled() -> bool {
    true
}

fn default_skill_workspace_dir() -> String {
    ".sigil/skills".to_owned()
}

fn default_skill_workspace_agents_dir() -> String {
    ".sigil/agents".to_owned()
}

fn default_skill_user_skills() -> bool {
    true
}

fn default_skill_user_agents() -> bool {
    true
}

fn default_skill_compatibility_sources() -> Vec<String> {
    vec!["claude".to_owned()]
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
