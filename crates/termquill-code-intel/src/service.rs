use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use termquill_kernel::{CodeIntelligenceConfig, LanguageServerConfig};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

use crate::{
    cache::TimedCache,
    error::CodeIntelError,
    language::{rust_document_symbols, rust_syntax_diagnostics},
    lsp::{
        LspClient, definition_supported, diagnostics_supported, document_symbol_supported,
        lsp_error_to_reason, lsp_uri_to_workspace_path, position_params, references_supported,
        response_array, text_document_identifier, workspace_symbol_supported,
    },
    workspace::{
        config_enabled, effective_servers, file_uri_from_path, find_server_root, language_for_path,
        resolve_workspace_file, safe_lsp_command, sanitize_lsp_env, server_for_path,
        workspace_relative_path,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodeIntelStatus {
    Off,
    Starting {
        server: String,
    },
    Indexing {
        server: String,
        detail: Option<String>,
    },
    Ready {
        servers: usize,
    },
    Degraded {
        reason: String,
    },
    Error {
        reason: String,
    },
}

impl CodeIntelStatus {
    pub fn line(&self) -> String {
        match self {
            Self::Off => "off".to_owned(),
            Self::Starting { server } => format!("starting {server}"),
            Self::Indexing { server, detail } => match detail {
                Some(detail) => format!("indexing {server} {detail}"),
                None => format!("indexing {server}"),
            },
            Self::Ready { servers } => format!("ready {servers} server(s)"),
            Self::Degraded { reason } => format!("degraded {reason}"),
            Self::Error { reason } => format!("error {reason}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeRange {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeSymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub range: CodeRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeLocation {
    pub path: String,
    pub range: CodeRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeDiagnostic {
    pub path: String,
    pub range: CodeRange,
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QueryMetadata {
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub external_results_filtered: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeIntelServerStatus {
    pub server: String,
    pub languages: Vec<String>,
    pub status: String,
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeIntelResponse<T> {
    pub server: String,
    pub capability: String,
    pub results: Vec<T>,
    pub metadata: QueryMetadata,
    #[serde(default)]
    pub server_statuses: Vec<CodeIntelServerStatus>,
}

#[derive(Clone)]
pub struct CodeIntelligenceService {
    inner: Arc<ServiceInner>,
}

struct ServiceInner {
    workspace_root: PathBuf,
    config: CodeIntelligenceConfig,
    servers: Vec<LanguageServerConfig>,
    clients: Mutex<BTreeMap<String, ProcessLanguageServer>>,
    status: Mutex<CodeIntelStatus>,
    symbol_cache: Mutex<TimedCache<CodeIntelResponse<CodeSymbol>>>,
}

struct ProcessLanguageServer {
    config: LanguageServerConfig,
    capabilities: Value,
    child: Child,
    client: LspClient<ChildStdout, ChildStdin>,
    versions: BTreeMap<PathBuf, i32>,
}

impl Drop for ProcessLanguageServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl CodeIntelligenceService {
    pub fn new(workspace_root: PathBuf, config: CodeIntelligenceConfig) -> Self {
        let servers = effective_servers(&config);
        let status = if config_enabled(&config) {
            CodeIntelStatus::Degraded {
                reason: "lazy".to_owned(),
            }
        } else {
            CodeIntelStatus::Off
        };
        Self {
            inner: Arc::new(ServiceInner {
                workspace_root,
                config,
                servers,
                clients: Mutex::new(BTreeMap::new()),
                status: Mutex::new(status),
                symbol_cache: Mutex::new(TimedCache::new(Duration::from_secs(300))),
            }),
        }
    }

    pub fn config(&self) -> &CodeIntelligenceConfig {
        &self.inner.config
    }

    pub fn enabled(&self) -> bool {
        config_enabled(&self.inner.config)
    }

    pub async fn status(&self) -> CodeIntelStatus {
        self.inner.status.lock().await.clone()
    }

    pub async fn shutdown(&self) -> Result<()> {
        let started = Instant::now();
        let mut clients = self.inner.clients.lock().await;
        for server in clients.values_mut() {
            server.shutdown(self.request_timeout()).await;
        }
        let stopped = clients.len();
        clients.clear();
        *self.inner.status.lock().await = CodeIntelStatus::Off;
        tracing::debug!(
            stopped,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "code intelligence service shutdown complete"
        );
        Ok(())
    }

    pub fn configured_status_line(config: &CodeIntelligenceConfig) -> String {
        if !config.enabled || config.startup == termquill_kernel::CodeIntelStartup::Off {
            "off".to_owned()
        } else {
            config.startup.as_str().to_owned()
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.inner.workspace_root
    }

    pub fn resolve_file(&self, requested: &str) -> Result<PathBuf> {
        resolve_workspace_file(&self.inner.workspace_root, requested)
    }

    pub async fn document_symbols(
        &self,
        requested: &str,
        query: Option<&str>,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let started = Instant::now();
        let path = self.resolve_file(requested)?;
        let limit = self.limit(max_results);
        let cache_key = format!("symbols:{}:{query:?}:{limit}", path.display());
        if let Some(response) = self.inner.symbol_cache.lock().await.get(&cache_key) {
            return Ok(response);
        }

        match self
            .lsp_document_symbols(&path, query, limit, started)
            .await
        {
            Ok(result) => {
                self.inner
                    .symbol_cache
                    .lock()
                    .await
                    .insert(cache_key, result.clone());
                Ok(result)
            }
            Err(_) => {
                let symbols = rust_document_symbols(
                    &self.inner.workspace_root,
                    &path,
                    query,
                    limit.saturating_add(1),
                )?;
                Ok(response(
                    "tree-sitter-rust".to_owned(),
                    vec!["rust".to_owned()],
                    "tree_sitter/document_symbols".to_owned(),
                    symbols,
                    limit,
                    started,
                    0,
                ))
            }
        }
    }

    pub async fn workspace_symbols(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let started = Instant::now();
        let limit = self.limit(max_results);
        if let Ok(result) = self.lsp_workspace_symbols(query, limit, started).await {
            return Ok(result);
        }
        let mut symbols = Vec::new();
        for entry in WalkBuilder::new(&self.inner.workspace_root)
            .hidden(false)
            .git_ignore(true)
            .build()
            .filter_map(Result::ok)
        {
            if symbols.len() > limit {
                break;
            }
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("rs") {
                continue;
            }
            let mut file_symbols = rust_document_symbols(
                &self.inner.workspace_root,
                path,
                Some(query),
                limit.saturating_add(1).saturating_sub(symbols.len()),
            )?;
            symbols.append(&mut file_symbols);
        }
        Ok(response(
            "tree-sitter-rust".to_owned(),
            vec!["rust".to_owned()],
            "tree_sitter/workspace_symbols".to_owned(),
            symbols,
            limit,
            started,
            0,
        ))
    }

    pub async fn definition(
        &self,
        requested: &str,
        line: u64,
        character: u64,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeLocation>> {
        let started = Instant::now();
        let limit = self.limit(max_results);
        let path = self.resolve_file(requested)?;
        let mut clients = self.inner.clients.lock().await;
        let server = self.ensure_client_locked(&mut clients, &path).await?;
        if !definition_supported(&server.capabilities) {
            return Err(CodeIntelError::UnsupportedCapability {
                server: server.config.name.clone(),
                capability: "textDocument/definition",
            }
            .into());
        }
        server.sync_document(&path).await?;
        let value = server
            .client
            .request(
                "textDocument/definition",
                position_params(&path, line, character),
                self.request_timeout(),
            )
            .await?;
        let (locations, filtered) = self.parse_locations(response_array(value)).await?;
        Ok(response_with_filtered(
            server.config.name.clone(),
            server.config.languages.clone(),
            "textDocument/definition".to_owned(),
            locations,
            limit,
            started,
            filtered,
        ))
    }

    pub async fn references(
        &self,
        requested: &str,
        line: u64,
        character: u64,
        include_declaration: bool,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeLocation>> {
        let started = Instant::now();
        let limit = self.limit(max_results);
        let path = self.resolve_file(requested)?;
        let mut clients = self.inner.clients.lock().await;
        let server = self.ensure_client_locked(&mut clients, &path).await?;
        if !references_supported(&server.capabilities) {
            return Err(CodeIntelError::UnsupportedCapability {
                server: server.config.name.clone(),
                capability: "textDocument/references",
            }
            .into());
        }
        server.sync_document(&path).await?;
        let mut params = position_params(&path, line, character);
        params["context"] = json!({ "includeDeclaration": include_declaration });
        let value = server
            .client
            .request("textDocument/references", params, self.request_timeout())
            .await?;
        let (locations, filtered) = self.parse_locations(response_array(value)).await?;
        Ok(response_with_filtered(
            server.config.name.clone(),
            server.config.languages.clone(),
            "textDocument/references".to_owned(),
            locations,
            limit,
            started,
            filtered,
        ))
    }

    pub async fn diagnostics(
        &self,
        requested_paths: &[String],
        severity: Option<&str>,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeDiagnostic>> {
        let started = Instant::now();
        let limit = self.limit(max_results);
        let mut diagnostics = Vec::new();
        let mut server_name = "tree-sitter-rust".to_owned();
        let mut capability = "tree_sitter/diagnostics".to_owned();
        let mut server_statuses = Vec::new();

        for requested in requested_paths {
            let path = self.resolve_file(requested)?;
            match self.lsp_diagnostics_for_path(&path).await {
                Ok(result) => {
                    server_name = result.0;
                    let languages = result.1;
                    capability = result.2;
                    let count = result.3.len();
                    server_statuses.push(server_status(
                        server_name.clone(),
                        languages,
                        "ready".to_owned(),
                        count,
                        count,
                        false,
                    ));
                    diagnostics.extend(result.3);
                }
                Err(_) => {
                    let fallback = rust_syntax_diagnostics(&self.inner.workspace_root, &path)?;
                    let count = fallback.len();
                    server_statuses.push(server_status(
                        "tree-sitter-rust".to_owned(),
                        vec!["rust".to_owned()],
                        "fallback".to_owned(),
                        count,
                        count,
                        false,
                    ));
                    diagnostics.extend(fallback);
                }
            }
        }

        if let Some(filter) = severity {
            let filter = filter.to_ascii_lowercase();
            diagnostics.retain(|diagnostic| diagnostic.severity == filter);
        }

        Ok(response_with_statuses(
            server_name,
            capability,
            diagnostics,
            server_statuses,
            limit,
            started,
            0,
        ))
    }

    async fn lsp_document_symbols(
        &self,
        path: &Path,
        query: Option<&str>,
        limit: usize,
        started: Instant,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let mut clients = self.inner.clients.lock().await;
        let server = self.ensure_client_locked(&mut clients, path).await?;
        if !document_symbol_supported(&server.capabilities) {
            return Err(CodeIntelError::UnsupportedCapability {
                server: server.config.name.clone(),
                capability: "textDocument/documentSymbol",
            }
            .into());
        }
        server.sync_document(path).await?;
        let value = server
            .client
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": text_document_identifier(path) }),
                self.request_timeout(),
            )
            .await?;
        let query = query.map(str::to_ascii_lowercase);
        let mut symbols = Vec::new();
        collect_lsp_symbols(
            &value,
            &workspace_relative_path(&self.inner.workspace_root, path),
            query.as_deref(),
            &mut symbols,
        );
        Ok(response(
            server.config.name.clone(),
            server.config.languages.clone(),
            "textDocument/documentSymbol".to_owned(),
            symbols,
            limit,
            started,
            0,
        ))
    }

    async fn lsp_workspace_symbols(
        &self,
        query: &str,
        limit: usize,
        started: Instant,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let configs = self.inner.servers.clone();
        if configs.is_empty() {
            return Err(CodeIntelError::Disabled.into());
        }
        let mut clients = self.inner.clients.lock().await;
        let mut symbols = Vec::new();
        let mut filtered = 0usize;
        let mut successful_servers = Vec::new();
        let mut server_statuses = Vec::new();

        for config in configs {
            match self
                .lsp_workspace_symbols_for_server_locked(&mut clients, &config.name, query)
                .await
            {
                Ok((server_symbols, server_filtered, languages)) => {
                    let count = server_symbols.len();
                    successful_servers.push(config.name.clone());
                    server_statuses.push(server_status(
                        config.name,
                        languages,
                        "ready".to_owned(),
                        count,
                        count,
                        false,
                    ));
                    symbols.extend(server_symbols);
                    filtered = filtered.saturating_add(server_filtered);
                }
                Err(error) => {
                    server_statuses.push(server_status(
                        config.name,
                        config.languages,
                        format!("degraded {}", lsp_error_to_reason(error)),
                        0,
                        0,
                        false,
                    ));
                }
            }
        }

        if successful_servers.is_empty() {
            return Err(CodeIntelError::ServerUnavailable {
                server: "workspace/symbol".to_owned(),
                reason: "no configured language server could answer workspace/symbol".to_owned(),
            }
            .into());
        }

        let server_label = if successful_servers.len() == 1 {
            successful_servers.remove(0)
        } else {
            "multiple".to_owned()
        };
        Ok(response_with_statuses(
            server_label,
            "workspace/symbol".to_owned(),
            symbols,
            server_statuses,
            limit,
            started,
            filtered,
        ))
    }

    async fn lsp_workspace_symbols_for_server_locked(
        &self,
        clients: &mut BTreeMap<String, ProcessLanguageServer>,
        server_name: &str,
        query: &str,
    ) -> Result<(Vec<CodeSymbol>, usize, Vec<String>)> {
        let server = self
            .ensure_client_by_name_locked(clients, server_name)
            .await?;
        if !workspace_symbol_supported(&server.capabilities) {
            return Err(CodeIntelError::UnsupportedCapability {
                server: server.config.name.clone(),
                capability: "workspace/symbol",
            }
            .into());
        }
        let value = server
            .client
            .request(
                "workspace/symbol",
                json!({ "query": query }),
                self.request_timeout(),
            )
            .await?;
        let mut symbols = Vec::new();
        let mut filtered = 0usize;
        for entry in response_array(value) {
            if let Some(symbol) = self.parse_symbol_information(&entry).await {
                symbols.push(symbol);
            } else {
                filtered += 1;
            }
        }
        Ok((symbols, filtered, server.config.languages.clone()))
    }

    async fn lsp_diagnostics_for_path(
        &self,
        path: &Path,
    ) -> Result<(String, Vec<String>, String, Vec<CodeDiagnostic>)> {
        let mut clients = self.inner.clients.lock().await;
        let server = self.ensure_client_locked(&mut clients, path).await?;
        server.sync_document(path).await?;
        let uri = file_uri_from_path(path);
        let (mut raw, capability) = if diagnostics_supported(&server.capabilities) {
            let pull_result = server
                .client
                .request(
                    "textDocument/diagnostic",
                    json!({
                        "textDocument": text_document_identifier(path),
                        "identifier": null,
                        "previousResultId": null
                    }),
                    Duration::from_millis(self.inner.config.default_timeout_ms.min(1_500)),
                )
                .await
                .ok()
                .map(pull_diagnostics_from_response)
                .unwrap_or_default();
            (pull_result, "textDocument/diagnostic")
        } else {
            (Vec::new(), "textDocument/publishDiagnostics")
        };
        if raw.is_empty() {
            raw = server
                .client
                .wait_for_diagnostics(&uri, Duration::from_millis(300))
                .await?;
        }
        let diagnostics = raw
            .iter()
            .filter_map(|value| parse_diagnostic_value(&self.inner.workspace_root, path, value))
            .collect::<Vec<_>>();
        Ok((
            server.config.name.clone(),
            server.config.languages.clone(),
            capability.to_owned(),
            diagnostics,
        ))
    }

    async fn ensure_client_locked<'a>(
        &self,
        clients: &'a mut BTreeMap<String, ProcessLanguageServer>,
        path: &Path,
    ) -> Result<&'a mut ProcessLanguageServer> {
        let config = server_for_path(&self.inner.servers, path)
            .cloned()
            .ok_or_else(|| CodeIntelError::NoServerForPath {
                path: workspace_relative_path(&self.inner.workspace_root, path),
            })?;
        self.ensure_client_by_name_locked(clients, &config.name)
            .await
    }

    async fn ensure_client_by_name_locked<'a>(
        &self,
        clients: &'a mut BTreeMap<String, ProcessLanguageServer>,
        server_name: &str,
    ) -> Result<&'a mut ProcessLanguageServer> {
        if !self.enabled() {
            return Err(CodeIntelError::Disabled.into());
        }
        if !clients.contains_key(server_name) {
            let config = self
                .inner
                .servers
                .iter()
                .find(|server| server.name == server_name)
                .cloned()
                .ok_or_else(|| anyhow!("unknown language server {server_name}"))?;
            *self.inner.status.lock().await = CodeIntelStatus::Starting {
                server: config.name.clone(),
            };
            match self.start_server(config.clone()).await {
                Ok(server) => {
                    clients.insert(server_name.to_owned(), server);
                    *self.inner.status.lock().await = CodeIntelStatus::Ready {
                        servers: clients.len(),
                    };
                }
                Err(error) => {
                    let reason = lsp_error_to_reason(error);
                    *self.inner.status.lock().await = CodeIntelStatus::Degraded {
                        reason: format!("{server_name} {reason}"),
                    };
                    return Err(CodeIntelError::ServerUnavailable {
                        server: server_name.to_owned(),
                        reason,
                    }
                    .into());
                }
            }
        }
        clients
            .get_mut(server_name)
            .ok_or_else(|| anyhow!("language server {server_name} missing after startup"))
    }

    async fn start_server(&self, config: LanguageServerConfig) -> Result<ProcessLanguageServer> {
        let root = find_server_root(&self.inner.workspace_root, &config)?;
        let command_path = safe_lsp_command(&self.inner.workspace_root, &config.command)?;
        let mut command = Command::new(command_path);
        command
            .args(&config.args)
            .current_dir(&root)
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command.env_clear();
        for (key, value) in sanitize_lsp_env(&config.env) {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start language server {}", config.name))?;
        if let Some(stderr) = child.stderr.take() {
            drain_stderr(config.name.clone(), stderr);
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("language server {} stdout unavailable", config.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("language server {} stdin unavailable", config.name))?;
        let mut client = LspClient::new(stdout, stdin);
        let capabilities = client
            .initialize(
                &root,
                config.initialization_options.clone(),
                Duration::from_millis(config.startup_timeout_ms),
            )
            .await?;
        Ok(ProcessLanguageServer {
            config,
            capabilities,
            child,
            client,
            versions: BTreeMap::new(),
        })
    }

    async fn parse_locations(&self, values: Vec<Value>) -> Result<(Vec<CodeLocation>, usize)> {
        let mut locations = Vec::new();
        let mut filtered = 0usize;
        let mut seen = BTreeSet::new();
        for value in values {
            let uri = value
                .get("uri")
                .or_else(|| value.get("targetUri"))
                .and_then(Value::as_str);
            let range = value
                .get("range")
                .or_else(|| value.get("targetSelectionRange"))
                .or_else(|| value.get("targetRange"));
            let Some(uri) = uri else {
                filtered += 1;
                continue;
            };
            let Some(range) = range.and_then(parse_range) else {
                filtered += 1;
                continue;
            };
            let Some((path, canonical)) =
                lsp_uri_to_workspace_path(&self.inner.workspace_root, uri)
            else {
                filtered += 1;
                continue;
            };
            let key = format!("{}:{}:{}", path, range.start_line, range.start_character);
            if !seen.insert(key) {
                continue;
            }
            locations.push(CodeLocation {
                preview: preview_line(&canonical, range.start_line).await,
                path,
                range,
            });
        }
        Ok((locations, filtered))
    }

    async fn parse_symbol_information(&self, value: &Value) -> Option<CodeSymbol> {
        let location = value.get("location")?;
        let uri = location.get("uri")?.as_str()?;
        let (path, _) = lsp_uri_to_workspace_path(&self.inner.workspace_root, uri)?;
        Some(CodeSymbol {
            name: value.get("name")?.as_str()?.to_owned(),
            kind: lsp_symbol_kind(value.get("kind").and_then(Value::as_u64)),
            path,
            range: parse_range(location.get("range")?)?,
            container_name: value
                .get("containerName")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.config.default_timeout_ms)
    }

    fn limit(&self, requested: usize) -> usize {
        if requested == 0 {
            self.inner.config.max_results
        } else {
            requested.min(self.inner.config.max_results)
        }
    }
}

impl ProcessLanguageServer {
    async fn shutdown(&mut self, timeout: Duration) {
        let _ = self.client.shutdown(timeout).await;
        let _ = self.child.kill().await;
    }

    async fn sync_document(&mut self, path: &Path) -> Result<()> {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        let language = language_for_path(&self.config, path);
        let version = self.versions.get(path).copied().unwrap_or(0) + 1;
        if self.versions.contains_key(path) {
            self.client.did_change(path, version, text).await?;
        } else {
            self.client.did_open(path, &language, version, text).await?;
        }
        self.versions.insert(path.to_path_buf(), version);
        Ok(())
    }
}

fn drain_stderr(server: String, mut stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut buffer = [0_u8; 1024];
        let mut total = 0usize;
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => {
                    total = total.saturating_add(count);
                    if total > 64 * 1024 {
                        break;
                    }
                }
                Err(error) => {
                    tracing::debug!(server, "failed to drain lsp stderr: {error}");
                    break;
                }
            }
        }
    });
}

fn response<T>(
    server: String,
    languages: Vec<String>,
    capability: String,
    mut results: Vec<T>,
    limit: usize,
    started: Instant,
    external_filtered: usize,
) -> CodeIntelResponse<T> {
    let total = results.len();
    let truncated = total > limit;
    results.truncate(limit);
    let metadata = QueryMetadata {
        returned: total.min(limit),
        total,
        truncated,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        external_results_filtered: external_filtered,
    };
    let server_statuses = vec![server_status(
        server.clone(),
        languages,
        "ready".to_owned(),
        metadata.returned,
        metadata.total,
        metadata.truncated,
    )];
    CodeIntelResponse {
        server,
        capability,
        results,
        metadata,
        server_statuses,
    }
}

fn response_with_filtered<T>(
    server: String,
    languages: Vec<String>,
    capability: String,
    results: Vec<T>,
    limit: usize,
    started: Instant,
    external_filtered: usize,
) -> CodeIntelResponse<T> {
    response(
        server,
        languages,
        capability,
        results,
        limit,
        started,
        external_filtered,
    )
}

fn response_with_statuses<T>(
    server: String,
    capability: String,
    mut results: Vec<T>,
    mut server_statuses: Vec<CodeIntelServerStatus>,
    limit: usize,
    started: Instant,
    external_filtered: usize,
) -> CodeIntelResponse<T> {
    let total = results.len();
    let truncated = total > limit;
    results.truncate(limit);
    let metadata = QueryMetadata {
        returned: total.min(limit),
        total,
        truncated,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        external_results_filtered: external_filtered,
    };
    if server_statuses.is_empty() {
        server_statuses.push(server_status(
            server.clone(),
            Vec::new(),
            "ready".to_owned(),
            metadata.returned,
            metadata.total,
            metadata.truncated,
        ));
    }
    CodeIntelResponse {
        server,
        capability,
        results,
        metadata,
        server_statuses,
    }
}

fn server_status(
    server: String,
    languages: Vec<String>,
    status: String,
    returned: usize,
    total: usize,
    truncated: bool,
) -> CodeIntelServerStatus {
    CodeIntelServerStatus {
        server,
        languages,
        status,
        returned,
        total,
        truncated,
    }
}

fn collect_lsp_symbols(
    value: &Value,
    path: &str,
    query: Option<&str>,
    symbols: &mut Vec<CodeSymbol>,
) {
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        collect_lsp_symbol_item(item, path, query, None, symbols);
    }
}

fn collect_lsp_symbol_item(
    item: &Value,
    path: &str,
    query: Option<&str>,
    container: Option<String>,
    symbols: &mut Vec<CodeSymbol>,
) {
    let Some(name) = item.get("name").and_then(Value::as_str) else {
        return;
    };
    let matches_query = query
        .map(|needle| name.to_ascii_lowercase().contains(needle))
        .unwrap_or(true);
    if matches_query {
        let range = item
            .get("selectionRange")
            .or_else(|| item.get("range"))
            .and_then(parse_range)
            .unwrap_or(CodeRange {
                start_line: 1,
                start_character: 0,
                end_line: 1,
                end_character: 0,
            });
        symbols.push(CodeSymbol {
            name: name.to_owned(),
            kind: lsp_symbol_kind(item.get("kind").and_then(Value::as_u64)),
            path: path.to_owned(),
            range,
            container_name: container.clone(),
        });
    }
    if let Some(children) = item.get("children").and_then(Value::as_array) {
        for child in children {
            collect_lsp_symbol_item(child, path, query, Some(name.to_owned()), symbols);
        }
    }
}

fn parse_diagnostic_value(
    workspace_root: &Path,
    fallback_path: &Path,
    value: &Value,
) -> Option<CodeDiagnostic> {
    Some(CodeDiagnostic {
        path: value
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|uri| lsp_uri_to_workspace_path(workspace_root, uri).map(|item| item.0))
            .unwrap_or_else(|| workspace_relative_path(workspace_root, fallback_path)),
        range: value
            .get("range")
            .and_then(parse_range)
            .unwrap_or(CodeRange {
                start_line: 1,
                start_character: 0,
                end_line: 1,
                end_character: 0,
            }),
        severity: lsp_diagnostic_severity(value.get("severity").and_then(Value::as_u64)),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("diagnostic")
            .chars()
            .take(500)
            .collect(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn pull_diagnostics_from_response(value: Value) -> Vec<Value> {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        return items.clone();
    }
    value.as_array().cloned().unwrap_or_default()
}

fn parse_range(value: &Value) -> Option<CodeRange> {
    let start = value.get("start")?;
    let end = value.get("end")?;
    Some(CodeRange {
        start_line: start.get("line")?.as_u64()?.saturating_add(1),
        start_character: start.get("character")?.as_u64()?,
        end_line: end.get("line")?.as_u64()?.saturating_add(1),
        end_character: end.get("character")?.as_u64()?,
    })
}

fn lsp_symbol_kind(kind: Option<u64>) -> String {
    match kind {
        Some(1) => "file",
        Some(2) => "module",
        Some(3) => "namespace",
        Some(4) => "package",
        Some(5) => "class",
        Some(6) => "method",
        Some(7) => "property",
        Some(8) => "field",
        Some(9) => "constructor",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(12) => "function",
        Some(13) => "variable",
        Some(14) => "constant",
        Some(15) => "string",
        Some(16) => "number",
        Some(17) => "boolean",
        Some(18) => "array",
        Some(19) => "object",
        Some(20) => "key",
        Some(21) => "null",
        Some(22) => "enum_member",
        Some(23) => "struct",
        Some(24) => "event",
        Some(25) => "operator",
        Some(26) => "type_parameter",
        _ => "symbol",
    }
    .to_owned()
}

fn lsp_diagnostic_severity(severity: Option<u64>) -> String {
    match severity {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "information",
        Some(4) => "hint",
        _ => "unknown",
    }
    .to_owned()
}

async fn preview_line(path: &Path, line: u64) -> Option<String> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    content
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .map(|line| line.chars().take(200).collect())
}

#[cfg(test)]
#[path = "tests/service_tests.rs"]
mod tests;
