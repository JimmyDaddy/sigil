use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fmt, fs,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(not(unix))]
use std::fs::OpenOptions;

use anyhow::{Context, Result};
use fs2::FileExt as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use url::Url;

use crate::{
    execution_backend::ExecutionConfig,
    model_route::{ConnectionId, ModelRef},
    mutation::MutationArtifactRetentionPolicy,
    permission::{ApprovalMode, NetworkPolicy, PermissionConfig},
    process_environment::normalize_environment_variable_names,
    provider::ReasoningEffort,
    session::{
        DEFAULT_PROVIDER_TURN_INITIAL_DELAY_MS, DEFAULT_PROVIDER_TURN_JITTER_RATIO_MILLIONTHS,
        DEFAULT_PROVIDER_TURN_MAX_CUMULATIVE_DELAY_MS, DEFAULT_PROVIDER_TURN_MAX_DELAY_MS,
        DEFAULT_PROVIDER_TURN_MAX_PARTIAL_OUTPUT_RETRIES,
        DEFAULT_PROVIDER_TURN_MAX_TRANSPORT_RETRIES, ProviderTurnRecoveryPolicyV1,
    },
    task::AgentRole,
    verification::VerificationConfig,
};

pub const CONFIG_VERSION_V2: u32 = 2;
pub const SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV: &str = "SIGIL_MODEL_REQUEST_TIMEOUT_SECS";
pub const SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV: &str = "SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS";
pub const SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV: &str = "SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS";

/// Root runtime configuration shared by the TUI, CLI, kernel, and adapters.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RootConfig {
    pub config_version: u32,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, Value>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

impl fmt::Debug for RootConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RootConfig")
            .field("config_version", &self.config_version)
            .field("workspace", &self.workspace)
            .field("storage", &self.storage)
            .field("session", &self.session)
            .field("agent", &self.agent)
            .field("model_request", &self.model_request)
            .field("permission", &self.permission)
            .field("memory", &self.memory)
            .field("skills", &self.skills)
            .field("compaction", &self.compaction)
            .field("code_intelligence", &self.code_intelligence)
            .field("terminal", &self.terminal)
            .field("execution", &self.execution)
            .field("verification", &self.verification)
            .field("appearance", &self.appearance)
            .field("task", &self.task)
            .field("web", &self.web)
            .field(
                "connections",
                &format_args!("[{} redacted]", self.connections.len()),
            )
            .field("mcp_server_count", &self.mcp_servers.len())
            .finish()
    }
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
    pub max_hosted_enabled_provider_requests_per_run: Option<u32>,
    #[serde(default = "default_web_provider_hosted_max_uses")]
    pub provider_hosted_max_uses_per_request: Option<u32>,
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
    pub max_hosted_enabled_provider_requests_per_run: Option<u32>,
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
            max_hosted_enabled_provider_requests_per_run: min_optional_cap(
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

/// Tightens an optional limit: a parent cap applies even when the base is unlimited (None);
/// without a cap the base is kept unchanged.
fn min_optional_cap(base: Option<u32>, cap: Option<u32>) -> Option<u32> {
    match (base, cap) {
        (None, cap) => cap,
        (base, None) => base,
        (Some(base), Some(cap)) => Some(base.min(cap)),
    }
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
const fn default_web_max_hosted_requests_per_run() -> Option<u32> {
    None
}
const fn default_web_provider_hosted_max_uses() -> Option<u32> {
    None
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
    /// Internal storage for the provider-neutral durable recovery policy. `RootConfig` maps the
    /// public `[recovery.provider]` table here at parse/save boundaries so existing runtime
    /// consumers retain one coherent request-transport policy.
    #[serde(default)]
    pub provider_turn_recovery: ProviderTurnRecoveryConfig,
}

impl Default for ModelRequestConfig {
    fn default() -> Self {
        Self {
            request_timeout_secs: default_model_request_timeout_secs(),
            stream_idle_timeout_secs: default_model_request_stream_idle_timeout_secs(),
            stream_total_timeout_secs: None,
            provider_turn_recovery: ProviderTurnRecoveryConfig::default(),
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

    /// Resolves the bounded policy used for provider-turn recovery schedules.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured retry or delay value exceeds the product hard caps or
    /// the delay bounds are internally inconsistent.
    pub fn provider_turn_recovery_policy(&self) -> Result<ProviderTurnRecoveryPolicyV1> {
        self.provider_turn_recovery.to_policy()
    }
}

/// Product-configurable bounds for future durable provider-turn recovery schedules.
///
/// The canonical user-facing table is `[recovery.provider]`. Existing schedules keep their
/// persisted policy fingerprint and never change when this configuration changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnRecoveryConfig {
    #[serde(default = "default_provider_turn_max_transport_retries")]
    pub max_transport_retries: u32,
    #[serde(default = "default_provider_turn_max_partial_output_retries")]
    pub max_partial_output_retries: u32,
    #[serde(default = "default_provider_turn_initial_delay_ms")]
    pub initial_delay_ms: u64,
    #[serde(default = "default_provider_turn_max_delay_ms")]
    pub max_delay_ms: u64,
    #[serde(
        default = "default_provider_turn_jitter_ratio_millionths",
        rename = "jitter_ratio",
        serialize_with = "serialize_provider_turn_jitter_ratio",
        deserialize_with = "deserialize_provider_turn_jitter_ratio"
    )]
    pub jitter_ratio_millionths: u32,
    #[serde(default = "default_provider_turn_max_cumulative_delay_ms")]
    pub max_cumulative_delay_ms: u64,
}

impl Default for ProviderTurnRecoveryConfig {
    fn default() -> Self {
        Self {
            max_transport_retries: default_provider_turn_max_transport_retries(),
            max_partial_output_retries: default_provider_turn_max_partial_output_retries(),
            initial_delay_ms: default_provider_turn_initial_delay_ms(),
            max_delay_ms: default_provider_turn_max_delay_ms(),
            jitter_ratio_millionths: default_provider_turn_jitter_ratio_millionths(),
            max_cumulative_delay_ms: default_provider_turn_max_cumulative_delay_ms(),
        }
    }
}

impl ProviderTurnRecoveryConfig {
    /// Resolves the config to the policy consumed by a provider-turn owner.
    pub fn to_policy(&self) -> Result<ProviderTurnRecoveryPolicyV1> {
        anyhow::ensure!(
            self.max_transport_retries <= 10,
            "model_request.provider_turn_recovery.max_transport_retries must not exceed 10"
        );
        anyhow::ensure!(
            self.max_partial_output_retries <= 3,
            "model_request.provider_turn_recovery.max_partial_output_retries must not exceed 3"
        );
        anyhow::ensure!(
            self.initial_delay_ms <= 60_000,
            "model_request.provider_turn_recovery.initial_delay_ms must not exceed 60000"
        );
        anyhow::ensure!(
            self.max_delay_ms <= 120_000,
            "model_request.provider_turn_recovery.max_delay_ms must not exceed 120000"
        );
        anyhow::ensure!(
            self.max_cumulative_delay_ms <= 600_000,
            "model_request.provider_turn_recovery.max_cumulative_delay_ms must not exceed 600000"
        );
        anyhow::ensure!(
            self.jitter_ratio_millionths <= 1_000_000,
            "model_request.provider_turn_recovery.jitter_ratio must be between 0.0 and 1.0"
        );
        let policy = ProviderTurnRecoveryPolicyV1 {
            max_transport_retries: self.max_transport_retries,
            max_partial_output_retries: self.max_partial_output_retries,
            initial_delay_ms: self.initial_delay_ms,
            max_delay_ms: self.max_delay_ms,
            jitter_ratio_millionths: self.jitter_ratio_millionths,
            max_cumulative_delay_ms: self.max_cumulative_delay_ms,
        };
        policy.validate()?;
        Ok(policy)
    }
}

const fn default_provider_turn_max_transport_retries() -> u32 {
    DEFAULT_PROVIDER_TURN_MAX_TRANSPORT_RETRIES
}

const fn default_provider_turn_max_partial_output_retries() -> u32 {
    DEFAULT_PROVIDER_TURN_MAX_PARTIAL_OUTPUT_RETRIES
}

const fn default_provider_turn_initial_delay_ms() -> u64 {
    DEFAULT_PROVIDER_TURN_INITIAL_DELAY_MS
}

const fn default_provider_turn_max_delay_ms() -> u64 {
    DEFAULT_PROVIDER_TURN_MAX_DELAY_MS
}

const fn default_provider_turn_jitter_ratio_millionths() -> u32 {
    DEFAULT_PROVIDER_TURN_JITTER_RATIO_MILLIONTHS
}

const fn default_provider_turn_max_cumulative_delay_ms() -> u64 {
    DEFAULT_PROVIDER_TURN_MAX_CUMULATIVE_DELAY_MS
}

fn serialize_provider_turn_jitter_ratio<S>(
    millionths: &u32,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_f64(f64::from(*millionths) / 1_000_000.0)
}

fn deserialize_provider_turn_jitter_ratio<'de, D>(
    deserializer: D,
) -> std::result::Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let ratio = f64::deserialize(deserializer)?;
    if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
        return Err(serde::de::Error::custom(
            "provider_turn_recovery.jitter_ratio must be between 0.0 and 1.0",
        ));
    }
    let millionths = ratio * 1_000_000.0;
    let rounded = millionths.round();
    if (millionths - rounded).abs() > f64::EPSILON {
        return Err(serde::de::Error::custom(
            "provider_turn_recovery.jitter_ratio supports at most six decimal places",
        ));
    }
    Ok(rounded as u32)
}

