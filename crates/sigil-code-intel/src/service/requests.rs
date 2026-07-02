use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeEditPlan {
    pub server: String,
    pub capability: String,
    pub edit: CodeWorkspaceEdit,
    pub metadata: QueryMetadata,
}

#[derive(Clone)]
pub struct CodeIntelligenceService {
    pub(super) inner: Arc<ServiceInner>,
}

pub(super) struct ServiceInner {
    pub(super) workspace_root: PathBuf,
    pub(super) config: CodeIntelligenceConfig,
    pub(super) server_plan: RwLock<ServerPlanState>,
    pub(super) clients: Mutex<BTreeMap<String, LanguageServerHandle>>,
    pub(super) status: Mutex<CodeIntelStatus>,
    pub(super) symbol_cache: Mutex<TimedCache<CodeIntelResponse<CodeSymbol>>>,
}

impl CodeIntelligenceService {
    pub fn new(workspace_root: PathBuf, config: CodeIntelligenceConfig) -> Self {
        let server_plan = if config_enabled(&config) {
            initial_server_plan(&config, &workspace_root)
        } else {
            Default::default()
        };
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
                server_plan: RwLock::new(server_plan),
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
        let handles = {
            let mut clients = self.inner.clients.lock().await;
            let handles = clients.values().cloned().collect::<Vec<_>>();
            clients.clear();
            handles
        };
        let stopped = handles.len();
        for handle in handles {
            if let Some(server) = handle.lock().await.as_mut() {
                server.shutdown(self.request_timeout()).await;
            }
        }
        *self.inner.status.lock().await = CodeIntelStatus::Off;
        tracing::debug!(
            stopped,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "code intelligence service shutdown complete"
        );
        Ok(())
    }

