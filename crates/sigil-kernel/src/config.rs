use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use url::Url;

use crate::{
    execution_backend::ExecutionConfig,
    mutation::MutationArtifactRetentionPolicy,
    permission::{ApprovalMode, NetworkPolicy, PermissionConfig},
    process_environment::normalize_environment_variable_names,
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
    pub web: WebConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

/// Root Web V1 policy shared by every entrypoint and task role.
///
/// A missing `[web]` block intentionally resolves to the alpha defaults. Runtime callers may
/// only further restrict this policy with a non-persistent policy cap; they must not use it to
/// enable a route that this root policy disables.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default = "default_web_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub network_mode: NetworkPolicy,
    #[serde(default = "default_web_allow_http")]
    pub allow_http: bool,
    #[serde(default)]
    pub proxy_mode: WebProxyMode,
    #[serde(default)]
    pub redirect_policy: WebRedirectPolicy,
    #[serde(default)]
    pub search_route: WebSearchRoute,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_mcp: Option<WebSearchMcpConfig>,
    #[serde(default = "default_web_max_same_origin_redirects")]
    pub max_same_origin_redirects: u32,
    #[serde(default = "default_web_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_web_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_web_max_url_bytes")]
    pub max_url_bytes: usize,
    #[serde(default = "default_web_max_query_chars")]
    pub max_query_chars: usize,
    #[serde(default = "default_web_max_query_bytes")]
    pub max_query_bytes: usize,
    #[serde(default = "default_web_max_domains")]
    pub max_domains: usize,
    #[serde(default = "default_web_max_results")]
    pub max_results: u32,
    #[serde(default = "default_web_url_capabilities")]
    pub max_url_capabilities_per_session: usize,
    #[serde(default = "default_web_url_capability_ttl_secs")]
    pub url_capability_ttl_secs: u64,
    #[serde(default = "default_web_max_wire_response_bytes")]
    pub max_wire_response_bytes: u64,
    #[serde(default = "default_web_max_decoded_response_bytes")]
    pub max_decoded_response_bytes: u64,
    #[serde(default = "default_web_max_model_content_bytes")]
    pub max_model_content_bytes: u64,
    #[serde(default = "default_web_max_hosted_turn_buffer_bytes")]
    pub max_hosted_turn_buffer_bytes: u64,
    #[serde(default = "default_web_max_fetches_per_run")]
    pub max_fetches_per_run: u32,
    #[serde(default = "default_web_max_client_searches_per_run")]
    pub max_client_searches_per_run: u32,
    #[serde(default = "default_web_max_hosted_requests_per_run")]
    pub max_hosted_enabled_provider_requests_per_run: u32,
    #[serde(default = "default_web_provider_hosted_max_uses")]
    pub provider_hosted_max_uses_per_request: u32,
    #[serde(default = "default_web_max_network_attempts_per_run")]
    pub max_network_attempts_per_run: u32,
    #[serde(default = "default_web_max_total_wire_bytes_per_run")]
    pub max_total_wire_bytes_per_run: u64,
    #[serde(default = "default_web_max_total_decoded_bytes_per_run")]
    pub max_total_decoded_bytes_per_run: u64,
    #[serde(default = "default_web_max_total_model_bytes_per_run")]
    pub max_total_model_bytes_per_run: u64,
    #[serde(default = "default_web_max_concurrent_requests")]
    pub max_concurrent_requests: u32,
    #[serde(default = "default_web_per_host_rate_limit")]
    pub per_host_rate_limit_per_minute: u32,
    #[serde(default = "default_web_allowed_ports")]
    pub allowed_ports: Vec<u16>,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub allowed_private_hosts: Vec<String>,
    #[serde(default)]
    pub allowed_private_cidrs: Vec<String>,
    #[serde(default)]
    pub bundled_search: WebBundledSearchConfig,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: default_web_enabled(),
            network_mode: NetworkPolicy::Allow,
            allow_http: default_web_allow_http(),
            proxy_mode: WebProxyMode::default(),
            redirect_policy: WebRedirectPolicy::default(),
            search_route: WebSearchRoute::default(),
            search_mcp: None,
            max_same_origin_redirects: default_web_max_same_origin_redirects(),
            timeout_secs: default_web_timeout_secs(),
            connect_timeout_secs: default_web_connect_timeout_secs(),
            max_url_bytes: default_web_max_url_bytes(),
            max_query_chars: default_web_max_query_chars(),
            max_query_bytes: default_web_max_query_bytes(),
            max_domains: default_web_max_domains(),
            max_results: default_web_max_results(),
            max_url_capabilities_per_session: default_web_url_capabilities(),
            url_capability_ttl_secs: default_web_url_capability_ttl_secs(),
            max_wire_response_bytes: default_web_max_wire_response_bytes(),
            max_decoded_response_bytes: default_web_max_decoded_response_bytes(),
            max_model_content_bytes: default_web_max_model_content_bytes(),
            max_hosted_turn_buffer_bytes: default_web_max_hosted_turn_buffer_bytes(),
            max_fetches_per_run: default_web_max_fetches_per_run(),
            max_client_searches_per_run: default_web_max_client_searches_per_run(),
            max_hosted_enabled_provider_requests_per_run: default_web_max_hosted_requests_per_run(),
            provider_hosted_max_uses_per_request: default_web_provider_hosted_max_uses(),
            max_network_attempts_per_run: default_web_max_network_attempts_per_run(),
            max_total_wire_bytes_per_run: default_web_max_total_wire_bytes_per_run(),
            max_total_decoded_bytes_per_run: default_web_max_total_decoded_bytes_per_run(),
            max_total_model_bytes_per_run: default_web_max_total_model_bytes_per_run(),
            max_concurrent_requests: default_web_max_concurrent_requests(),
            per_host_rate_limit_per_minute: default_web_per_host_rate_limit(),
            allowed_ports: default_web_allowed_ports(),
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            allowed_private_hosts: Vec::new(),
            allowed_private_cidrs: Vec::new(),
            bundled_search: WebBundledSearchConfig::default(),
        }
    }
}

