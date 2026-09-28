//! Explicit third-party MCP configuration conversion. Parsing never launches or publishes.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sigil_kernel::{McpServerConfig, SecretRedactor};

const MAX_IMPORT_BYTES: usize = 1024 * 1024;
const MAX_PREVIEW_TEXT_BYTES: usize = 512;

/// Reads a file explicitly selected by the user for preview, without publishing or launching.
/// Symlink targets are allowed; the opened descriptor must be a bounded regular file.
///
/// # Errors
///
/// Rejects unavailable or non-regular files, input above the existing document budget, and
/// invalid import documents. No credential value or source path is included in the error.
pub fn preview_mcp_configuration_import_file(
    path: &std::path::Path,
) -> Result<McpConfigurationImport> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| anyhow!("MCP import file is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("MCP import file is unavailable"))?;
    ensure!(metadata.is_file(), "MCP import requires a regular file");
    ensure!(
        metadata.len() <= MAX_IMPORT_BYTES as u64,
        "MCP import exceeds the 1 MiB document resource budget"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_IMPORT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("MCP import file could not be read"))?;
    preview_mcp_configuration_import(&bytes)
}

/// Safe display-only description. The host retains raw configurations separately.
#[derive(Debug, Clone, Serialize)]
pub struct McpImportCandidateSummary {
    pub index: usize,
    pub name: String,
    pub transport: Option<String>,
    pub description: String,
    pub importable: bool,
    pub issues: Vec<McpImportIssue>,
}

/// Closed diagnostics never echo source values, command arguments, URLs or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpImportIssue {
    InvalidServerObject,
    InvalidConfiguration,
    AmbiguousTransport,
    UnsupportedTransport,
    IgnoredFields,
    EnvironmentValuesNotImported,
    HeaderValuesNotImported,
    CommandArgumentsWillBeSaved,
}

/// Host-owned conversion, deliberately not serializable or Debug-printable.
/// Publication and the config-generation CAS remain the caller's explicit operation.
pub struct McpConfigurationImport {
    summaries: Vec<McpImportCandidateSummary>,
    candidates: Vec<Option<McpServerConfig>>,
    root_fields_ignored: bool,
}

impl McpConfigurationImport {
    #[must_use]
    pub fn summaries(&self) -> &[McpImportCandidateSummary] {
        &self.summaries
    }

    #[must_use]
    pub fn root_fields_ignored(&self) -> bool {
        self.root_fields_ignored
    }

    /// Builds a new configuration list after exact user selection. Existing entries are never
    /// silently replaced; callers must publish against their original config revision.
    pub fn selected_configurations(
        &self,
        selected_indices: &[usize],
        existing: &[McpServerConfig],
    ) -> Result<Vec<McpServerConfig>> {
        let mut selected = BTreeSet::new();
        let mut names = existing
            .iter()
            .map(|server| server.name.as_str())
            .collect::<BTreeSet<_>>();
        let mut result = existing.to_vec();
        for &index in selected_indices {
            ensure!(
                selected.insert(index),
                "MCP import selection contains a duplicate index"
            );
            let candidate = self
                .candidates
                .get(index)
                .and_then(Option::as_ref)
                .ok_or_else(|| anyhow!("MCP import selection is unavailable or invalid"))?;
            ensure!(
                names.insert(&candidate.name),
                "MCP import selection conflicts with an existing server name; rename or remove that entry explicitly"
            );
            result.push(candidate.clone());
        }
        ensure!(
            result.len() <= crate::mcp_declaration::MAX_MCP_SERVER_DECLARATIONS,
            "MCP import exceeds the configured server resource budget"
        );
        Ok(result)
    }
}