/// Parses the canonical RFC-0068 `[recovery.provider]` table into the existing internal request
/// transport configuration. Keeping this translation at the root boundary makes the on-disk
/// contract explicit without leaking a provider-recovery ownership concern into every consumer
/// that legitimately owns `ModelRequestConfig` today.
fn parse_root_config_toml(raw: &str) -> Result<RootConfig> {
    let mut root: toml::Table =
        toml::from_str(raw).map_err(|_| anyhow::anyhow!("failed to parse config"))?;
    let Some(recovery) = root.remove("recovery") else {
        return toml::from_str(raw).map_err(|_| anyhow::anyhow!("failed to parse config"));
    };
    let recovery = recovery.as_table().context("[recovery] must be a table")?;
    anyhow::ensure!(
        recovery.len() == 1 && recovery.contains_key("provider"),
        "[recovery] supports only the [recovery.provider] policy table"
    );
    let provider_policy = recovery
        .get("provider")
        .cloned()
        .context("[recovery.provider] is required when [recovery] is present")?;
    anyhow::ensure!(
        provider_policy.is_table(),
        "[recovery.provider] must be a table"
    );
    let model_request = root
        .entry("model_request".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let model_request = model_request
        .as_table_mut()
        .context("[model_request] must be a table")?;
    anyhow::ensure!(
        !model_request.contains_key("provider_turn_recovery"),
        "configure provider recovery in [recovery.provider], not [model_request.provider_turn_recovery]"
    );
    model_request.insert("provider_turn_recovery".to_owned(), provider_policy);
    let normalized = toml::to_string(&root).context("failed to normalize recovery config")?;
    toml::from_str(&normalized).map_err(|_| anyhow::anyhow!("failed to parse config"))
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
        Self::load_with_model_request_env(path, |name| env::var(name).ok())
    }

    /// Loads the exact persisted configuration without applying process environment overrides.
    ///
    /// Mutation paths use this view so a temporary runtime override cannot become a durable
    /// setting as a side effect of publishing an unrelated configuration change.
    pub fn load_persisted(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        Self::parse_persisted(&raw)
            .map_err(|_| anyhow::anyhow!("failed to parse {}", path.display()))
    }

    /// Parses persisted TOML without consulting process environment overrides.
    pub fn parse_persisted(raw: &str) -> Result<Self> {
        let config = parse_root_config_toml(raw)?;
        config.validate_config_schema()?;
        Ok(config)
    }

    /// Parses one configuration payload and applies only the documented model-request
    /// environment overrides. Callers that already hold an authority-opened config handle use
    /// this entry point so parsing cannot silently reopen a path that may have been replaced.
    pub fn parse_with_model_request_env(raw: &str) -> Result<Self> {
        let mut config = parse_root_config_toml(raw)
            .map_err(|_| anyhow::anyhow!("failed to parse configuration payload"))?;
        config.validate_config_schema()?;
        config.apply_model_request_env_overrides_with(|name| env::var(name).ok())?;
        Ok(config)
    }

    fn load_with_model_request_env(
        path: &Path,
        read_env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let mut config = parse_root_config_toml(&raw)
            .map_err(|_| anyhow::anyhow!("failed to parse {}", path.display()))?;
        config.validate_config_schema()?;
        config.apply_model_request_env_overrides_with(read_env)?;
        Ok(config)
    }

    /// Serializes the config to TOML and atomically publishes it to `path`.
    ///
    /// On Unix, the published file is always mode `0600`. A symbolic-link destination is rejected
    /// instead of followed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let lock = ConfigUpdateLockGuard::acquire(path)?;
        self.save_with_update_lock(path, &lock)
    }

    /// Publishes only when the live config still matches the caller's parsed source snapshot.
    pub fn save_if_unchanged(&self, path: &Path, expected: &Self) -> Result<()> {
        let lock = ConfigUpdateLockGuard::acquire(path)?;
        let live = Self::load(path)?;
        let expected = expected.render_persisted_toml()?;
        let live = live.render_persisted_toml()?;
        anyhow::ensure!(
            expected == live,
            "config changed since it was loaded; reload and retry"
        );
        self.save_with_update_lock(path, &lock)
    }

    /// Publishes only when the exact source bytes are still current under the update lock.
    pub fn save_if_source_bytes_unchanged(
        &self,
        path: &Path,
        expected_source: &[u8],
    ) -> Result<()> {
        let lock = ConfigUpdateLockGuard::acquire(path)?;
        let live = fs::read(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        anyhow::ensure!(
            live == expected_source,
            "config changed since it was loaded; reload and retry"
        );
        self.save_with_update_lock(path, &lock)
    }

    /// Serializes and publishes while the caller holds this config's update lock.
    ///
    /// This is used by multi-resource transactions that must keep credential and config updates
    /// under one cross-process lock.
    pub fn save_with_update_lock(&self, path: &Path, lock: &ConfigUpdateLockGuard) -> Result<()> {
        anyhow::ensure!(
            lock.config_path == path,
            "config update lock does not match publication path"
        );
        let rendered = self.render_persisted_toml()?;
        atomic_publish_private_file(path, rendered.as_bytes())
            .with_context(|| format!("failed to write config at {}", path.display()))
    }

    fn render_persisted_toml(&self) -> Result<String> {
        let mut root = toml::Value::try_from(self.clone())
            .context("failed to serialize root config to toml")?;
        let root_table = root
            .as_table_mut()
            .context("serialized root config must be a TOML table")?;
        let Some(model_request) = root_table.get_mut("model_request") else {
            return toml::to_string_pretty(&root)
                .context("failed to serialize root config to toml");
        };
        let model_request = model_request
            .as_table_mut()
            .context("serialized model_request must be a TOML table")?;
        let Some(provider_policy) = model_request.remove("provider_turn_recovery") else {
            return toml::to_string_pretty(&root)
                .context("failed to serialize root config to toml");
        };
        let mut recovery = toml::Table::new();
        recovery.insert("provider".to_owned(), provider_policy);
        root_table.insert("recovery".to_owned(), toml::Value::Table(recovery));
        toml::to_string_pretty(&root).context("failed to serialize root config to toml")
    }

    /// Applies provider-neutral model request timeout environment overrides.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured override is not a positive integer.
    pub fn apply_model_request_env_overrides(&mut self) -> Result<()> {
        self.apply_model_request_env_overrides_with(|name| env::var(name).ok())
    }

    fn apply_model_request_env_overrides_with(
        &mut self,
        read_env: impl Fn(&str) -> Option<String>,
    ) -> Result<()> {
        if let Some(value) =
            read_positive_env_u64_with(SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV, &read_env)?
        {
            self.model_request.request_timeout_secs = value;
        }
        if let Some(value) =
            read_positive_env_u64_with(SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV, &read_env)?
        {
            self.model_request.stream_idle_timeout_secs = value;
        }
        if let Some(value) =
            read_positive_env_u64_with(SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV, &read_env)?
        {
            self.model_request.stream_total_timeout_secs = Some(value);
        }
        Ok(())
    }

    fn validate_config_schema(&self) -> Result<()> {
        anyhow::ensure!(
            self.config_version == CONFIG_VERSION_V2,
            "config_version = {CONFIG_VERSION_V2} is required"
        );
        anyhow::ensure!(
            self.agent.connection.is_some(),
            "config_version = {CONFIG_VERSION_V2} requires [agent].connection"
        );
        self.model_request.provider_turn_recovery_policy()?;
        for (name, role) in self.task.role_configs() {
            anyhow::ensure!(
                role.connection.is_some() == role.model.is_some(),
                "config_version = {CONFIG_VERSION_V2} requires [{name}].connection and [{name}].model to be configured together"
            );
            if let (Some(connection), Some(model)) = (role.connection.as_ref(), role.model.as_ref())
            {
                ModelRef::new(connection.clone(), model.clone())
                    .map_err(anyhow::Error::new)
                    .with_context(|| format!("invalid [{name}] model route"))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigPublishError {
    #[error(
        "config replacement for {path} was partially applied; preserve recovery file {recovery_path}"
    )]
    ReplacementPartiallyApplied {
        path: PathBuf,
        recovery_path: PathBuf,
        previous_path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "config {path} was published, but parent-directory synchronization failed; durability is uncertain"
    )]
    PublishedButDurabilityUncertain {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "config {path} was published through its opened parent, but pathname visibility is uncertain"
    )]
    PublishedButVisibilityUncertain { path: PathBuf },
}

