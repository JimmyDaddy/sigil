use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use sigil_kernel::{CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig};

use crate::{
    discovery::{
        DiscoveredLanguageServer, ServerAvailability, built_in_profiles, discover_language_servers,
    },
    error::CodeIntelError,
};

#[derive(Debug, Clone, Default, PartialEq)]
/// Effective language-server configuration and discovery status for Doctor output.
pub struct EffectiveServerPlan {
    /// Language servers that would be eligible for the workspace.
    pub servers: Vec<LanguageServerConfig>,
    /// Per-server discovery and configuration status.
    pub statuses: Vec<PlannedServerStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Provider-neutral status for one planned language server.
pub struct PlannedServerStatus {
    /// Stable server name.
    pub server: String,
    /// Language identifiers served by the process.
    pub languages: Vec<String>,
    /// Human-readable discovery or configuration state.
    pub status: String,
}

/// Computes the language-server plan for a workspace without starting any process.
///
/// Discovery failures are represented as a degraded status so Doctor remains diagnostic rather
/// than failing the entire command.
pub fn effective_server_plan(
    config: &CodeIntelligenceConfig,
    workspace_root: &Path,
) -> EffectiveServerPlan {
    if !config.auto_discover {
        return configured_or_default_plan(config);
    }

    match discover_language_servers(workspace_root, config.report_missing) {
        Ok(discovered) => effective_server_plan_from_discovered(config, discovered),
        Err(error) => {
            let mut plan = configured_or_default_plan(config);
            plan.statuses.push(PlannedServerStatus {
                server: "discovery".to_owned(),
                languages: Vec::new(),
                status: format!("degraded {error}"),
            });
            plan
        }
    }
}

pub fn default_rust_analyzer_server() -> LanguageServerConfig {
    built_in_profiles()
        .into_iter()
        .find(|profile| profile.name == "rust-analyzer")
        .map(|profile| profile.to_config())
        .unwrap_or_else(fallback_rust_analyzer_server)
}

fn fallback_rust_analyzer_server() -> LanguageServerConfig {
    LanguageServerConfig {
        name: "rust-analyzer".to_owned(),
        languages: vec!["rust".to_owned()],
        command: "rust-analyzer".to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        root_markers: vec!["Cargo.toml".to_owned(), "rust-project.json".to_owned()],
        file_extensions: vec!["rs".to_owned()],
        initialization_options: serde_json::json!({ "check": { "command": "check" } }),
        trust_required: true,
        startup_timeout_ms: 10_000,
    }
}

/// Returns whether code intelligence is both enabled and permitted to start on demand.
pub fn config_enabled(config: &CodeIntelligenceConfig) -> bool {
    config.enabled && config.server_startup != CodeIntelStartup::Off
}

pub fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace root {}",
            workspace_root.display()
        )
    })
}

pub fn resolve_workspace_file(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    if requested.trim().is_empty() {
        bail!("path cannot be empty");
    }
    let root = canonical_workspace_root(workspace_root)?;
    let request_path = PathBuf::from(requested);
    let candidate = if request_path.is_absolute() {
        request_path
    } else {
        root.join(request_path)
    };
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CodeIntelError::NotFound {
                path: requested.to_owned(),
            }
        } else {
            CodeIntelError::Io {
                path: candidate.clone(),
                source: error,
            }
        }
    })?;
    if !canonical.starts_with(&root) {
        return Err(CodeIntelError::PathOutsideWorkspace {
            path: requested.to_owned(),
        }
        .into());
    }
    Ok(canonical)
}

pub fn workspace_relative_path(workspace_root: &Path, path: &Path) -> String {
    let root = canonical_workspace_root(workspace_root).unwrap_or_else(|_| workspace_root.into());
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string()
}

pub fn server_for_path<'a>(
    servers: &'a [LanguageServerConfig],
    path: &Path,
) -> Option<&'a LanguageServerConfig> {
    let extension = normalized_extension(path)?;
    servers.iter().find(|server| {
        server
            .file_extensions
            .iter()
            .any(|configured| configured.trim_start_matches('.') == extension)
    })
}

pub fn language_for_path(server: &LanguageServerConfig, path: &Path) -> String {
    if server.languages.len() == 1 {
        return server.languages[0].clone();
    }
    if normalized_extension(path).as_deref() == Some("rs") {
        return "rust".to_owned();
    }
    server
        .languages
        .first()
        .cloned()
        .unwrap_or_else(|| "plaintext".to_owned())
}

