use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use termquill_kernel::{CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig};

use crate::error::CodeIntelError;

pub fn effective_servers(config: &CodeIntelligenceConfig) -> Vec<LanguageServerConfig> {
    if !config.servers.is_empty() {
        return config.servers.clone();
    }
    vec![default_rust_analyzer_server()]
}

pub fn default_rust_analyzer_server() -> LanguageServerConfig {
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

pub fn config_enabled(config: &CodeIntelligenceConfig) -> bool {
    config.enabled && config.startup != CodeIntelStartup::Off
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