/// Exclusive cross-process guard for one root-config update transaction.
#[derive(Debug)]
pub struct ConfigUpdateLockGuard {
    config_path: PathBuf,
    file: File,
    #[cfg(unix)]
    _parent_directory: File,
    #[cfg(windows)]
    _parent_guards: Vec<File>,
}

impl ConfigUpdateLockGuard {
    /// Acquires the lock associated with `config_path`.
    ///
    /// The lock file is owner-only and rejects symlink traversal on Unix.
    pub fn acquire(config_path: &Path) -> Result<Self> {
        let parent = config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_created = secure_config_parent(parent)?;
        #[cfg(unix)]
        let parent_directory = open_config_parent_directory(parent)?;
        #[cfg(unix)]
        if parent_created {
            secure_opened_config_parent(&parent_directory, parent)?;
        }
        #[cfg(windows)]
        let parent_guards = lock_windows_config_parent_ancestors(parent)?;
        #[cfg(windows)]
        if parent_created {
            secure_private_path_permissions(parent)?;
        }
        let file_name = config_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sigil.toml");
        let lock_path = parent.join(format!(".{file_name}.update.lock"));
        #[cfg(unix)]
        let file = open_config_update_lock_at(
            &parent_directory,
            lock_path
                .file_name()
                .expect("config update lock path always has a file name"),
            &lock_path,
        )?;
        #[cfg(not(unix))]
        let file = {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true);
            let file = options.open(&lock_path).with_context(|| {
                format!("failed to open config update lock {}", lock_path.display())
            })?;
            secure_private_path_permissions(&lock_path)?;
            file
        };
        file.lock_exclusive().with_context(|| {
            format!(
                "failed to acquire config update lock {}",
                lock_path.display()
            )
        })?;
        Ok(Self {
            config_path: config_path.to_path_buf(),
            file,
            #[cfg(unix)]
            _parent_directory: parent_directory,
            #[cfg(windows)]
            _parent_guards: parent_guards,
        })
    }
}

