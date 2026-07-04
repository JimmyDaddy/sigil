use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{
    execution_backend::ExecutionConfig,
    mutation::MutationArtifactRetentionPolicy,
    permission::{ApprovalMode, PermissionConfig},
    provider::ReasoningEffort,
    task::AgentRole,
    verification::VerificationConfig,
};

pub const SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV: &str = "SIGIL_MODEL_REQUEST_TIMEOUT_SECS";
pub const SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV: &str = "SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS";
pub const SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV: &str = "SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS";

/// Root runtime configuration shared by the TUI, CLI, kernel, and adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RootConfig {
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub session: SessionConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub model_request: ModelRequestConfig,
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
    pub execution: ExecutionConfig,
    #[serde(default, skip_serializing_if = "VerificationConfig::is_empty")]
    pub verification: VerificationConfig,
    #[serde(default)]
    pub appearance: AppearanceConfig,
    #[serde(default)]
    pub task: TaskConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

/// Provider-neutral timeout settings for model requests.
///
/// This config controls how long Sigil waits for model transport phases. It is intentionally
/// separate from provider blocks so users do not need to configure the same timeout per provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ModelRequestConfig {
    #[serde(default = "default_model_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_model_request_stream_idle_timeout_secs")]
    pub stream_idle_timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_total_timeout_secs: Option<u64>,
}

impl Default for ModelRequestConfig {
    fn default() -> Self {
        Self {
            request_timeout_secs: default_model_request_timeout_secs(),
            stream_idle_timeout_secs: default_model_request_stream_idle_timeout_secs(),
            stream_total_timeout_secs: None,
        }
    }
}

impl ModelRequestConfig {
    /// Resolves this user config into runtime durations.
    ///
    /// # Errors
    ///
    /// Returns an error when any configured timeout is zero.
    pub fn to_timeouts(&self) -> Result<ModelRequestTimeouts> {
        if self.request_timeout_secs == 0 {
            anyhow::bail!("model_request.request_timeout_secs must be greater than 0");
        }
        if self.stream_idle_timeout_secs == 0 {
            anyhow::bail!("model_request.stream_idle_timeout_secs must be greater than 0");
        }
        if self.stream_total_timeout_secs == Some(0) {
            anyhow::bail!("model_request.stream_total_timeout_secs must be greater than 0");
        }
        Ok(ModelRequestTimeouts {
            request_timeout: Duration::from_secs(self.request_timeout_secs),
            stream_idle_timeout: Duration::from_secs(self.stream_idle_timeout_secs),
            stream_total_timeout: self.stream_total_timeout_secs.map(Duration::from_secs),
        })
    }
}

fn default_model_request_timeout_secs() -> u64 {
    120
}

fn default_model_request_stream_idle_timeout_secs() -> u64 {
    180
}

/// Runtime timeout policy applied to provider requests and streamed response bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRequestTimeouts {
    pub request_timeout: Duration,
    pub stream_idle_timeout: Duration,
    pub stream_total_timeout: Option<Duration>,
}

impl Default for ModelRequestTimeouts {
    fn default() -> Self {
        ModelRequestConfig::default()
            .to_timeouts()
            .expect("default model request timeout config is valid")
    }
}

/// Local code intelligence configuration.
///
/// This config is parsed by the shared root config so entrypoints preserve it while
/// `sigil-code-intel` owns the actual LSP lifecycle and language analysis behavior.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CodeIntelligenceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub server_startup: CodeIntelStartup,
    #[serde(default = "default_code_intel_timeout_ms")]
    pub default_timeout_ms: u64,
    #[serde(default = "default_code_intel_max_results")]
    pub max_results: usize,
    #[serde(default = "default_code_intel_max_payload_bytes")]
    pub max_payload_bytes: usize,
    #[serde(default = "default_code_intel_auto_discover")]
    pub auto_discover: bool,
    #[serde(default = "default_code_intel_report_missing")]
    pub report_missing: bool,
    #[serde(default)]
    pub servers: Vec<LanguageServerConfig>,
}

impl Default for CodeIntelligenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server_startup: CodeIntelStartup::default(),
            default_timeout_ms: default_code_intel_timeout_ms(),
            max_results: default_code_intel_max_results(),
            max_payload_bytes: default_code_intel_max_payload_bytes(),
            auto_discover: default_code_intel_auto_discover(),
            report_missing: default_code_intel_report_missing(),
            servers: Vec::new(),
        }
    }
}