    pub fn configured_status_line(config: &CodeIntelligenceConfig) -> String {
        if !config.enabled || config.startup == sigil_kernel::CodeIntelStartup::Off {
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

    async fn ensure_server_plan(&self) {
        if !config_enabled(&self.inner.config) || !self.inner.config.discovery.enabled {
            return;
        }
        if self.server_plan_snapshot().discovery_loaded {
            return;
        }

        let config = self.inner.config.clone();
        let workspace_root = self.inner.workspace_root.clone();
        let next_plan = match tokio::task::spawn_blocking(move || {
            effective_server_plan(&config, &workspace_root)
        })
        .await
        {
            Ok(plan) => server_plan_state_from_effective(plan, true),
            Err(error) => ServerPlanState {
                servers: Vec::new(),
                discovery_statuses: vec![server_status(
                    "discovery".to_owned(),
                    Vec::new(),
                    format!("degraded {error}"),
                    0,
                    0,
                    false,
                )],
                discovery_loaded: true,
            },
        };

        let mut state = match self.inner.server_plan.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !state.discovery_loaded {
            *state = next_plan;
        }
    }

    pub(super) fn server_plan_snapshot(&self) -> ServerPlanState {
        match self.inner.server_plan.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn current_servers(&self) -> Vec<LanguageServerConfig> {
        self.server_plan_snapshot().servers
    }

    fn with_discovery_statuses<T>(
        &self,
        mut response: CodeIntelResponse<T>,
    ) -> CodeIntelResponse<T> {
        response.server_statuses =
            self.merge_discovery_statuses(std::mem::take(&mut response.server_statuses));
        response
    }

    fn merge_discovery_statuses(
        &self,
        response_statuses: Vec<CodeIntelServerStatus>,
    ) -> Vec<CodeIntelServerStatus> {
        let mut statuses = BTreeMap::new();
        for status in self.server_plan_snapshot().discovery_statuses {
            statuses.insert(status.server.clone(), status);
        }
        for status in response_statuses {
            statuses.insert(status.server.clone(), status);
        }
        statuses.into_values().collect()
    }

    pub async fn document_symbols(
        &self,
        requested: &str,
        query: Option<&str>,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let started = Instant::now();
        let path = self.resolve_file(requested)?;
        self.ensure_server_plan().await;
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
            Err(error) => {
                let symbols = rust_document_symbols(
                    &self.inner.workspace_root,
                    &path,
                    query,
                    limit.saturating_add(1),
                )?;
                let mut server_statuses = Vec::new();
                if let Some(server) = server_for_path(&self.current_servers(), &path) {
                    server_statuses.push(server_status(
                        server.name.clone(),
                        server.languages.clone(),
                        format!("degraded {}", lsp_error_to_reason(error)),
                        0,
                        0,
                        false,
                    ));
                }
                let count = symbols.len();
                server_statuses.push(server_status(
                    "tree-sitter-rust".to_owned(),
                    vec!["rust".to_owned()],
                    "fallback".to_owned(),
                    count.min(limit),
                    count,
                    count > limit,
                ));
                Ok(self.with_discovery_statuses(response_with_statuses(
                    "tree-sitter-rust".to_owned(),
                    "tree_sitter/document_symbols".to_owned(),
                    symbols,
                    server_statuses,
                    limit,
                    started,
                    0,
                )))
            }
        }
    }

    pub async fn workspace_symbols(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let started = Instant::now();
        self.ensure_server_plan().await;
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
        let mut server_statuses = self
            .current_servers()
            .iter()
            .map(|server| {
                server_status(
                    server.name.clone(),
                    server.languages.clone(),
                    "degraded workspace/symbol unavailable".to_owned(),
                    0,
                    0,
                    false,
                )
            })
            .collect::<Vec<_>>();
        let count = symbols.len();
        server_statuses.push(server_status(
            "tree-sitter-rust".to_owned(),
            vec!["rust".to_owned()],
            "fallback".to_owned(),
            count.min(limit),
            count,
            count > limit,
        ));
        Ok(self.with_discovery_statuses(response_with_statuses(
            "tree-sitter-rust".to_owned(),
            "tree_sitter/workspace_symbols".to_owned(),
            symbols,
            server_statuses,
            limit,
            started,
            0,
        )))
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
        self.ensure_server_plan().await;
        let response = self
            .synced_lsp_request(
                &path,
                "textDocument/definition",
                definition_supported,
                position_params(&path, line, character),
            )
            .await?;
        let (locations, filtered) = self.parse_locations(response_array(response.value)).await?;
        Ok(self.with_discovery_statuses(response_with_filtered(
            response.server_name,
            response.languages,
            "textDocument/definition".to_owned(),
            locations,
            limit,
            started,
            filtered,
        )))
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
        self.ensure_server_plan().await;
        let mut params = position_params(&path, line, character);
        params["context"] = json!({ "includeDeclaration": include_declaration });
        let response = self
            .synced_lsp_request(
                &path,
                "textDocument/references",
                references_supported,
                params,
            )
            .await?;
        let (locations, filtered) = self.parse_locations(response_array(response.value)).await?;
        Ok(self.with_discovery_statuses(response_with_filtered(
            response.server_name,
            response.languages,
            "textDocument/references".to_owned(),
            locations,
            limit,
            started,
            filtered,
        )))
    }

    pub async fn code_actions(
        &self,
        requested: &str,
        line: u64,
        character: u64,
        end_line: Option<u64>,
        end_character: Option<u64>,
        only: Option<&str>,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeActionSummary>> {
        let started = Instant::now();
        let limit = self.limit(max_results);
        let path = self.resolve_file(requested)?;
        self.ensure_server_plan().await;
        let response = self
            .synced_lsp_request(
                &path,
                "textDocument/codeAction",
                code_action_supported,
                code_action_params(&path, line, character, end_line, end_character, only),
            )
            .await?;
        let mut filtered = 0usize;
        let mut actions = response_array(response.value)
            .into_iter()
            .filter_map(|value| {
                parse_code_action_summary(&value).or_else(|| {
                    filtered += 1;
                    None
                })
            })
            .collect::<Vec<_>>();
        if let Some(only) = only {
            actions.retain(|action| {
                action
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind == only || kind.starts_with(&format!("{only}.")))
            });
        }
        Ok(self.with_discovery_statuses(response_with_filtered(
            response.server_name,
            response.languages,
            "textDocument/codeAction".to_owned(),
            actions,
            limit,
            started,
            filtered,
        )))
    }

    pub async fn code_action_edit_plan(
        &self,
        requested: &str,
        line: u64,
        character: u64,
        end_line: Option<u64>,
        end_character: Option<u64>,
        title: Option<&str>,
        kind: Option<&str>,
    ) -> Result<CodeEditPlan> {
        let started = Instant::now();
        let path = self.resolve_file(requested)?;
        self.ensure_server_plan().await;
        let response = self
            .synced_lsp_request(
                &path,
                "textDocument/codeAction",
                code_action_supported,
                code_action_params(&path, line, character, end_line, end_character, kind),
            )
            .await?;
        let actions = response_array(response.value);
        let mut selected = select_code_action(actions, title, kind)?;
        if selected.get("edit").is_none()
            && self
                .lsp_capability_supported(&path, code_action_resolve_supported)
                .await?
        {
            selected = self
                .synced_lsp_request(
                    &path,
                    "codeAction/resolve",
                    code_action_resolve_supported,
                    selected.clone(),
                )
                .await?
                .value;
        }
        let edit_value =
            selected
                .get("edit")
                .ok_or_else(|| CodeIntelError::UnsupportedCapability {
                    server: response.server_name.clone(),
                    capability: "codeAction/edit",
                })?;
        let edit = workspace_edit_from_lsp(&self.inner.workspace_root, edit_value)?;
        if edit.files.is_empty() {
            return Err(anyhow!("selected code action produced no workspace edits"));
        }
        Ok(CodeEditPlan {
            server: response.server_name,
            capability: "textDocument/codeAction".to_owned(),
            metadata: QueryMetadata {
                returned: edit.files.len(),
                total: edit.files.len(),
                truncated: false,
                elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                external_results_filtered: edit.external_changes_filtered,
            },
            edit,
        })
    }

    pub async fn rename_edit_plan(
        &self,
        requested: &str,
        line: u64,
        character: u64,
        new_name: &str,
    ) -> Result<CodeEditPlan> {
        let started = Instant::now();
        let path = self.resolve_file(requested)?;
        self.ensure_server_plan().await;
        let mut params = position_params(&path, line, character);
        params["newName"] = json!(new_name);
        let response = self
            .synced_lsp_request(&path, "textDocument/rename", rename_supported, params)
            .await?;
        let edit = workspace_edit_from_lsp(&self.inner.workspace_root, &response.value)?;
        if edit.files.is_empty() {
            return Err(anyhow!("rename produced no workspace edits"));
        }
        Ok(CodeEditPlan {
            server: response.server_name,
            capability: "textDocument/rename".to_owned(),
            metadata: QueryMetadata {
                returned: edit.files.len(),
                total: edit.files.len(),
                truncated: false,
                elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                external_results_filtered: edit.external_changes_filtered,
            },
            edit,
        })
    }

    pub async fn diagnostics(
        &self,
        requested_paths: &[String],
        severity: Option<&str>,
        max_results: usize,
    ) -> Result<CodeIntelResponse<CodeDiagnostic>> {
        let started = Instant::now();
        self.ensure_server_plan().await;
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
                Err(error) => {
                    if let Some(server) = server_for_path(&self.current_servers(), &path) {
                        server_statuses.push(server_status(
                            server.name.clone(),
                            server.languages.clone(),
                            format!("degraded {}", lsp_error_to_reason(error)),
                            0,
                            0,
                            false,
                        ));
                    }
                    if !is_rust_source_path(&path) {
                        if diagnostics.is_empty() {
                            server_name = "code-intel".to_owned();
                            capability = "diagnostics/unsupported".to_owned();
                        }
                        server_statuses.push(server_status(
                            "code-intel".to_owned(),
                            Vec::new(),
                            format!(
                                "unsupported {}",
                                workspace_relative_path(&self.inner.workspace_root, &path)
                            ),
                            0,
                            0,
                            false,
                        ));
                        continue;
                    }
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

        Ok(self.with_discovery_statuses(response_with_statuses(
            server_name,
            capability,
            diagnostics,
            server_statuses,
            limit,
            started,
            0,
        )))
    }

    pub(super) async fn lsp_document_symbols(
        &self,
        path: &Path,
        query: Option<&str>,
        limit: usize,
        started: Instant,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let lsp_response = self
            .synced_lsp_request(
                path,
                "textDocument/documentSymbol",
                document_symbol_supported,
                json!({ "textDocument": text_document_identifier(path) }),
            )
            .await?;
        let query = query.map(str::to_ascii_lowercase);
        let mut symbols = Vec::new();
        collect_lsp_symbols(
            &lsp_response.value,
            &workspace_relative_path(&self.inner.workspace_root, path),
            query.as_deref(),
            &mut symbols,
        );
        Ok(self.with_discovery_statuses(response(
            lsp_response.server_name,
            lsp_response.languages,
            "textDocument/documentSymbol".to_owned(),
            symbols,
            limit,
            started,
            0,
        )))
    }

    pub(super) async fn lsp_workspace_symbols(
        &self,
        query: &str,
        limit: usize,
        started: Instant,
    ) -> Result<CodeIntelResponse<CodeSymbol>> {
        let configs = self.current_servers();
        if configs.is_empty() {
            return Err(CodeIntelError::Disabled.into());
        }
        let mut symbols = Vec::new();
        let mut filtered = 0usize;
        let mut successful_servers = Vec::new();
        let mut server_statuses = Vec::new();

        for config in configs {
            match self
                .lsp_workspace_symbols_for_server(&config.name, query)
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
        Ok(self.with_discovery_statuses(response_with_statuses(
            server_label,
            "workspace/symbol".to_owned(),
            symbols,
            server_statuses,
            limit,
            started,
            filtered,
        )))
    }

    pub(super) async fn lsp_workspace_symbols_for_server(
        &self,
        server_name: &str,
        query: &str,
    ) -> Result<(Vec<CodeSymbol>, usize, Vec<String>)> {
        let server_handle = self.ensure_client_by_name(server_name).await?;
        let mut server_guard = server_handle.lock().await;
        let server = language_server_mut(&mut server_guard, "workspace/symbol")?;
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
        let server_handle = self.ensure_client(path).await?;
        let mut server_guard = server_handle.lock().await;
        let server = language_server_mut(&mut server_guard, "textDocument/diagnostic")?;
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

    async fn synced_lsp_request(
        &self,
        path: &Path,
        capability: &'static str,
        supported: fn(&Value) -> bool,
        params: Value,
    ) -> Result<LspRequestOutput> {
        let server_handle = self.ensure_client(path).await?;
        let mut server_guard = server_handle.lock().await;
        let server = language_server_mut(&mut server_guard, capability)?;
        if !supported(&server.capabilities) {
            return Err(CodeIntelError::UnsupportedCapability {
                server: server.config.name.clone(),
                capability,
            }
            .into());
        }
        server.sync_document(path).await?;
        let value = server
            .client
            .request(capability, params, self.request_timeout())
            .await?;
        Ok(LspRequestOutput {
            server_name: server.config.name.clone(),
            languages: server.config.languages.clone(),
            value,
        })
    }

    async fn lsp_capability_supported(
        &self,
        path: &Path,
        supported: fn(&Value) -> bool,
    ) -> Result<bool> {
        let server_handle = self.ensure_client(path).await?;
        let mut server_guard = server_handle.lock().await;
        let server = language_server_mut(&mut server_guard, "capability check")?;
        Ok(supported(&server.capabilities))
    }

    pub(super) async fn ensure_client(&self, path: &Path) -> Result<LanguageServerHandle> {
        let config = server_for_path(&self.current_servers(), path)
            .cloned()
            .ok_or_else(|| CodeIntelError::NoServerForPath {
                path: workspace_relative_path(&self.inner.workspace_root, path),
            })?;
        self.ensure_client_by_name(&config.name).await
    }

    pub(super) async fn ensure_client_by_name(
        &self,
        server_name: &str,
    ) -> Result<LanguageServerHandle> {
        if !self.enabled() {
            return Err(CodeIntelError::Disabled.into());
        }
        if let Some(handle) = self.inner.clients.lock().await.get(server_name).cloned() {
            return Ok(handle);
        }

        let config = self
            .server_plan_snapshot()
            .servers
            .iter()
            .find(|server| server.name == server_name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown language server {server_name}"))?;
        let handle = {
            let mut clients = self.inner.clients.lock().await;
            clients
                .entry(server_name.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut server_slot = handle.lock().await;
        if server_slot.is_some() {
            drop(server_slot);
            return Ok(handle);
        }

        *self.inner.status.lock().await = CodeIntelStatus::Starting {
            server: config.name.clone(),
        };
        match self.start_server(config.clone()).await {
            Ok(server) => {
                *server_slot = Some(server);
                drop(server_slot);
                let servers = self.inner.clients.lock().await.len();
                *self.inner.status.lock().await = CodeIntelStatus::Ready { servers };
                Ok(handle)
            }
            Err(error) => {
                drop(server_slot);
                self.remove_client_handle(server_name, &handle).await;
                let reason = lsp_error_to_reason(error);
                *self.inner.status.lock().await = CodeIntelStatus::Degraded {
                    reason: format!("{server_name} {reason}"),
                };
                Err(CodeIntelError::ServerUnavailable {
                    server: server_name.to_owned(),
                    reason,
                }
                .into())
            }
        }
    }

    async fn remove_client_handle(&self, server_name: &str, handle: &LanguageServerHandle) {
        let mut clients = self.inner.clients.lock().await;
        if clients
            .get(server_name)
            .is_some_and(|existing| Arc::ptr_eq(existing, handle))
        {
            clients.remove(server_name);
        }
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

    pub(super) async fn parse_locations(
        &self,
        values: Vec<Value>,
    ) -> Result<(Vec<CodeLocation>, usize)> {
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

    pub(super) async fn parse_symbol_information(&self, value: &Value) -> Option<CodeSymbol> {
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

    pub(super) fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.config.default_timeout_ms)
    }

    pub(super) fn limit(&self, requested: usize) -> usize {
        if requested == 0 {
            self.inner.config.max_results
        } else {
            requested.min(self.inner.config.max_results)
        }
    }
}