/// Proxy policy used by native Web V1 transports.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WebProxyMode {
    #[default]
    Environment,
    Direct,
}

/// Redirect policy used by native Web V1 transports.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebRedirectPolicy {
    #[default]
    SameOrigin,
    Deny,
}

/// Ordered Web search route preference selected once per run.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchRoute {
    #[default]
    Auto,
    ProviderHosted,
    Mcp,
    Bundled,
    Disabled,
}

/// Exact user-configured MCP binding eligible for the stable `websearch` product surface.
///
/// Request templates, result paths, and field aliases are intentionally not configurable in V1.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WebSearchMcpConfig {
    pub server: String,
    pub tool: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct WebSearchMcpConfigWire {
    server: String,
    tool: String,
}

impl<'de> Deserialize<'de> for WebSearchMcpConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WebSearchMcpConfigWire::deserialize(deserializer)?;
        if wire.server.trim().is_empty() || wire.server.trim() != wire.server {
            return Err(serde::de::Error::custom(
                "web.search_mcp.server must be exact and non-empty",
            ));
        }
        if wire.tool.trim().is_empty() || wire.tool.trim() != wire.tool {
            return Err(serde::de::Error::custom(
                "web.search_mcp.tool must be exact and non-empty",
            ));
        }
        Ok(Self {
            server: wire.server,
            tool: wire.tool,
        })
    }
}

/// Non-persistent restrictions that a parent run may impose on `WebConfig`.
///
/// Every field is a cap, never an override: callers use [`WebConfig::meet_policy_cap`] to
/// calculate the effective policy, so child or runtime state cannot reopen a disabled route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebPolicyCap {
    pub enabled: Option<bool>,
    pub bundled_search_enabled: Option<bool>,
    pub network_mode: Option<NetworkPolicy>,
    pub allowed_routes: Option<BTreeSet<WebSearchRoute>>,
    pub allowed_domains: Option<BTreeSet<String>>,
    pub blocked_domains: BTreeSet<String>,
    pub max_query_chars: Option<usize>,
    pub max_query_bytes: Option<usize>,
    pub max_client_searches_per_run: Option<u32>,
    pub max_hosted_enabled_provider_requests_per_run: Option<u32>,
    pub max_network_attempts_per_run: Option<u32>,
    pub max_concurrent_requests: Option<u32>,
}

/// Resolved Web policy after applying a non-persistent [`WebPolicyCap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveWebPolicy {
    pub enabled: bool,
    pub bundled_search_enabled: bool,
    pub network_mode: NetworkPolicy,
    pub allowed_routes: BTreeSet<WebSearchRoute>,
    pub allowed_domains: BTreeSet<String>,
    pub blocked_domains: BTreeSet<String>,
    pub max_query_chars: usize,
    pub max_query_bytes: usize,
    pub max_client_searches_per_run: u32,
    pub max_hosted_enabled_provider_requests_per_run: u32,
    pub max_network_attempts_per_run: u32,
    pub max_concurrent_requests: u32,
}