impl Drop for ConfigUpdateLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(unix)]
fn open_config_update_lock_at(
    parent: &File,
    lock_name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };

    let name = std::ffi::CString::new(lock_name.as_bytes())
        .context("config update lock name contains a NUL byte")?;
    // SAFETY: the directory descriptor and relative C string are valid. O_NOFOLLOW makes an
    // existing symbolic-link lock fail instead of resolving it.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to open config update lock {}",
                display_path.display()
            )
        });
    }
    // SAFETY: descriptor was returned by openat and transfers to File exactly once.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", display_path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "config update lock is not a regular file: {}",
        display_path.display()
    );
    // SAFETY: file owns a valid descriptor.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to secure {}", display_path.display()));
    }
    Ok(file)
}

/// Atomically publishes one private local-state file using the same path, permission, no-follow,
/// durability, and Windows replacement guarantees as the root configuration.
///
/// The caller owns serialization and must create any missing parent hierarchy before calling.
///
/// # Errors
///
/// Returns an error when the path is unsafe, private permissions cannot be established, the
/// create-new temporary file cannot be synced, or atomic publication is not durable.
pub fn atomic_publish_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_publish_private_config_with_parent_sync(path, bytes, sync_config_parent)
}

/// Reports whether one existing private file or directory is restricted to the current user
/// (plus Local System on Windows).
///
/// This is a read-only diagnostic counterpart to [`secure_private_path_permissions`].
///
/// # Errors
///
/// Returns an error when the path cannot be inspected or its Windows security descriptor cannot
/// be read.
pub fn private_path_permissions_are_restricted(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect private path {}", path.display()))?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir()),
        "private path is not a regular file or directory: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        Ok(metadata.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        let current_user_sid = current_windows_user_sid_string()?;
        let sddl = windows_private_dacl_sddl(path)?;
        let current_user_is_present =
            windows_private_sddl_has_current_user(&sddl, &current_user_sid);
        let protected = sddl.contains("D:P");
        let system_allowed = sddl.contains(";;;SY)");
        let broad_principal_allowed = [";;;OW)", ";;;WD)", ";;;BU)", ";;;AU)", ";;;AN)"]
            .iter()
            .any(|principal| sddl.contains(principal));
        Ok(current_user_is_present && protected && system_allowed && !broad_principal_allowed)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Ok(true)
    }
}

/// Tightens one already-created private file or directory using the platform's native access
/// controls.
///
/// Unix callers receive owner-only mode (`0600` for files, `0700` for directories). Windows
/// callers receive a protected DACL that grants full access only to the current process user and
/// Local System, and the current user becomes the object owner.
/// Symbolic links and Windows reparse points are rejected.
///
/// # Errors
///
/// Returns an error when the path is not a regular file/directory or the platform cannot establish
/// the private permissions.
pub fn secure_private_path_permissions(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect private path {}", path.display()))?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && (metadata.is_file() || metadata.is_dir()),
        "private path is not a regular file or directory: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};

        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                },
                DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW,
            },
        };

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        anyhow::ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "private path is a Windows reparse point: {}",
            path.display()
        );
        let current_user_sid = current_windows_user_sid_string()?;
        let sddl =
            format!("O:{current_user_sid}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{current_user_sid})")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the SDDL buffer is NUL terminated and descriptor is writable for the duration of
        // the call. Windows owns the returned allocation until LocalFree below.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to construct private Windows DACL");
        }
        let wide_path = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: the path is NUL terminated and descriptor remains valid until LocalFree.
        let applied = unsafe {
            SetFileSecurityW(
                wide_path.as_ptr(),
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            )
        };
        let source = (applied == 0).then(std::io::Error::last_os_error);
        // SAFETY: descriptor was allocated by LocalAlloc through the conversion API and is freed
        // exactly once here.
        unsafe {
            let _ = LocalFree(descriptor);
        }
        if let Some(source) = source {
            return Err(source)
                .with_context(|| format!("failed to secure Windows ACL for {}", path.display()));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn current_windows_user_sid_string() -> Result<String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    use windows_sys::Win32::{
        Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a process pseudo-handle and token is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to open current user token");
    }
    // SAFETY: OpenProcessToken transferred ownership of one valid handle.
    let token = unsafe { fs::File::from_raw_handle(token) };
    let mut bytes = 0_u32;
    // SAFETY: the zero-length probe intentionally supplies a null output buffer.
    let first = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut bytes,
        )
    };
    // SAFETY: GetLastError reads thread-local Win32 error state after the immediately preceding
    // failed sizing call.
    let first_error = unsafe { GetLastError() };
    anyhow::ensure!(
        first == 0 && first_error == ERROR_INSUFFICIENT_BUFFER && bytes > 0,
        "failed to size current Windows user token"
    );
    let mut storage = vec![0_u8; bytes as usize];
    // SAFETY: storage has the exact byte capacity requested by the sizing call.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("failed to read current Windows user token");
    }
    // SAFETY: GetTokenInformation initialized a TOKEN_USER structure at the start of storage.
    let token_user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let mut rendered = std::ptr::null_mut();
    // SAFETY: token_user.User.Sid is owned by storage and rendered is writable.
    if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut rendered) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to render current Windows user SID");
    }
    let len = {
        let mut len = 0_usize;
        // SAFETY: ConvertSidToStringSidW returned a NUL-terminated UTF-16 allocation.
        while unsafe { *rendered.add(len) } != 0 {
            len += 1;
        }
        len
    };
    // SAFETY: rendered points to len initialized UTF-16 code units.
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(rendered, len) });
    // SAFETY: rendered was allocated by LocalAlloc through ConvertSidToStringSidW.
    unsafe {
        let _ = LocalFree(rendered.cast());
    }
    Ok(value)
}

