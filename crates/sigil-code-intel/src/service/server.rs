use super::*;

pub(super) type LanguageServerHandle = Arc<Mutex<Option<ProcessLanguageServer>>>;

#[derive(Clone, Default)]
pub(super) struct ServerPlanState {
    pub(super) servers: Vec<LanguageServerConfig>,
    pub(super) discovery_statuses: Vec<CodeIntelServerStatus>,
    pub(super) discovery_loaded: bool,
}

pub(super) struct ProcessLanguageServer {
    pub(super) config: LanguageServerConfig,
    pub(super) capabilities: Value,
    pub(super) child: Child,
    pub(super) client: LspClient<ChildStdout, ChildStdin>,
    pub(super) versions: BTreeMap<PathBuf, i32>,
}

pub(super) struct LspRequestOutput {
    pub(super) server_name: String,
    pub(super) languages: Vec<String>,
    pub(super) source_path: PathBuf,
    pub(super) source_version: i32,
    pub(super) source_hash: String,
    pub(super) value: Value,
}

impl Drop for ProcessLanguageServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub(super) fn initial_server_plan(
    config: &CodeIntelligenceConfig,
    workspace_root: &Path,
) -> ServerPlanState {
    if config.server_startup == CodeIntelStartup::Lazy && config.auto_discover {
        return ServerPlanState {
            servers: config.servers.clone(),
            discovery_statuses: config
                .servers
                .iter()
                .map(|server| {
                    server_status(
                        server.name.clone(),
                        server.languages.clone(),
                        "configured".to_owned(),
                        0,
                        0,
                        false,
                    )
                })
                .collect(),
            discovery_loaded: false,
        };
    }
    server_plan_state_from_effective(effective_server_plan(config, workspace_root), true)
}

pub(super) fn server_plan_state_from_effective(
    plan: EffectiveServerPlan,
    discovery_loaded: bool,
) -> ServerPlanState {
    ServerPlanState {
        servers: plan.servers,
        discovery_statuses: plan
            .statuses
            .into_iter()
            .map(|status| {
                server_status(status.server, status.languages, status.status, 0, 0, false)
            })
            .collect(),
        discovery_loaded,
    }
}

impl ProcessLanguageServer {
    #[cfg(test)]
    pub(super) async fn shutdown(&mut self, timeout: Duration) {
        let _ = self.client.shutdown(timeout).await;
        let _ = self.child.kill().await;
    }

    pub(super) async fn sync_document(&mut self, path: &Path) -> Result<(i32, String)> {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        let language = language_for_path(&self.config, path);
        let version = self.versions.get(path).copied().unwrap_or(0) + 1;
        let content_hash = sigil_kernel::bytes_hash(text.as_bytes());
        if self.versions.contains_key(path) {
            self.client.did_change(path, version, text).await?;
        } else {
            self.client.did_open(path, &language, version, text).await?;
        }
        self.versions.insert(path.to_path_buf(), version);
        Ok((version, content_hash))
    }
}

pub(super) fn language_server_mut<'a>(
    slot: &'a mut Option<ProcessLanguageServer>,
    capability: &str,
) -> Result<&'a mut ProcessLanguageServer> {
    slot.as_mut()
        .ok_or_else(|| anyhow!("language server unavailable while handling {capability}"))
}

pub(super) fn drain_stderr(server: String, mut stderr: ChildStderr) {
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