impl WebConfig {
    /// Applies only tightening restrictions and returns the effective per-run policy.
    #[must_use]
    pub fn meet_policy_cap(&self, cap: &WebPolicyCap) -> EffectiveWebPolicy {
        let base_routes = web_search_route_candidates(self.search_route);
        let allowed_routes = cap
            .allowed_routes
            .as_ref()
            .map_or(base_routes.clone(), |routes| {
                base_routes.intersection(routes).copied().collect()
            });
        let base_domains = self
            .allowed_domains
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let allowed_domains =
            cap.allowed_domains
                .as_ref()
                .map_or(base_domains.clone(), |domains| {
                    if base_domains.is_empty() {
                        domains.clone()
                    } else {
                        base_domains.intersection(domains).cloned().collect()
                    }
                });
        let mut blocked_domains = self
            .blocked_domains
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        blocked_domains.extend(cap.blocked_domains.iter().cloned());
        EffectiveWebPolicy {
            enabled: self.enabled && cap.enabled.unwrap_or(true),
            bundled_search_enabled: self.bundled_search.enabled
                && cap.bundled_search_enabled.unwrap_or(true),
            network_mode: stricter_network_policy(self.network_mode, cap.network_mode),
            allowed_routes,
            allowed_domains,
            blocked_domains,
            max_query_chars: min_cap(self.max_query_chars, cap.max_query_chars),
            max_query_bytes: min_cap(self.max_query_bytes, cap.max_query_bytes),
            max_client_searches_per_run: min_cap(
                self.max_client_searches_per_run,
                cap.max_client_searches_per_run,
            ),
            max_hosted_enabled_provider_requests_per_run: min_cap(
                self.max_hosted_enabled_provider_requests_per_run,
                cap.max_hosted_enabled_provider_requests_per_run,
            ),
            max_network_attempts_per_run: min_cap(
                self.max_network_attempts_per_run,
                cap.max_network_attempts_per_run,
            ),
            max_concurrent_requests: min_cap(
                self.max_concurrent_requests,
                cap.max_concurrent_requests,
            ),
        }
    }
}

fn web_search_route_candidates(route: WebSearchRoute) -> BTreeSet<WebSearchRoute> {
    match route {
        WebSearchRoute::Auto => [
            WebSearchRoute::ProviderHosted,
            WebSearchRoute::Mcp,
            WebSearchRoute::Bundled,
        ]
        .into_iter()
        .collect(),
        WebSearchRoute::Disabled => BTreeSet::new(),
        route => [route].into_iter().collect(),
    }
}

fn stricter_network_policy(base: NetworkPolicy, cap: Option<NetworkPolicy>) -> NetworkPolicy {
    match cap {
        Some(NetworkPolicy::Deny) | None if base == NetworkPolicy::Deny => NetworkPolicy::Deny,
        Some(NetworkPolicy::Deny) => NetworkPolicy::Deny,
        Some(NetworkPolicy::Ask) if base == NetworkPolicy::Allow => NetworkPolicy::Ask,
        _ => base,
    }
}

fn min_cap<T: Ord + Copy>(base: T, cap: Option<T>) -> T {
    cap.map_or(base, |value| base.min(value))
}

/// Controls the runtime-private bundled stable search profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct WebBundledSearchConfig {
    #[serde(default = "default_web_bundled_search_enabled")]
    pub enabled: bool,
}

impl Default for WebBundledSearchConfig {
    fn default() -> Self {
        Self {
            enabled: default_web_bundled_search_enabled(),
        }
    }
}