/// Converts an explicitly supplied `mcpServers` object to current typed configuration candidates.
/// Inline environment/header values are omitted with diagnostics. Transport and required fields
/// are validated by the same `McpServerConfig` deserializer as normal configuration loading.
pub fn preview_mcp_configuration_import(bytes: &[u8]) -> Result<McpConfigurationImport> {
    ensure!(
        bytes.len() <= MAX_IMPORT_BYTES,
        "MCP import exceeds the 1 MiB document resource budget"
    );
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow!("MCP import must be a valid JSON object"))?;
    let root = document
        .as_object()
        .ok_or_else(|| anyhow!("MCP import must be a JSON object"))?;
    let servers = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("MCP import requires an explicit mcpServers object"))?;
    ensure!(
        servers.len() <= crate::mcp_declaration::MAX_MCP_SERVER_DECLARATIONS,
        "MCP import exceeds the configured server resource budget"
    );
    let mut summaries = Vec::with_capacity(servers.len());
    let mut candidates = Vec::with_capacity(servers.len());
    for (index, (name, source)) in servers.iter().enumerate() {
        let (candidate, issues) = convert_server(name, source);
        summaries.push(McpImportCandidateSummary {
            index,
            name: preview_text(name),
            description: candidate
                .as_ref()
                .map_or_else(String::new, |server| preview_text(&server.description)),
            transport: candidate
                .as_ref()
                .map(|server| server.transport_name().to_owned()),
            importable: candidate.is_some(),
            issues,
        });
        candidates.push(candidate);
    }
    Ok(McpConfigurationImport {
        summaries,
        candidates,
        root_fields_ignored: root.keys().any(|key| key != "mcpServers"),
    })
}

fn convert_server(name: &str, source: &Value) -> (Option<McpServerConfig>, Vec<McpImportIssue>) {
    let Some(source) = source.as_object() else {
        return (None, vec![McpImportIssue::InvalidServerObject]);
    };
    let mut issues = Vec::new();
    if source.keys().any(|key| {
        !matches!(
            key.as_str(),
            "command" | "args" | "url" | "type" | "transport" | "description" | "env" | "headers"
        )
    }) {
        issues.push(McpImportIssue::IgnoredFields);
    }
    if source.contains_key("env") {
        issues.push(McpImportIssue::EnvironmentValuesNotImported);
    }
    if source.contains_key("headers") {
        issues.push(McpImportIssue::HeaderValuesNotImported);
    }
    let result = (|| -> Result<McpServerConfig> {
        if source.contains_key("command") && source.contains_key("url") {
            issues.push(McpImportIssue::AmbiguousTransport);
            bail!("ambiguous transport");
        }
        let transport = if source.contains_key("url") {
            "streamable_http"
        } else {
            "stdio"
        };
        for field in ["type", "transport"] {
            if let Some(declared) = source.get(field) {
                let supported = matches!(
                    (transport, declared.as_str()),
                    ("stdio", Some("stdio"))
                        | (
                            "streamable_http",
                            Some("http" | "streamable-http" | "streamable_http")
                        )
                );
                if !supported {
                    issues.push(McpImportIssue::UnsupportedTransport);
                    bail!("unsupported transport");
                }
            }
        }
        let mut target = json!({
            "name": name,
            "description": source.get("description").cloned().unwrap_or_else(|| json!("")),
            "transport": transport,
            "startup": "lazy",
            "required": false,
            "trust": {"trust_class":"third_party", "approval_default":"ask"},
        });
        if transport == "stdio" {
            target["command"] = source.get("command").cloned().unwrap_or(Value::Null);
            target["args"] = source.get("args").cloned().unwrap_or_else(|| json!([]));
            issues.push(McpImportIssue::CommandArgumentsWillBeSaved);
        } else {
            target["url"] = source.get("url").cloned().unwrap_or(Value::Null);
        }
        serde_json::from_value(target).map_err(|_| anyhow!("invalid imported MCP configuration"))
    })();
    match result {
        Ok(config) => (Some(config), issues),
        Err(_) => {
            if !issues.iter().any(|issue| {
                matches!(
                    issue,
                    McpImportIssue::AmbiguousTransport | McpImportIssue::UnsupportedTransport
                )
            }) {
                issues.push(McpImportIssue::InvalidConfiguration);
            }
            (None, issues)
        }
    }
}

fn preview_text(text: &str) -> String {
    let redacted = SecretRedactor::default().redact_text(text);
    let mut end = redacted.len().min(MAX_PREVIEW_TEXT_BYTES);
    while !redacted.is_char_boundary(end) {
        end -= 1;
    }
    redacted[..end].to_owned()
}

#[cfg(test)]
#[path = "tests/mcp_import_tests.rs"]
mod tests;