#[cfg(windows)]
fn windows_private_sddl_has_current_user(sddl: &str, current_user_sid: &str) -> bool {
    let has_principal = |principal: &str| {
        sddl.contains(&format!("O:{principal}")) && sddl.contains(&format!(";;;{principal})"))
    };
    has_principal(current_user_sid)
        || windows_current_user_sddl_alias(current_user_sid).is_some_and(has_principal)
}

#[cfg(windows)]
fn windows_current_user_sddl_alias(current_user_sid: &str) -> Option<&'static str> {
    match current_user_sid {
        "S-1-5-18" => Some("SY"),
        "S-1-5-19" => Some("LS"),
        "S-1-5-20" => Some("NS"),
        sid if sid.ends_with("-500") => Some("LA"),
        sid if sid.ends_with("-501") => Some("LG"),
        _ => None,
    }
}

#[cfg(windows)]
fn windows_private_dacl_sddl(path: &Path) -> Result<String> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::{
        Foundation::{ERROR_SUCCESS, LocalFree},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
                SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
    };

    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut owner = std::ptr::null_mut();
    // SAFETY: the path is NUL terminated and descriptor is writable for the API call.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .with_context(|| format!("failed to read Windows ACL for {}", path.display()));
    }
    let mut rendered = std::ptr::null_mut();
    let mut rendered_len = 0_u32;
    // SAFETY: descriptor remains valid and both output pointers are writable.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut rendered,
            &mut rendered_len,
        )
    };
    let source = (converted == 0).then(std::io::Error::last_os_error);
    let value = if source.is_none() {
        // SAFETY: the conversion API returned a valid UTF-16 buffer with the reported length.
        Some(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(rendered, rendered_len as usize)
        }))
    } else {
        None
    };
    // SAFETY: both allocations, when non-null, were returned by Windows LocalAlloc APIs.
    unsafe {
        if !rendered.is_null() {
            let _ = LocalFree(rendered.cast());
        }
        let _ = LocalFree(descriptor);
    }
    if let Some(source) = source {
        return Err(source)
            .with_context(|| format!("failed to render Windows ACL for {}", path.display()));
    }
    Ok(value.expect("successful SDDL conversion produced a value"))
}

fn atomic_publish_private_config_with_parent_sync<F>(
    path: &Path,
    bytes: &[u8],
    sync_parent: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    let secure_existing_parent = path
        .parent()
        .zip(
            default_user_config_path()
                .ok()
                .and_then(|default_path| default_path.parent().map(Path::to_path_buf)),
        )
        .is_some_and(|(parent, default_parent)| parent == default_parent);
    atomic_publish_private_config_with_parent_policy(
        path,
        bytes,
        secure_existing_parent,
        sync_parent,
    )
}

fn atomic_publish_private_config_with_parent_policy<F>(
    path: &Path,
    bytes: &[u8],
    secure_existing_parent: bool,
    sync_parent: F,
) -> Result<()>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    let explicit_parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = explicit_parent.unwrap_or_else(|| Path::new("."));
    let parent_created = secure_config_parent(parent)?;
    #[cfg(windows)]
    let _windows_parent_guards = lock_windows_config_parent_ancestors(parent)?;
    #[cfg(windows)]
    if parent_created || secure_existing_parent {
        secure_private_path_permissions(parent)?;
    }
    #[cfg(unix)]
    let parent_directory = open_config_parent_directory(parent)?;
    #[cfg(unix)]
    if parent_created || secure_existing_parent {
        secure_opened_config_parent(&parent_directory, parent)?;
    }
    #[cfg(unix)]
    let parent_identity = config_parent_file_identity(&parent_directory)?;
    #[cfg(not(unix))]
    let parent_identity = config_parent_identity(parent)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("config path has no file name: {}", path.display()))?;
    #[cfg(unix)]
    let _target_exists = inspect_config_target_at(&parent_directory, file_name, path)?;
    #[cfg(not(unix))]
    let target_exists = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "refusing to replace symbolic-link config {}",
                path.display()
            );
        }
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => {
            anyhow::bail!(
                "config destination is not a regular file: {}",
                path.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect config {}", path.display()));
        }
    };

    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let temp_path = parent.join(&temp_name);

    let result = (|| -> Result<()> {
        #[cfg(unix)]
        let mut file = create_private_config_temp_at(&parent_directory, &temp_name, &temp_path)?;
        #[cfg(not(unix))]
        let mut file = {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                use windows_sys::Win32::Storage::FileSystem::{
                    FILE_GENERIC_READ, FILE_GENERIC_WRITE, WRITE_DAC, WRITE_OWNER,
                };
                options
                    .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC | WRITE_OWNER);
            }
            options
                .open(&temp_path)
                .with_context(|| format!("failed to create {}", temp_path.display()))?
        };
        #[cfg(windows)]
        secure_private_path_permissions(&temp_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            file.set_permissions(fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to secure {}", temp_path.display()))?;
        }
        file.write_all(bytes)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
        drop(file);
        #[cfg(windows)]
        if target_exists {
            secure_private_path_permissions(path)?;
        }
        anyhow::ensure!(
            config_parent_identity(parent)? == parent_identity,
            "config parent changed while publishing {}",
            path.display()
        );
        #[cfg(unix)]
        replace_config_file_at(&parent_directory, &temp_name, file_name, path)?;
        #[cfg(not(unix))]
        replace_config_file(&temp_path, path, target_exists)?;
        #[cfg(windows)]
        if secure_private_path_permissions(path).is_err() {
            return Err(ConfigPublishError::PublishedButVisibilityUncertain {
                path: path.to_path_buf(),
            }
            .into());
        }
        #[cfg(unix)]
        if config_parent_identity(parent).ok().as_ref() != Some(&parent_identity) {
            return Err(ConfigPublishError::PublishedButVisibilityUncertain {
                path: path.to_path_buf(),
            }
            .into());
        }
        if let Err(source) = sync_parent(parent) {
            return Err(ConfigPublishError::PublishedButDurabilityUncertain {
                path: path.to_path_buf(),
                source,
            }
            .into());
        }
        #[cfg(unix)]
        if let Err(source) = parent_directory.sync_all() {
            return Err(ConfigPublishError::PublishedButDurabilityUncertain {
                path: path.to_path_buf(),
                source,
            }
            .into());
        }
        Ok(())
    })();

    let preserve_recovery_file = result.as_ref().err().is_some_and(|error| {
        matches!(
            error.downcast_ref::<ConfigPublishError>(),
            Some(ConfigPublishError::ReplacementPartiallyApplied { .. })
        )
    });
    if result.is_err() && !preserve_recovery_file {
        #[cfg(unix)]
        let _ = remove_config_temp_at(&parent_directory, &temp_name);
        #[cfg(not(unix))]
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn secure_config_parent(parent: &Path) -> Result<bool> {
    reject_config_parent_symlink_components(parent)?;
    let created = match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "refusing to use symbolic-link config parent {}",
                parent.display()
            );
        }
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!(
                "failed to create config parent because it is not a directory: {}",
                parent.display()
            );
        }
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            let metadata = fs::symlink_metadata(parent)
                .with_context(|| format!("failed to inspect {}", parent.display()))?;
            anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "config parent is not a private directory: {}",
                parent.display()
            );
            true
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", parent.display()));
        }
    };

    Ok(created)
}