const fn default_web_enabled() -> bool {
    true
}
const fn default_web_allow_http() -> bool {
    true
}
const fn default_web_max_same_origin_redirects() -> u32 {
    5
}
const fn default_web_timeout_secs() -> u64 {
    15
}
const fn default_web_connect_timeout_secs() -> u64 {
    5
}
const fn default_web_max_url_bytes() -> usize {
    2_048
}
const fn default_web_max_query_chars() -> usize {
    512
}
const fn default_web_max_query_bytes() -> usize {
    2_048
}
const fn default_web_max_domains() -> usize {
    10
}
const fn default_web_max_results() -> u32 {
    8
}
const fn default_web_url_capabilities() -> usize {
    256
}
const fn default_web_url_capability_ttl_secs() -> u64 {
    3_600
}
const fn default_web_max_wire_response_bytes() -> u64 {
    2_097_152
}
const fn default_web_max_decoded_response_bytes() -> u64 {
    1_048_576
}
const fn default_web_max_model_content_bytes() -> u64 {
    24_000
}
const fn default_web_max_hosted_turn_buffer_bytes() -> u64 {
    262_144
}
const fn default_web_max_fetches_per_run() -> u32 {
    5
}
const fn default_web_max_client_searches_per_run() -> u32 {
    3
}
const fn default_web_max_hosted_requests_per_run() -> u32 {
    4
}
const fn default_web_provider_hosted_max_uses() -> u32 {
    3
}
const fn default_web_max_network_attempts_per_run() -> u32 {
    12
}
const fn default_web_max_total_wire_bytes_per_run() -> u64 {
    8_388_608
}
const fn default_web_max_total_decoded_bytes_per_run() -> u64 {
    4_194_304
}
const fn default_web_max_total_model_bytes_per_run() -> u64 {
    98_304
}
const fn default_web_max_concurrent_requests() -> u32 {
    2
}
const fn default_web_per_host_rate_limit() -> u32 {
    10
}
fn default_web_allowed_ports() -> Vec<u16> {
    vec![80, 443]
}
const fn default_web_bundled_search_enabled() -> bool {
    true
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
        Self {
            request_timeout: Duration::from_secs(default_model_request_timeout_secs()),
            stream_idle_timeout: Duration::from_secs(
                default_model_request_stream_idle_timeout_secs(),
            ),
            stream_total_timeout: None,
        }
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
pub const DEFAULT_TERMINAL_NOTIFICATION_MINIMUM_RUN_DURATION_MS: u64 = 10_000;
pub const MIN_TERMINAL_NOTIFICATION_RUN_DURATION_MS: u64 = 1_000;
pub const MAX_TERMINAL_NOTIFICATION_RUN_DURATION_MS: u64 = 3_600_000;

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
    #[serde(default)]
    pub notifications: TerminalNotificationConfig,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            keyboard_enhancement: default_terminal_keyboard_enhancement(),
            mouse_capture: default_terminal_mouse_capture(),
            osc52_clipboard: default_terminal_osc52_clipboard(),
            scroll_sensitivity: default_terminal_scroll_sensitivity(),
            notifications: TerminalNotificationConfig::default(),
        }
    }
}

/// Privacy-bounded terminal attention notification settings.
///
/// Notification payloads are selected by the interactive entrypoint from a fixed signal set;
/// this config only controls whether and how those ephemeral terminal bytes may be emitted.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalNotificationConfig {
    pub enabled: bool,
    pub method: TerminalNotificationMethod,
    pub minimum_run_duration_ms: u64,
}

impl Default for TerminalNotificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            method: TerminalNotificationMethod::Auto,
            minimum_run_duration_ms: default_terminal_notification_minimum_run_duration_ms(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct TerminalNotificationConfigWire {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    method: TerminalNotificationMethod,
    #[serde(default = "default_terminal_notification_minimum_run_duration_ms")]
    minimum_run_duration_ms: u64,
}

impl<'de> Deserialize<'de> for TerminalNotificationConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TerminalNotificationConfigWire::deserialize(deserializer)?;
        let config = Self {
            enabled: wire.enabled,
            method: wire.method,
            minimum_run_duration_ms: wire.minimum_run_duration_ms,
        };
        config
            .validate()
            .map_err(<D::Error as serde::de::Error>::custom)?;
        Ok(config)
    }
}

impl TerminalNotificationConfig {
    /// Validates the bounded duration used to decide whether a completed run is long enough to
    /// notify.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !(MIN_TERMINAL_NOTIFICATION_RUN_DURATION_MS..=MAX_TERMINAL_NOTIFICATION_RUN_DURATION_MS)
            .contains(&self.minimum_run_duration_ms)
        {
            return Err(format!(
                "terminal.notifications.minimum_run_duration_ms must be between {MIN_TERMINAL_NOTIFICATION_RUN_DURATION_MS} and {MAX_TERMINAL_NOTIFICATION_RUN_DURATION_MS}"
            ));
        }
        Ok(())
    }
}

/// Terminal protocol selected for ephemeral attention notifications.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalNotificationMethod {
    #[default]
    Auto,
    Osc9,
    Osc777,
    Bell,
}

impl TerminalNotificationMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Osc9 => "osc9",
            Self::Osc777 => "osc777",
            Self::Bell => "bell",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Osc9,
            Self::Osc9 => Self::Osc777,
            Self::Osc777 => Self::Bell,
            Self::Bell => Self::Auto,
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
/// Appearance choices are user-interface preferences only. They must not affect session history,
/// provider-visible request material, tool approval audit entries, or cache-stable state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AppearanceConfig {
    #[serde(default)]
    pub theme: ThemeId,
    #[serde(default)]
    pub syntax_theme: SyntaxThemeId,
    #[serde(default)]
    pub usage_cost_currency: UsageCostCurrency,
    #[serde(default = "default_appearance_info_rail")]
    pub info_rail: bool,
    #[serde(default, skip_serializing_if = "ThemeColorOverrides::is_empty")]
    pub colors: ThemeColorOverrides,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            theme: ThemeId::default(),
            syntax_theme: SyntaxThemeId::default(),
            usage_cost_currency: UsageCostCurrency::default(),
            info_rail: default_appearance_info_rail(),
            colors: ThemeColorOverrides::default(),
        }
    }
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
        match self {
            Self::Auto => Self::Usd,
            Self::Usd => Self::Cny,
            Self::Cny => Self::Auto,
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
    #[serde(default)]
    pub retention: SessionRetentionConfig,
}