fn normalized_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

fn configured_or_default_plan(config: &CodeIntelligenceConfig) -> EffectiveServerPlan {
    let servers = if config.servers.is_empty() {
        vec![default_rust_analyzer_server()]
    } else {
        config.servers.clone()
    };
    EffectiveServerPlan {
        statuses: servers
            .iter()
            .map(|server| planned_status(server, "configured"))
            .collect(),
        servers,
    }
}

pub(crate) fn effective_server_plan_from_discovered(
    config: &CodeIntelligenceConfig,
    discovered: Vec<DiscoveredLanguageServer>,
) -> EffectiveServerPlan {
    let mut plan = EffectiveServerPlan::default();
    for server in discovered {
        match server.availability {
            ServerAvailability::Installed => {
                plan.statuses
                    .push(planned_status(&server.config, "installed"));
                plan.servers.push(server.config);
            }
            ServerAvailability::Missing => {
                plan.statuses
                    .push(planned_status(&server.config, "missing"));
            }
        }
    }
    apply_configured_server_overrides(&mut plan, &config.servers);
    plan
}

fn apply_configured_server_overrides(
    plan: &mut EffectiveServerPlan,
    configured_servers: &[LanguageServerConfig],
) {
    for configured in configured_servers {
        if let Some(existing) = plan
            .servers
            .iter_mut()
            .find(|server| server.name == configured.name)
        {
            *existing = configured.clone();
        } else {
            plan.servers.push(configured.clone());
        }
        replace_status(plan, planned_status(configured, "configured"));
    }
}

fn replace_status(plan: &mut EffectiveServerPlan, next: PlannedServerStatus) {
    if let Some(existing) = plan
        .statuses
        .iter_mut()
        .find(|status| status.server == next.server)
    {
        *existing = next;
    } else {
        plan.statuses.push(next);
    }
}

fn planned_status(server: &LanguageServerConfig, status: &str) -> PlannedServerStatus {
    PlannedServerStatus {
        server: server.name.clone(),
        languages: server.languages.clone(),
        status: status.to_owned(),
    }
}

pub fn find_server_root(workspace_root: &Path, server: &LanguageServerConfig) -> Result<PathBuf> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    for marker in &server.root_markers {
        let candidate = workspace_root.join(marker);
        if candidate.exists() {
            return Ok(workspace_root.clone());
        }
    }
    Ok(workspace_root)
}

pub fn sanitize_lsp_env(configured: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "RUSTUP_HOME",
        "CARGO_HOME",
    ] {
        if let Some(value) = std::env::var_os(key) {
            env.insert(key.to_owned(), value.to_string_lossy().to_string());
        }
    }
    for (key, value) in configured {
        if !is_secret_like_key(key) {
            env.insert(key.clone(), value.clone());
        }
    }
    env
}

fn is_secret_like_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("API_KEY")
        || upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
}

pub fn safe_lsp_command(workspace_root: &Path, command: &str) -> Result<PathBuf> {
    if command.trim().is_empty() {
        bail!("language server command cannot be empty");
    }
    let command_path = PathBuf::from(command);
    if command_path.is_absolute() {
        return Ok(command_path);
    }
    if command_path.components().count() == 1 {
        return Ok(command_path);
    }
    let root = canonical_workspace_root(workspace_root)?;
    let joined = root.join(command_path);
    let normalized = lexical_normalize(&joined)?;
    if !normalized.starts_with(&root) {
        return Err(anyhow!("language server command escapes workspace"));
    }
    Ok(normalized)
}

fn lexical_normalize(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => bail!("platform path prefixes are not supported"),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
        }
    }
    Ok(normalized)
}

pub fn file_uri_from_path(path: &Path) -> String {
    let mut uri = String::from("file://");
    let path_text = path.to_string_lossy();
    if !path_text.starts_with('/') {
        uri.push('/');
    }
    uri.push_str(&percent_encode_path(&path_text));
    uri
}

pub fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode_path(raw)))
}

fn percent_encode_path(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '-' | '_' | '~' | ':') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

#[cfg(test)]
#[path = "tests/workspace_tests.rs"]
mod tests;