fn reject_config_parent_symlink_components(parent: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(component, std::path::Component::Prefix(_)) {
            // A Windows drive or verbatim prefix is not inspectable until the following root
            // component has been appended (for example, `\\?\C:` becomes `\\?\C:\`).
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let is_root_level_alias = current
                    .parent()
                    .is_some_and(|ancestor| ancestor == Path::new("/"));
                anyhow::ensure!(
                    is_root_level_alias,
                    "refusing to traverse symbolic-link config ancestor {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_config_parent_directory(parent: &Path) -> Result<fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };

    let walk_path = config_parent_walk_path(parent)?;
    let mut directory = if walk_path.is_absolute() {
        fs::File::open("/").context("failed to open filesystem root for config publish")?
    } else {
        fs::File::open(".").context("failed to open current directory for config publish")?
    };
    for component in walk_path.components() {
        let name = match component {
            std::path::Component::RootDir | std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => std::ffi::OsStr::new(".."),
            std::path::Component::Normal(name) => name,
            std::path::Component::Prefix(_) => {
                anyhow::bail!(
                    "unsupported Unix config parent prefix {}",
                    walk_path.display()
                );
            }
        };
        let name = std::ffi::CString::new(name.as_bytes())
            .context("config parent component contains a NUL byte")?;
        // SAFETY: directory owns a valid descriptor and name is a valid relative C string.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to open config parent component without following links: {}",
                    parent.display()
                )
            });
        }
        // SAFETY: descriptor was returned by openat and transfers to File exactly once.
        directory = unsafe { fs::File::from_raw_fd(descriptor) };
    }
    Ok(directory)
}

#[cfg(unix)]
fn config_parent_walk_path(parent: &Path) -> Result<PathBuf> {
    if !parent.is_absolute() {
        return Ok(parent.to_path_buf());
    }
    let mut components = parent.components();
    let Some(std::path::Component::RootDir) = components.next() else {
        return Ok(parent.to_path_buf());
    };
    let Some(first) = components.next() else {
        return Ok(parent.to_path_buf());
    };
    let std::path::Component::Normal(first_name) = first else {
        return Ok(parent.to_path_buf());
    };
    let first_path = Path::new("/").join(first_name);
    let metadata = fs::symlink_metadata(&first_path)
        .with_context(|| format!("failed to inspect {}", first_path.display()))?;
    if !metadata.file_type().is_symlink() {
        return Ok(parent.to_path_buf());
    }
    let mut resolved = first_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve root-level alias {}",
            first_path.display()
        )
    })?;
    anyhow::ensure!(
        resolved.is_absolute(),
        "root-level config alias did not resolve to an absolute directory"
    );
    for component in components {
        resolved.push(component.as_os_str());
    }
    Ok(resolved)
}

#[cfg(unix)]
fn secure_opened_config_parent(directory: &fs::File, display_path: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: directory owns a valid descriptor for the exact opened parent.
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to secure {}", display_path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn config_parent_file_identity(directory: &fs::File) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = directory
        .metadata()
        .context("failed to inspect opened config parent")?;
    anyhow::ensure!(metadata.is_dir(), "opened config parent is not a directory");
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
fn inspect_config_target_at(
    parent: &fs::File,
    file_name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<bool> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let name = std::ffi::CString::new(file_name.as_bytes())
        .context("config file name contains a NUL byte")?;
    // SAFETY: stat points to initialized writable storage; directory fd and C string are valid.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    // SAFETY: arguments remain valid for the duration of the syscall.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        let file_type = stat.st_mode & libc::S_IFMT;
        anyhow::ensure!(
            file_type != libc::S_IFLNK,
            "refusing to replace symbolic-link config {}",
            display_path.display()
        );
        anyhow::ensure!(
            file_type == libc::S_IFREG,
            "config destination is not a regular file: {}",
            display_path.display()
        );
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(false)
    } else {
        Err(error).with_context(|| format!("failed to inspect config {}", display_path.display()))
    }
}

#[cfg(unix)]
fn create_private_config_temp_at(
    parent: &fs::File,
    temp_name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };

    let name = std::ffi::CString::new(temp_name.as_bytes())
        .context("temporary config file name contains a NUL byte")?;
    // SAFETY: directory fd and C string are valid; returned descriptor is uniquely owned below.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to create {}", display_path.display()));
    }
    // SAFETY: descriptor was returned by openat and ownership transfers to File exactly once.
    let file = unsafe { fs::File::from_raw_fd(descriptor) };
    // SAFETY: file owns a valid descriptor.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to secure {}", display_path.display()));
    }
    Ok(file)
}

