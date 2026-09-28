//! ACP identity is an opaque handle to a host-derived managed namespace, never a path.

use super::*;
use sigil_runtime::managed_storage_writer::StorageWriterChannelV1;

const SESSION_ID_PREFIX: &str = "sigil-acp-v1:";

struct SessionIdentity {
    nonce: uuid::Uuid,
    scope: String,
}

impl SessionIdentity {
    fn parse(value: &str) -> Result<Self> {
        ensure!(
            value.len() <= SESSION_ID_PREFIX.len() + 32 + 1 + 256,
            "invalid ACP session identity"
        );
        let (nonce, scope) = value
            .strip_prefix(SESSION_ID_PREFIX)
            .and_then(|value| value.split_once(':'))
            .context("unsupported ACP session identity")?;
        let parsed = uuid::Uuid::parse_str(nonce).context("invalid ACP session nonce")?;
        ensure!(
            nonce == parsed.simple().to_string(),
            "invalid ACP session nonce encoding"
        );
        sigil_application::SessionScopeId::new(scope.to_owned())?;
        Ok(Self {
            nonce: parsed,
            scope: scope.to_owned(),
        })
    }

    fn public_id(&self) -> String {
        format!("{SESSION_ID_PREFIX}{}:{}", self.nonce.simple(), self.scope)
    }
}

fn namespace_key(cwd: &Path, nonce: uuid::Uuid) -> String {
    // The existing workspace identity hashes the canonical workspace path and survives process
    // restart. Hash its variable-length slug with the host nonce into the writer's bounded key.
    let workspace = sigil_runtime::workspace_id_for_root(cwd);
    let hash = sigil_kernel::sha256_hex(format!("sigil-acp-v1\0{workspace}\0{nonce}").as_bytes());
    format!("acp-{}", &hash[..60])
}

async fn canonical_workspace(cwd: PathBuf) -> Result<PathBuf> {
    ensure!(cwd.is_absolute(), "ACP cwd must be an absolute directory");
    tokio::task::spawn_blocking(move || {
        let cwd = cwd.canonicalize().context("open ACP workspace")?;
        ensure!(cwd.is_dir(), "ACP cwd must be a directory");
        Ok(cwd)
    })
    .await?
}

fn workspace_paths(config: &Path, cwd: &Path) -> Result<sigil_runtime::SigilPaths> {
    let root = sigil_kernel::RootConfig::load(config)?;
    let configured =
        sigil_kernel::resolve_workspace_root(config, cwd, &root.workspace.root).canonicalize()?;
    ensure!(
        configured == cwd,
        "configured workspace differs from ACP cwd; use workspace.root = \".\" for client-selected workspaces"
    );
    Ok(sigil_runtime::resolve_sigil_paths(
        &root.storage,
        &root.session,
        cwd,
    ))
}

impl Adapter {
    async fn workspace_services(&self, cwd: &Path) -> Result<Arc<ApplicationRunServices>> {
        let cell = {
            let mut workspaces = self
                .workspaces
                .lock()
                .map_err(|_| anyhow!("ACP workspace cache unavailable"))?;
            workspaces.entry(cwd.to_owned()).or_default().clone()
        };
        cell.get_or_try_init(|| async {
            let config = self.config.clone();
            let cwd = cwd.to_owned();
            tokio::task::spawn_blocking(move || {
                workspace_paths(&config, &cwd)?;
                let services = ApplicationRunServices::new(Arc::new(
                    crate::egress_disclosure::CliEgressDisclosurePresenter::stderr(),
                ));
                Ok(Arc::new(
                    sigil_runtime::application_host::attach_boot_authority_to_services(
                        services, &config, &cwd,
                    )?,
                ))
            })
            .await?
        })
        .await
        .cloned()
    }