pub const DEFAULT_SESSION_RETENTION_MAX_SESSIONS: usize = 500;
pub const DEFAULT_SESSION_RETENTION_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_SESSION_RETENTION_EXPIRE_OLDER_THAN_MS: u64 = 180 * 24 * 60 * 60 * 1000;

/// Policy used only by explicit local session maintenance actions.
///
/// Ordinary run, resume, startup, and serve paths do not apply this policy implicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionRetentionConfig {
    #[serde(default = "default_session_retention_max_sessions")]
    pub max_sessions: Option<usize>,
    #[serde(default = "default_session_retention_max_bytes")]
    pub max_bytes: Option<u64>,
    #[serde(default = "default_session_retention_expire_older_than_ms")]
    pub expire_older_than_ms: Option<u64>,
}

impl Default for SessionRetentionConfig {
    fn default() -> Self {
        Self {
            max_sessions: default_session_retention_max_sessions(),
            max_bytes: default_session_retention_max_bytes(),
            expire_older_than_ms: default_session_retention_expire_older_than_ms(),
        }
    }
}

fn default_session_retention_max_sessions() -> Option<usize> {
    Some(DEFAULT_SESSION_RETENTION_MAX_SESSIONS)
}

fn default_session_retention_max_bytes() -> Option<u64> {
    Some(DEFAULT_SESSION_RETENTION_MAX_BYTES)
}

fn default_session_retention_expire_older_than_ms() -> Option<u64> {
    Some(DEFAULT_SESSION_RETENTION_EXPIRE_OLDER_THAN_MS)
}

/// User-local storage root configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default)]
    pub state_root: StorageRoot,
    #[serde(default)]
    pub cache_root: StorageRoot,
    #[serde(default)]
    pub mutation_artifact_retention: MutationArtifactRetentionConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            state_root: StorageRoot::Auto,
            cache_root: StorageRoot::Auto,
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
    #[serde(default)]
    pub multi_agent_mode: MultiAgentMode,
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
            multi_agent_mode: MultiAgentMode::default(),
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

/// Model delegation policy for agent tools.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MultiAgentMode {
    None,
    #[default]
    #[serde(alias = "explicitRequestOnly")]
    ExplicitRequestOnly,
    Proactive,
}

impl MultiAgentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ExplicitRequestOnly => "explicit_request_only",
            Self::Proactive => "proactive",
        }
    }
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SkillConfig {
    #[serde(default = "default_skill_enabled")]
    pub enabled: bool,
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

/// Validated root MCP server configuration with an explicit transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpServerTransportConfig,
    pub startup_timeout_secs: u64,
    pub required: bool,
    pub startup: McpServerStartup,
    pub trust: McpServerTrustPolicy,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpServerTransportConfig::Stdio {
                command: String::new(),
                args: Vec::new(),
                inherit_env: Vec::new(),
            },
            startup_timeout_secs: default_startup_timeout_secs(),
            required: default_mcp_server_required(),
            startup: McpServerStartup::default(),
            trust: McpServerTrustPolicy::default(),
        }
    }
}

impl McpServerConfig {
    #[must_use]
    pub fn stdio(&self) -> Option<(&str, &[String], &[String])> {
        match &self.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                inherit_env,
            } => Some((command, args, inherit_env)),
            McpServerTransportConfig::StreamableHttp(_) => None,
        }
    }

    #[must_use]
    pub fn streamable_http(&self) -> Option<&McpStreamableHttpConfig> {
        match &self.transport {
            McpServerTransportConfig::StreamableHttp(config) => Some(config),
            McpServerTransportConfig::Stdio { .. } => None,
        }
    }

    #[must_use]
    pub fn transport_name(&self) -> &'static str {
        match self.transport {
            McpServerTransportConfig::Stdio { .. } => "stdio",
            McpServerTransportConfig::StreamableHttp(_) => "streamable_http",
        }
    }
}

/// Transport-specific MCP configuration kept separate from shared lifecycle and trust fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerTransportConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        inherit_env: Vec<String>,
    },
    StreamableHttp(McpStreamableHttpConfig),
}