/// Terminal integration controls for interactive entrypoints.
pub const DEFAULT_TERMINAL_SCROLL_SENSITIVITY: u16 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalConfig {
    #[serde(default = "default_terminal_keyboard_enhancement")]
    pub keyboard_enhancement: TerminalKeyboardEnhancement,
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
            keyboard_enhancement: default_terminal_keyboard_enhancement(),
            mouse_capture: default_terminal_mouse_capture(),
            osc52_clipboard: default_terminal_osc52_clipboard(),
            scroll_sensitivity: default_terminal_scroll_sensitivity(),
        }
    }
}

/// Policy for terminal keyboard enhancement in interactive entrypoints.
///
/// `auto` probes the current terminal before requesting enhanced key reporting,
/// `on` forces the request, and `off` keeps the baseline keyboard protocol.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKeyboardEnhancement {
    #[default]
    Auto,
    On,
    Off,
}

impl TerminalKeyboardEnhancement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
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
    #[serde(default)]
    pub usage_cost_currency: UsageCostCurrency,
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

/// User preference for displaying provider usage cost estimates.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageCostCurrency {
    #[default]
    Auto,
    Usd,
    Cny,
}

impl UsageCostCurrency {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Usd, Self::Cny];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Usd => "usd",
            Self::Cny => "cny",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Usd => "USD",
            Self::Cny => "CNY",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|currency| *currency == self)
            .expect("usage cost currency variants must be listed in ALL");
        Self::ALL[(index + 1) % Self::ALL.len()]
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
        let mut config: Self =
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
        config.apply_model_request_env_overrides()?;
        Ok(config)
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

    /// Applies provider-neutral model request timeout environment overrides.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured override is not a positive integer.
    pub fn apply_model_request_env_overrides(&mut self) -> Result<()> {
        if let Some(value) = read_positive_env_u64(SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV)? {
            self.model_request.request_timeout_secs = value;
        }
        if let Some(value) = read_positive_env_u64(SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV)? {
            self.model_request.stream_idle_timeout_secs = value;
        }
        if let Some(value) = read_positive_env_u64(SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV)? {
            self.model_request.stream_total_timeout_secs = Some(value);
        }
        Ok(())
    }
}

fn read_positive_env_u64(name: &str) -> Result<Option<u64>> {
    let Some(value) = env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("invalid {name}: expected positive integer"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be greater than 0");
    }
    Ok(Some(parsed))
}

/// Returns the visible per-user config directory for sigil.
///
/// # Errors
///
/// Returns an error when the current platform does not expose a usable home directory.
pub fn default_user_config_dir() -> Result<PathBuf> {
    Ok(user_home_dir()?.join(".sigil"))
}

fn user_home_dir() -> Result<PathBuf> {
    user_home_dir_from_env(
        current_config_platform(),
        env::var_os("HOME"),
        env::var_os("USERPROFILE"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum ConfigPlatform {
    Windows,
    Macos,
    Other,
}

fn current_config_platform() -> ConfigPlatform {
    current_config_platform_from_os(std::env::consts::OS)
}

fn current_config_platform_from_os(os: &str) -> ConfigPlatform {
    match os {
        "windows" => ConfigPlatform::Windows,
        "macos" => ConfigPlatform::Macos,
        _ => ConfigPlatform::Other,
    }
}

fn user_home_dir_from_env(
    platform: ConfigPlatform,
    home: Option<OsString>,
    userprofile: Option<OsString>,
) -> Result<PathBuf> {
    match platform {
        ConfigPlatform::Windows => userprofile
            .or(home)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("missing home directory for sigil config directory")),
        ConfigPlatform::Macos | ConfigPlatform::Other => home
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("missing HOME for sigil config directory")),
    }
}

/// Returns the visible per-user config file path for sigil.
///
/// # Errors
///
/// Returns an error when the current platform does not expose a usable config directory.
pub fn default_user_config_path() -> Result<PathBuf> {
    Ok(default_user_config_dir()?.join("sigil.toml"))
}

/// Resolves the config path that entrypoints should prefer on startup.
///
/// Explicit paths always win. Otherwise Sigil uses `~/.sigil/sigil.toml`.
///
/// Workspace-local `sigil.toml` files are intentionally not discovered implicitly because they
/// often contain personal provider, permission, and MCP settings that should not be committed.
///
/// # Errors
///
/// Returns an error when the implicit per-user config directory cannot be determined.
pub fn preferred_config_path(explicit: Option<&Path>, _cwd: &Path) -> Result<PathBuf> {
    let default_path = default_user_config_path()?;
    Ok(preferred_config_path_for_known_paths(
        explicit,
        default_path,
    ))
}

fn preferred_config_path_for_known_paths(
    explicit: Option<&Path>,
    default_path: PathBuf,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    default_path
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<String>,
}

/// User-local storage root configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct StorageConfig {
    #[serde(default)]
    pub state_root: StorageRoot,
    #[serde(default)]
    pub cache_root: StorageRoot,
    #[serde(default = "default_project_assets_root")]
    pub project_assets_root: String,
    #[serde(default)]
    pub mutation_artifact_retention: MutationArtifactRetentionConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            state_root: StorageRoot::Auto,
            cache_root: StorageRoot::Auto,
            project_assets_root: default_project_assets_root(),
            mutation_artifact_retention: MutationArtifactRetentionConfig::default(),
        }
    }
}