#[cfg(unix)]
fn replace_config_file_at(
    parent: &fs::File,
    temp_name: &std::ffi::OsStr,
    file_name: &std::ffi::OsStr,
    display_path: &Path,
) -> Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let temp = std::ffi::CString::new(temp_name.as_bytes())
        .context("temporary config file name contains a NUL byte")?;
    let target = std::ffi::CString::new(file_name.as_bytes())
        .context("config file name contains a NUL byte")?;
    // SAFETY: both names are relative to the same valid directory descriptor.
    let result = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            temp.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to atomically replace config {} through opened parent",
                display_path.display()
            )
        })
    }
}

#[cfg(unix)]
fn remove_config_temp_at(parent: &fs::File, temp_name: &std::ffi::OsStr) -> std::io::Result<()> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let name = std::ffi::CString::new(temp_name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "temporary config file name contains a NUL byte",
        )
    })?;
    // SAFETY: directory descriptor and relative C string are valid.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn config_parent_identity(parent: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    reject_config_parent_symlink_components(parent)?;
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "config parent is no longer a directory: {}",
        parent.display()
    );
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn config_parent_identity(parent: &Path) -> Result<PathBuf> {
    reject_config_parent_symlink_components(parent)?;
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("failed to inspect {}", parent.display()))?;
    anyhow::ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "config parent is no longer a directory: {}",
        parent.display()
    );
    parent
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", parent.display()))
}

#[cfg(windows)]
fn lock_windows_config_parent_ancestors(parent: &Path) -> Result<Vec<fs::File>> {
    use std::{
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        ptr::{null, null_mut},
    };

    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
            OPEN_EXISTING,
        },
    };

    let absolute = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for config publish")?
            .join(parent)
    };
    let mut current = PathBuf::new();
    let mut rooted = false;
    let mut guards = Vec::new();
    for component in absolute.components() {
        use std::path::Component;

        match component {
            Component::Prefix(_) => current.push(component.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                rooted = true;
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                anyhow::bail!(
                    "config parent must not contain parent traversal: {}",
                    parent.display()
                );
            }
            Component::Normal(_) => current.push(component.as_os_str()),
        }
        if !rooted {
            continue;
        }

        let wide = current
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // Omitting FILE_SHARE_DELETE pins every lexical ancestor against rename/deletion while
        // the later pathname-based ReplaceFileW/MoveFileExW sequence is in flight.
        // SAFETY: wide is NUL-terminated and the returned handle is checked before ownership.
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to lock Windows config ancestor {}",
                    current.display()
                )
            });
        }
        // SAFETY: CreateFileW returned an owned, non-sentinel handle.
        let file = unsafe { fs::File::from_raw_handle(raw) };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file is live and info is a valid output pointer.
        if unsafe { GetFileInformationByHandle(raw, &raw mut info) } == 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to inspect locked Windows config ancestor {}",
                    current.display()
                )
            });
        }
        anyhow::ensure!(
            info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "refusing Windows reparse or non-directory config ancestor {}",
            current.display()
        );
        guards.push(file);
    }
    anyhow::ensure!(
        !guards.is_empty(),
        "config parent does not resolve to a rooted Windows directory"
    );
    Ok(guards)
}

#[cfg(windows)]
fn replace_config_file(temp_path: &Path, path: &Path, target_exists: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
    };

    let source = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // Existing files use ReplaceFileW so the destination's DACL is retained. New files inherit
    // the user-local parent ACL and use MoveFileExW for a write-through publish. No replacement
    // backup is ever requested: an old V1 config can contain a plaintext credential and must not
    // be copied into a Sigil recovery file.
    // SAFETY: both buffers are valid NUL-terminated UTF-16 strings and remain alive for the call.
    let replaced = if target_exists {
        unsafe {
            ReplaceFileW(
                destination.as_ptr(),
                source.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        }
    } else {
        unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        let source = std::io::Error::last_os_error();
        if target_exists && windows_replace_error_requires_recovery(source.raw_os_error()) {
            return Err(ConfigPublishError::ReplacementPartiallyApplied {
                path: path.to_path_buf(),
                recovery_path: temp_path.to_path_buf(),
                previous_path: None,
                source,
            }
            .into());
        }
        return Err(source).with_context(|| {
            format!(
                "failed to atomically replace {} with {}",
                path.display(),
                temp_path.display()
            )
        });
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn windows_replace_error_requires_recovery(raw_os_error: Option<i32>) -> bool {
    matches!(raw_os_error, Some(1175..=1177))
}

#[cfg(not(any(unix, windows)))]
fn replace_config_file(temp_path: &Path, path: &Path, _target_exists: bool) -> Result<()> {
    fs::rename(temp_path, path).with_context(|| {
        format!(
            "failed to atomically replace {} with {}",
            path.display(),
            temp_path.display()
        )
    })
}

#[cfg(not(windows))]
fn sync_config_parent(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent).and_then(|directory| directory.sync_all())
}

#[cfg(windows)]
fn sync_config_parent(_parent: &Path) -> std::io::Result<()> {
    // ReplaceFileW and MoveFileExW are the platform publication barriers. Windows does not expose
    // a portable directory-fsync equivalent; a successful replacement is therefore the strongest
    // available committed state and permits verified keyring retirement.
    Ok(())
}

fn read_positive_env_u64_with(
    name: &str,
    read_env: impl Fn(&str) -> Option<String>,
) -> Result<Option<u64>> {
    let Some(value) = read_env(name)
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
/// Explicit paths always win. Relative explicit paths are anchored to the launch working
/// directory so every runtime owner observes one stable absolute config identity. Otherwise
/// Sigil uses `~/.sigil/sigil.toml`.
///
/// Workspace-local `sigil.toml` files are intentionally not discovered implicitly because they
/// often contain personal provider, permission, and MCP settings that should not be committed.
///
/// # Errors
///
/// Returns an error when the implicit per-user config directory cannot be determined.
pub fn preferred_config_path(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        });
    }
    let default_path = default_user_config_path()?;
    Ok(preferred_config_path_for_known_paths(None, default_path))
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
    /// Provider-login credential persistence. New configurations default to the owner-only Sigil
    /// credential file; native credential-store access remains an explicit policy choice.
    #[serde(default)]
    pub credential_store: CredentialStorageMode,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            state_root: StorageRoot::Auto,
            cache_root: StorageRoot::Auto,
            mutation_artifact_retention: MutationArtifactRetentionConfig::default(),
            credential_store: CredentialStorageMode::File,
        }
    }
}