/// User-root Streamable HTTP transport configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpStreamableHttpConfig {
    pub url: String,
    pub http_headers: BTreeMap<String, String>,
    pub env_http_headers: BTreeMap<String, String>,
    pub bearer_token_env_var: Option<String>,
    pub oauth: Option<McpOAuthConfig>,
    pub client_capabilities: BTreeSet<McpRemoteClientCapability>,
}

/// Public OAuth client intent for one user-root Streamable HTTP MCP server.
///
/// Secrets, discovered registration metadata and tokens are deliberately excluded. They are
/// runtime-owned and may only be persisted in the native system credential store.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct McpOAuthConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// Public, bounded MCP client capabilities supported for remote root servers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum McpRemoteClientCapability {
    Roots,
    #[serde(rename = "elicitation")]
    ElicitationForm,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
enum McpServerConfigWire {
    Stdio {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "deserialize_inherit_env",
            serialize_with = "serialize_inherit_env"
        )]
        inherit_env: Vec<String>,
        #[serde(default = "default_startup_timeout_secs")]
        startup_timeout_secs: u64,
        #[serde(default = "default_mcp_server_required")]
        required: bool,
        #[serde(default)]
        startup: McpServerStartup,
        #[serde(default)]
        trust: McpServerTrustPolicy,
    },
    StreamableHttp {
        name: String,
        url: String,
        #[serde(default)]
        http_headers: BTreeMap<String, String>,
        #[serde(default)]
        env_http_headers: BTreeMap<String, String>,
        #[serde(default)]
        bearer_token_env_var: Option<String>,
        #[serde(default)]
        oauth: Option<McpOAuthConfig>,
        #[serde(default)]
        client_capabilities: Vec<McpRemoteClientCapability>,
        #[serde(default = "default_startup_timeout_secs")]
        startup_timeout_secs: u64,
        #[serde(default = "default_mcp_server_required")]
        required: bool,
        #[serde(default)]
        startup: McpServerStartup,
        #[serde(default)]
        trust: McpServerTrustPolicy,
    },
}

impl Serialize for McpServerConfig {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_mcp_server_config(self).map_err(serde::ser::Error::custom)?;
        let wire = match &self.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                inherit_env,
            } => McpServerConfigWire::Stdio {
                name: self.name.clone(),
                command: command.clone(),
                args: args.clone(),
                inherit_env: inherit_env.clone(),
                startup_timeout_secs: self.startup_timeout_secs,
                required: self.required,
                startup: self.startup,
                trust: self.trust.clone(),
            },
            McpServerTransportConfig::StreamableHttp(config) => {
                McpServerConfigWire::StreamableHttp {
                    name: self.name.clone(),
                    url: config.url.clone(),
                    http_headers: config.http_headers.clone(),
                    env_http_headers: config.env_http_headers.clone(),
                    bearer_token_env_var: config.bearer_token_env_var.clone(),
                    oauth: config.oauth.clone(),
                    client_capabilities: config.client_capabilities.iter().copied().collect(),
                    startup_timeout_secs: self.startup_timeout_secs,
                    required: self.required,
                    startup: self.startup,
                    trust: self.trust.clone(),
                }
            }
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for McpServerConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match McpServerConfigWire::deserialize(deserializer)? {
            McpServerConfigWire::Stdio {
                name,
                command,
                args,
                inherit_env,
                startup_timeout_secs,
                required,
                startup,
                trust,
            } => {
                let config = Self {
                    name,
                    transport: McpServerTransportConfig::Stdio {
                        command,
                        args,
                        inherit_env,
                    },
                    startup_timeout_secs,
                    required,
                    startup,
                    trust,
                };
                validate_mcp_server_config(&config).map_err(serde::de::Error::custom)?;
                Ok(config)
            }
            McpServerConfigWire::StreamableHttp {
                name,
                url,
                http_headers,
                env_http_headers,
                bearer_token_env_var,
                oauth,
                client_capabilities,
                startup_timeout_secs,
                required,
                startup,
                trust,
            } => {
                let capabilities = client_capabilities.iter().copied().collect::<BTreeSet<_>>();
                if capabilities.len() != client_capabilities.len() {
                    return Err(serde::de::Error::custom(
                        "duplicate streamable_http client_capabilities value",
                    ));
                }
                let config = Self {
                    name,
                    transport: McpServerTransportConfig::StreamableHttp(McpStreamableHttpConfig {
                        url,
                        http_headers,
                        env_http_headers,
                        bearer_token_env_var,
                        oauth,
                        client_capabilities: capabilities,
                    }),
                    startup_timeout_secs,
                    required,
                    startup,
                    trust,
                };
                validate_mcp_server_config(&config).map_err(serde::de::Error::custom)?;
                Ok(config)
            }
        }
    }
}