    async fn bind_session(
        &self,
        cwd: PathBuf,
        nonce: uuid::Uuid,
        expected_scope: Option<String>,
        servers: Vec<sigil_runtime::ApplicationMcpServerDeclaration>,
    ) -> Result<Arc<Session>> {
        let services = self.workspace_services(&cwd).await?;
        let writer = Arc::clone(
            &services
                .authority_composition()
                .context("ACP managed authority unavailable")?
                .storage_writer,
        );
        let config = self.config.clone();
        let bind_cwd = cwd.clone();
        let (binding, attachment, projection) = tokio::task::spawn_blocking(move || {
            let paths = workspace_paths(&config, &bind_cwd)?;
            let key = namespace_key(&bind_cwd, nonce);
            let expected_path = writer
                .managed_named_leaf_path(StorageWriterChannelV1::SessionLog, &key)?
                .join("records.jsonl");
            let requested_path = if let Some(scope) = &expected_scope {
                let root = writer.managed_leaf_path(StorageWriterChannelV1::SessionLog)?;
                let namespace = expected_path
                    .parent()
                    .and_then(Path::file_name)
                    .context("managed ACP namespace missing")?;
                let reference = sigil_kernel::SessionRef::new_relative(format!(
                    "{}.jsonl",
                    namespace.to_string_lossy()
                ))?;
                let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
                    paths.workspace_id,
                    &paths.session_log_dir,
                    &paths.session_exports_root,
                )
                .with_managed_session_log_root(root)?;
                let reopened = lifecycle.resolve_session_for_reopen(&reference, scope)?;
                ensure!(
                    reopened.session_log_path == expected_path,
                    "ACP session namespace changed"
                );
                reopened.session_log_path
            } else {
                paths.session_log_dir.join(format!("{key}.jsonl"))
            };
            let bound = bind_application_session_with_model_ref_and_projection_owner(
                &config,
                &bind_cwd,
                Some(&requested_path),
                None,
                None,
                Some(writer),
            )?;
            ensure!(
                bound.0.session_log_path == expected_path,
                "ACP session binding changed namespace"
            );
            if let Some(scope) = expected_scope {
                ensure!(
                    bound.0.session_scope_id == scope,
                    "ACP session identity changed during binding"
                );
            }
            Ok::<_, anyhow::Error>(bound)
        })
        .await??;
        let identity = SessionIdentity {
            nonce,
            scope: binding.session_scope_id,
        };
        Ok(Arc::new(Session {
            id: identity.public_id(),
            scope_id: identity.scope,
            projection,
            loading: AtomicBool::new(false),
            cwd,
            path: binding.session_log_path,
            services,
            attachment,
            servers: Mutex::new(servers),
            active: Mutex::new(None),
        }))
    }

    pub(super) async fn new_session(
        &self,
        request: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse> {
        let servers = mcp::declarations(request.mcp_servers)?;
        let cwd = canonical_workspace(request.cwd).await?;
        let session = self
            .bind_session(cwd, uuid::Uuid::new_v4(), None, servers)
            .await?;
        ensure!(
            !self.closing.load(Ordering::Acquire),
            "ACP connection closed during session creation"
        );
        self.sessions
            .lock()
            .map_err(|_| anyhow!("ACP session map unavailable"))?
            .insert(session.id.clone(), Arc::clone(&session));
        Ok(acp::NewSessionResponse::new(session.id.clone()))
    }

    pub(super) async fn load_session(
        &self,
        request: acp::LoadSessionRequest,
        client: ConnectionTo<Client>,
    ) -> Result<acp::LoadSessionResponse> {
        let identity = SessionIdentity::parse(request.session_id.0.as_ref())?;
        let servers = mcp::declarations(request.mcp_servers)?;
        let cwd = canonical_workspace(request.cwd).await?;
        let existing = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("ACP session map unavailable"))?
            .get(request.session_id.0.as_ref())
            .cloned();
        let session = if let Some(session) = existing {
            ensure!(
                session.cwd == cwd && session.scope_id == identity.scope,
                "ACP session workspace or identity mismatch"
            );
            session
        } else {
            self.bind_session(cwd, identity.nonce, Some(identity.scope), servers.clone())
                .await?
        };
        let loading = HistoryReplay::acquire(Arc::clone(&session))?;
        let config = self.config.clone();
        let check_session = Arc::clone(&session);
        tokio::task::spawn_blocking(move || {
            workspace_paths(&config, &check_session.cwd)?;
            let route = sigil_runtime::application_run::application_run_start_view(
                &config,
                &check_session.path,
                &check_session.scope_id,
                None,
            )?;
            ensure!(
                route.route_recovery.is_none(),
                "ACP session requires provider route recovery before loading"
            );
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        replay_history(&session, &client).await?;
        ensure!(
            !self.closing.load(Ordering::Acquire),
            "ACP connection closed during session loading"
        );
        *session
            .servers
            .lock()
            .map_err(|_| anyhow!("ACP MCP declarations unavailable"))? = servers;
        self.sessions
            .lock()
            .map_err(|_| anyhow!("ACP session map unavailable"))?
            .insert(session.id.clone(), Arc::clone(&session));
        drop(loading);
        Ok(acp::LoadSessionResponse::new())
    }
}

// Uses the same session admission mutex as prompts; replay cannot race a prompt or change its
// MCP declarations. Drop only releases this in-memory read admission, never execution resources.
struct HistoryReplay(Arc<Session>);
impl HistoryReplay {
    fn acquire(session: Arc<Session>) -> Result<Self> {
        let active = session
            .active
            .lock()
            .map_err(|_| anyhow!("ACP session admission unavailable"))?;
        ensure!(active.is_none(), "ACP session has an active prompt");
        ensure!(
            !session.loading.swap(true, Ordering::AcqRel),
            "ACP session history is already loading"
        );
        drop(active);
        Ok(Self(session))
    }
}
impl Drop for HistoryReplay {
    fn drop(&mut self) {
        self.0.loading.store(false, Ordering::Release);
    }
}

async fn replay_history(session: &Session, client: &ConnectionTo<Client>) -> Result<()> {
    let projection = session.projection.clone();
    let scope = session.scope_id.clone();
    // Retain only page cursors, not an unbounded copy of conversation content. The indexed
    // reader validates scope and existing stream budgets before any client notification.
    let cursors = tokio::task::spawn_blocking(move || {
        let mut cursors = Vec::new();
        let mut before = None;
        loop {
            let page = projection.transcript_page(
                &scope,
                before,
                100,
                &sigil_kernel::SessionReadBudget::default(),
            )?;
            cursors.push(before);
            match page.next_before {
                Some(next) => before = Some(next),
                None => break,
            }
        }
        Ok::<_, anyhow::Error>(cursors)
    })
    .await??;
    for before in cursors.into_iter().rev() {
        let projection = session.projection.clone();
        let scope = session.scope_id.clone();
        let page = tokio::task::spawn_blocking(move || {
            projection.transcript_page(
                &scope,
                before,
                100,
                &sigil_kernel::SessionReadBudget::default(),
            )
        })
        .await??;
        for message in page.messages {
            use sigil_runtime::application_run::ApplicationTranscriptRole;
            let mut content = message.content.unwrap_or_default();
            if message.role == ApplicationTranscriptRole::Tool {
                content = format!(
                    "Historical tool output ({}):\n{content}",
                    message.tool_name.as_deref().unwrap_or("tool")
                );
            }
            if message.truncated {
                content.push_str("\n[Stored transcript preview truncated]");
            }
            if message.image_attachment_count > 0 {
                content.push_str(
                    "\n[Stored image attachments omitted from this text-only transcript]",
                );
            }
            if content.is_empty() {
                continue;
            }
            let chunk =
                acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(content)))
                    .message_id(acp::MessageId::new(message.message_id));
            let update = match message.role {
                ApplicationTranscriptRole::User => acp::SessionUpdate::UserMessageChunk(chunk),
                ApplicationTranscriptRole::Assistant
                    if message.assistant_kind
                        == Some(sigil_kernel::AssistantMessageKind::ReasoningTrace) =>
                {
                    acp::SessionUpdate::AgentThoughtChunk(chunk)
                }
                ApplicationTranscriptRole::Assistant | ApplicationTranscriptRole::Tool => {
                    acp::SessionUpdate::AgentMessageChunk(chunk)
                }
            };
            client
                .send_notification(acp::SessionNotification::new(session.id.clone(), update))
                .map_err(|error| anyhow!(error.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/session_tests.rs"]
mod tests;