pub const DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_ARTIFACTS: usize = 10_000;
pub const DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MUTATION_ARTIFACT_RETENTION_EXPIRE_OLDER_THAN_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// User-visible retention policy for controlled mutation artifacts.
///
/// This config describes the policy used by explicit maintenance paths. It does not make normal
/// agent runs delete artifacts implicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactRetentionConfig {
    #[serde(default = "default_mutation_artifact_retention_max_artifacts")]
    pub max_artifacts: Option<usize>,
    #[serde(default = "default_mutation_artifact_retention_max_bytes")]
    pub max_bytes: Option<u64>,
    #[serde(default = "default_mutation_artifact_retention_expire_older_than_ms")]
    pub expire_older_than_ms: Option<u64>,
}

impl Default for MutationArtifactRetentionConfig {
    fn default() -> Self {
        Self {
            max_artifacts: default_mutation_artifact_retention_max_artifacts(),
            max_bytes: default_mutation_artifact_retention_max_bytes(),
            expire_older_than_ms: default_mutation_artifact_retention_expire_older_than_ms(),
        }
    }
}

impl MutationArtifactRetentionConfig {
    #[must_use]
    pub fn to_policy(&self) -> MutationArtifactRetentionPolicy {
        MutationArtifactRetentionPolicy {
            max_artifacts: self.max_artifacts,
            max_bytes: self.max_bytes,
            expire_older_than_ms: self.expire_older_than_ms,
        }
    }
}

fn default_mutation_artifact_retention_max_artifacts() -> Option<usize> {
    Some(DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_ARTIFACTS)
}

fn default_mutation_artifact_retention_max_bytes() -> Option<u64> {
    Some(DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_BYTES)
}

fn default_mutation_artifact_retention_expire_older_than_ms() -> Option<u64> {
    Some(DEFAULT_MUTATION_ARTIFACT_RETENTION_EXPIRE_OLDER_THAN_MS)
}

/// Storage root selector.
///
/// `auto` resolves to the platform user state/cache directory at runtime. Any other string is
/// treated as an explicit path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StorageRoot {
    #[default]
    Auto,
    Path(String),
}

impl Serialize for StorageRoot {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Path(path) => serializer.serialize_str(path),
        }
    }
}

impl<'de> Deserialize<'de> for StorageRoot {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(serde::de::Error::custom(
                "storage root path cannot be empty",
            ));
        }
        if trimmed.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        Ok(Self::Path(trimmed.to_owned()))
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
    #[serde(default = "default_max_subagents")]
    pub max_subagents: usize,
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
            max_subagents: default_max_subagents(),
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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

fn default_max_subagents() -> usize {
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

fn default_code_intel_auto_discover() -> bool {
    true
}

fn default_code_intel_report_missing() -> bool {
    true
}

fn default_terminal_mouse_capture() -> bool {
    false
}

fn default_terminal_keyboard_enhancement() -> TerminalKeyboardEnhancement {
    TerminalKeyboardEnhancement::Auto
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

fn default_project_assets_root() -> String {
    ".sigil".to_owned()
}

fn default_skill_user_skills() -> bool {
    true
}

fn default_skill_user_agents() -> bool {
    true
}

fn default_skill_compatibility_sources() -> Vec<String> {
    Vec::new()
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