/// Local provider credential persistence policy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStorageMode {
    /// Use the owner-only local file and silently read prior native records when possible.
    Auto,
    /// Require the OS credential store, allowing explicit system authentication UI.
    Keyring,
    /// Use the owner-only local credential file.
    #[default]
    File,
}

impl CredentialStorageMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Keyring => "keyring",
            Self::File => "file",
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
    /// Runtime-only provider adapter selected from the active connection.
    #[serde(skip)]
    pub runtime_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionId>,
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
    /// Controls whether ordinary conversation input may route to plan review or durable task
    /// orchestration. Explicit `manual` keeps chat-first behavior and disables automatic
    /// handoff; automatic routing never grants tool, shell, network, MCP or merge permission.
    #[serde(default)]
    pub routing_policy: TaskRoutingPolicy,
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
    #[serde(default = "default_max_parallel_read_steps")]
    pub max_parallel_read_steps: usize,
    #[serde(default = "default_max_parallel_changeset_steps")]
    pub max_parallel_changeset_steps: usize,
    #[serde(default = "default_max_planning_research_agents")]
    pub max_planning_research_agents: usize,
    #[serde(default = "default_allow_write_subagents")]
    pub allow_write_subagents: bool,
    #[serde(default)]
    pub multi_agent_mode: MultiAgentMode,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            enabled: default_task_enabled(),
            routing_policy: TaskRoutingPolicy::default(),
            planner: RoleModelConfig::default(),
            executor: RoleModelConfig::default(),
            subagent_read: RoleModelConfig::default(),
            subagent_write: RoleModelConfig::default(),
            max_plan_steps: default_max_plan_steps(),
            max_replans: default_max_replans(),
            max_subagents: default_max_subagents(),
            max_parallel_read_steps: default_max_parallel_read_steps(),
            max_parallel_changeset_steps: default_max_parallel_changeset_steps(),
            max_planning_research_agents: default_max_planning_research_agents(),
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

    fn role_configs(&self) -> impl Iterator<Item = (&'static str, &RoleModelConfig)> {
        [
            ("task.planner", &self.planner),
            ("task.executor", &self.executor),
            ("task.subagent_read", &self.subagent_read),
            ("task.subagent_write", &self.subagent_write),
        ]
        .into_iter()
    }
}

/// Admission policy for ordinary conversation-to-task routing.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskRoutingPolicy {
    Manual,
    #[default]
    Auto,
}

impl TaskRoutingPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

/// Model delegation policy for agent tools.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MultiAgentMode {
    None,
    #[default]
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

/// Optional model/runtime overrides for one task role.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct RoleModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionId>,
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
    /// Enables model-visible durable memory writes and cross-session retrieval.
    #[serde(default = "default_memory_writable")]
    pub writable: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
            writable: default_memory_writable(),
        }
    }
}

impl MemoryConfig {
    /// Builds the workspace-document configuration while leaving writable memory disabled.
    #[must_use]
    pub const fn with_enabled(enabled: bool) -> Self {
        Self {
            enabled,
            writable: false,
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
    #[serde(default = "default_skill_compatibility_auto_discover")]
    pub compatibility_auto_discover: bool,
    #[serde(default = "default_skill_compatibility_sources")]
    pub compatibility_sources: Vec<String>,
}

impl Default for SkillConfig {
    fn default() -> Self {
        Self {
            enabled: default_skill_enabled(),
            user_skills: default_skill_user_skills(),
            user_agents: default_skill_user_agents(),
            compatibility_auto_discover: default_skill_compatibility_auto_discover(),
            compatibility_sources: default_skill_compatibility_sources(),
        }
    }
}

/// Context compaction configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionConfig {
    /// Request-layout and admission policy. V3 remains provider-capability gated at runtime.
    #[serde(default)]
    pub strategy: CompactionStrategy,
    #[serde(default = "default_compaction_enabled")]
    pub enabled: bool,
    /// Explicit opt-in for a provider-native acceleration carrier after portable truth is durable.
    ///
    /// This can cause one additional provider request and therefore defaults off.
    #[serde(default)]
    pub native_carrier_enabled: bool,
    /// Fallback model window used only when provider/model metadata cannot resolve one.
    #[serde(
        default,
        rename = "fallback_context_window_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub context_window_tokens: Option<u32>,
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

/// Current cache-aware compaction preparation boundary.
pub const COMPACTION_PREPARATION_RATIO: f32 = 0.70;

/// Current cache-aware compaction emergency boundary.
pub const COMPACTION_EMERGENCY_RATIO: f32 = 0.92;

/// Configured compaction rollout policy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    /// Cache-stable epoch rotation with adaptive tail and economics admission.
    #[default]
    CacheAwareV3,
}

impl CompactionStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CacheAwareV3 => "cache_aware_v3",
        }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            strategy: CompactionStrategy::default(),
            enabled: default_compaction_enabled(),
            native_carrier_enabled: false,
            context_window_tokens: None,
        }
    }
}

impl CompactionConfig {
    /// Classifies the latest prompt token count against the current V3 pressure boundaries.
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
        if ratio >= COMPACTION_EMERGENCY_RATIO {
            CompactionThresholdStatus::Hard
        } else if ratio >= COMPACTION_PREPARATION_RATIO {
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

fn default_max_parallel_read_steps() -> usize {
    4
}

fn default_max_parallel_changeset_steps() -> usize {
    2
}

fn default_max_planning_research_agents() -> usize {
    3
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

fn default_memory_writable() -> bool {
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

fn default_skill_compatibility_auto_discover() -> bool {
    true
}

fn default_skill_compatibility_sources() -> Vec<String> {
    Vec::new()
}

fn default_compaction_enabled() -> bool {
    true
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