fn validate_mcp_server_config(config: &McpServerConfig) -> Result<()> {
    let name = config.name.trim();
    anyhow::ensure!(!name.is_empty(), "MCP server name cannot be empty");
    anyhow::ensure!(
        name == config.name,
        "MCP server name cannot contain leading or trailing whitespace"
    );
    anyhow::ensure!(
        !name.starts_with("builtin:"),
        "MCP server name uses reserved builtin: namespace"
    );
    anyhow::ensure!(
        config.startup_timeout_secs > 0,
        "MCP startup_timeout_secs must be greater than 0"
    );
    validate_mcp_pin_config(&config.trust)?;
    match &config.transport {
        McpServerTransportConfig::Stdio {
            command,
            inherit_env,
            ..
        } => {
            anyhow::ensure!(
                !command.trim().is_empty(),
                "stdio MCP command cannot be empty"
            );
            let normalized = normalize_environment_variable_names(inherit_env)?;
            anyhow::ensure!(
                &normalized == inherit_env,
                "stdio MCP inherit_env must be sorted and deduplicated"
            );
        }
        McpServerTransportConfig::StreamableHttp(remote) => {
            validate_remote_mcp_config(remote)?;
        }
    }
    Ok(())
}

fn validate_mcp_pin_config(trust: &McpServerTrustPolicy) -> Result<()> {
    match (trust.pin_version, trust.pinned.as_ref()) {
        (false, None) => Ok(()),
        (false, Some(_)) => anyhow::bail!("MCP pinned identity requires pin_version = true"),
        (true, None) => anyhow::bail!("MCP pin_version = true requires a pinned identity"),
        (true, Some(pin)) => {
            anyhow::ensure!(
                is_sha256_fingerprint(&pin.transport_fingerprint),
                "MCP pinned transport_fingerprint must be sha256: followed by 64 hex characters"
            );
            anyhow::ensure!(
                !pin.protocol_version.trim().is_empty(),
                "MCP pinned protocol_version cannot be empty"
            );
            anyhow::ensure!(
                !pin.server_name.trim().is_empty(),
                "MCP pinned server_name cannot be empty"
            );
            anyhow::ensure!(
                !pin.server_version.trim().is_empty(),
                "MCP pinned server_version cannot be empty"
            );
            Ok(())
        }
    }
}

fn validate_remote_mcp_config(config: &McpStreamableHttpConfig) -> Result<()> {
    let endpoint = Url::parse(&config.url).context("streamable_http MCP url is invalid")?;
    anyhow::ensure!(
        matches!(endpoint.scheme(), "https" | "http"),
        "streamable_http MCP url must use https or http"
    );
    anyhow::ensure!(
        endpoint.host_str().is_some(),
        "streamable_http MCP url must include a host"
    );
    anyhow::ensure!(
        endpoint.username().is_empty() && endpoint.password().is_none(),
        "streamable_http MCP url cannot contain userinfo"
    );
    anyhow::ensure!(
        endpoint.fragment().is_none(),
        "streamable_http MCP url cannot contain a fragment"
    );

    let header_count = config.http_headers.len()
        + config.env_http_headers.len()
        + usize::from(config.bearer_token_env_var.is_some());
    anyhow::ensure!(
        header_count <= 32,
        "streamable_http MCP custom headers exceed the limit of 32"
    );
    let mut names = BTreeSet::new();
    let mut total_bytes = 0usize;
    for (name, value) in &config.http_headers {
        validate_remote_header_name(name)?;
        anyhow::ensure!(
            !is_sensitive_header_name(name),
            "streamable_http MCP sensitive header {name} must reference an environment variable"
        );
        validate_remote_literal_header_value(value)?;
        register_remote_header_name(&mut names, name)?;
        total_bytes = total_bytes
            .saturating_add(name.len())
            .saturating_add(value.len());
    }
    for (name, environment_name) in &config.env_http_headers {
        validate_remote_header_name(name)?;
        validate_environment_variable_name(environment_name)?;
        register_remote_header_name(&mut names, name)?;
        total_bytes = total_bytes
            .saturating_add(name.len())
            .saturating_add(environment_name.len());
    }
    if let Some(environment_name) = &config.bearer_token_env_var {
        validate_environment_variable_name(environment_name)?;
        register_remote_header_name(&mut names, "authorization")?;
        total_bytes = total_bytes
            .saturating_add("authorization".len())
            .saturating_add(environment_name.len());
    }
    if let Some(oauth) = config.oauth.as_ref() {
        anyhow::ensure!(
            endpoint.scheme() == "https",
            "streamable_http MCP OAuth requires https / MCP OAuth 必须使用 https"
        );
        anyhow::ensure!(
            config.bearer_token_env_var.is_none()
                && !config
                    .env_http_headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("authorization")),
            "streamable_http MCP OAuth cannot be combined with a static Authorization or bearer credential / MCP OAuth 不能与静态 Authorization 或 bearer 凭据同时配置"
        );
        validate_remote_mcp_oauth_config(oauth)?;
    }
    anyhow::ensure!(
        total_bytes <= 32 * 1024,
        "streamable_http MCP custom header metadata exceeds 32 KiB"
    );
    if endpoint.scheme() == "http" {
        anyhow::ensure!(
            config.env_http_headers.is_empty() && config.bearer_token_env_var.is_none(),
            "streamable_http MCP credentials require https"
        );
    }
    Ok(())
}

fn validate_remote_mcp_oauth_config(config: &McpOAuthConfig) -> Result<()> {
    if let Some(client_id) = config.client_id.as_deref() {
        anyhow::ensure!(
            !client_id.is_empty()
                && client_id.len() <= 1024
                && !client_id.chars().any(char::is_control)
                && !client_id.chars().any(char::is_whitespace),
            "streamable_http MCP OAuth client_id must contain 1..=1024 non-whitespace bytes / MCP OAuth client_id 必须为 1..=1024 字节且不含空白字符"
        );
    }
    anyhow::ensure!(
        config.scopes.len() <= 32,
        "streamable_http MCP OAuth scopes exceed the limit of 32 / MCP OAuth scopes 不能超过 32 项"
    );
    let mut unique = BTreeSet::new();
    let mut total_bytes = 0usize;
    for scope in &config.scopes {
        total_bytes = total_bytes.saturating_add(scope.len());
        anyhow::ensure!(
            !scope.is_empty()
                && scope.len() <= 256
                && scope.bytes().all(|byte| {
                    byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
                }),
            "streamable_http MCP OAuth scope is empty, invalid, or exceeds 256 bytes / MCP OAuth scope 不能为空、格式无效或超过 256 字节"
        );
        anyhow::ensure!(
            unique.insert(scope),
            "streamable_http MCP OAuth scopes contain a duplicate value / MCP OAuth scopes 包含重复项"
        );
    }
    anyhow::ensure!(
        total_bytes <= 4 * 1024,
        "streamable_http MCP OAuth scope metadata exceeds 4 KiB / MCP OAuth scope 元数据超过 4 KiB"
    );
    Ok(())
}

fn validate_remote_header_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 128,
        "streamable_http MCP header name must contain 1..=128 bytes"
    );
    anyhow::ensure!(
        name.bytes().all(is_http_token_byte),
        "streamable_http MCP header name is invalid"
    );
    anyhow::ensure!(
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "accept"
                | "connection"
                | "content-length"
                | "content-type"
                | "host"
                | "mcp-protocol-version"
                | "mcp-session-id"
        ),
        "streamable_http MCP header {name} is transport-owned"
    );
    Ok(())
}

fn validate_remote_literal_header_value(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() <= 8 * 1024,
        "streamable_http MCP literal header value exceeds 8 KiB"
    );
    anyhow::ensure!(
        !value
            .bytes()
            .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0),
        "streamable_http MCP literal header value contains a control character"
    );
    Ok(())
}

fn register_remote_header_name(names: &mut BTreeSet<String>, name: &str) -> Result<()> {
    anyhow::ensure!(
        names.insert(name.to_ascii_lowercase()),
        "streamable_http MCP header {name} is configured more than once"
    );
    Ok(())
}

fn validate_environment_variable_name(name: &str) -> Result<()> {
    let normalized = normalize_environment_variable_names(&[name.to_owned()])?;
    anyhow::ensure!(
        normalized.first().is_some_and(|value| value == name),
        "environment variable name must match [A-Za-z_][A-Za-z0-9_]*"
    );
    Ok(())
}

fn is_sensitive_header_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "authorization"
        || name == "proxy-authorization"
        || name == "cookie"
        || name == "set-cookie"
        || name.contains("api-key")
        || name.contains("apikey")
        || name.contains("token")
        || name.contains("secret")
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn is_sha256_fingerprint(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn deserialize_inherit_env<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let names = Vec::<String>::deserialize(deserializer)?;
    normalize_environment_variable_names(&names).map_err(serde::de::Error::custom)
}

fn serialize_inherit_env<S>(names: &[String], serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let names = normalize_environment_variable_names(names).map_err(serde::ser::Error::custom)?;
    names.serialize(serializer)
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
    pub transport_fingerprint: String,
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
    true
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

fn default_terminal_notification_minimum_run_duration_ms() -> u64 {
    DEFAULT_TERMINAL_NOTIFICATION_MINIMUM_RUN_DURATION_MS
}

fn default_appearance_info_rail() -> bool {
    true
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
