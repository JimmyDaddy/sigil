//! Thin ACP transport: shared application runs retain session, approval and process authority.

mod approval;
mod events;
mod mcp;
mod sessions;

use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use agent_client_protocol::{Agent, Client, ConnectionTo, Stdio, schema::v1 as acp};
use anyhow::{Context, Result, anyhow, ensure};
use sigil_runtime::{
    application_run::{
        ApplicationRunControl, ApplicationRunInteraction, ApplicationRunRequest,
        ApplicationRunServices, ApplicationRunTerminalStatus,
        bind_application_session_with_model_ref_and_projection_owner, prepare_application_run,
    },
    interactive_session_attachment::InteractiveSessionAttachmentLease,
};
use tokio::sync::{OnceCell, watch};

type WorkspaceServices = Arc<OnceCell<Arc<ApplicationRunServices>>>;

struct Session {
    id: String,
    scope_id: String,
    projection: sigil_runtime::RuntimeSessionProjectionOwner,
    loading: AtomicBool,
    cwd: PathBuf,
    path: PathBuf,
    services: Arc<ApplicationRunServices>,
    attachment: Arc<InteractiveSessionAttachmentLease>,
    servers: Mutex<Vec<sigil_runtime::ApplicationMcpServerDeclaration>>,
    active: Mutex<Option<Arc<Run>>>,
}

struct Run {
    cancel: watch::Sender<bool>,
    control: Mutex<Option<Arc<ApplicationRunControl>>>,
    cancellation: Mutex<Cancellation>,
}

#[derive(Default)]
struct Cancellation {
    execution_joined: bool,
    requested: bool,
    ticket: Option<sigil_runtime::application_run::ApplicationCancellationTicket>,
    error: Option<String>,
}

impl Run {
    fn new() -> Self {
        let (cancel, _) = watch::channel(false);
        Self {
            cancel,
            control: Mutex::new(None),
            cancellation: Mutex::new(Cancellation::default()),
        }
    }

    fn cancel(&self) {
        self.cancel.send_replace(true);
        // Serialize the durable request with finalization before borrowing its control. A late
        // cancellation may retain Run, but must never retain a completed foreground lease.
        let mut cancellation = self
            .cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancellation.requested || cancellation.execution_joined {
            return;
        }
        let control = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(control) = control {
            cancellation.requested = true;
            match control.request_cancellation("ACP client cancelled the turn", None, || {}) {
                Ok(ticket) => cancellation.ticket = Some(ticket),
                Err(error) => {
                    cancellation.error = Some(error.to_string());
                    cancellation.ticket = error.into_ticket();
                }
            }
        }
    }

    async fn bind(self: &Arc<Self>, control: ApplicationRunControl) -> Result<()> {
        let control = Arc::new(control);
        *self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(control);
        if *self.cancel.borrow() {
            let run = Arc::clone(self);
            tokio::task::spawn_blocking(move || run.cancel()).await?;
        }
        Ok(())
    }

    async fn finalize_cancellation(
        self: &Arc<Self>,
        handler: events::EventHandler,
    ) -> Result<Option<sigil_kernel::RunCancellationTerminalOutcome>> {
        let run = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            // Also waits for a cancellation request that was admitted immediately before the
            // execution joined. A requested bit alone is never a completion receipt.
            let (ticket, error, control) = {
                let mut state = run
                    .cancellation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *run.cancel.borrow()
                    && !state.requested
                    && let Some(control) = run
                        .control
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                {
                    state.requested = true;
                    match control.request_cancellation("ACP client cancelled the turn", None, || {})
                    {
                        Ok(ticket) => state.ticket = Some(ticket),
                        Err(error) => {
                            state.error = Some(error.to_string());
                            state.ticket = error.into_ticket();
                        }
                    }
                }
                state.execution_joined = true;
                // Execution has actually joined and any admitted cancellation has supplied its
                // ticket. Keep the lease only through this owned settlement, not in late Run Arcs.
                let control = run
                    .control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                (state.ticket.take(), state.error.take(), control)
            };
            let Some(ticket) = ticket else {
                return error.map_or(Ok(None), |error| Err(anyhow!(error)));
            };
            let control = control.context("ACP cancellation control unavailable")?;
            let mut handler = handler;
            let result = tokio::runtime::Handle::current().block_on(control.finalize_cancellation(
                ticket,
                true,
                &mut handler,
            ));
            match (result, error) {
                (Ok(outcome), None) => Ok(Some(outcome)),
                (Ok(_), Some(error)) => Err(anyhow!(error)),
                (Err(error), _) => Err(error),
            }
        })
        .await?
    }
}

struct Adapter {
    config: PathBuf,
    initialized: AtomicBool,
    closing: AtomicBool,
    workspaces: Mutex<BTreeMap<PathBuf, WorkspaceServices>>,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<Result<()>>>>,
}

impl Adapter {
    fn new(config: PathBuf) -> Self {
        Self {
            config,
            initialized: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            workspaces: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn ready(&self) -> agent_client_protocol::Result<()> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(acp::Error::new(
                -32600,
                "initialize must complete before session requests",
            ));
        }
        if self.closing.load(Ordering::Acquire) {
            return Err(acp::Error::new(-32600, "ACP connection is closing"));
        }
        Ok(())
    }

    /// Retain every task until it completes or disconnect explicitly joins it. SDK callback
    /// futures never own preparation, a running agent, or process cleanup.
    fn spawn(
        &self,
        task: impl Future<Output = Result<()>> + Send + 'static,
    ) -> agent_client_protocol::Result<()> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_| acp::Error::internal_error())?;
        if self.closing.load(Ordering::Acquire) {
            return Err(acp::Error::request_cancelled());
        }
        let (finished, active): (Vec<_>, Vec<_>) = std::mem::take(&mut *tasks)
            .into_iter()
            .partition(tokio::task::JoinHandle::is_finished);
        *tasks = active;
        tasks.push(tokio::spawn(async move {
            let mut failures = Vec::new();
            for previous in finished {
                collect_owned_failure(previous.await, &mut failures);
            }
            // A prior settlement failure must survive reaping, but cannot prevent the newly
            // admitted request from running and settling its own resources.
            if let Err(error) = task.await {
                failures.push(format!("{error:#}"));
            }
            ensure!(failures.is_empty(), "{}", failures.join("; "));
            Ok(())
        }));
        Ok(())
    }

    fn session(&self, id: &acp::SessionId) -> agent_client_protocol::Result<Arc<Session>> {
        self.ready()?;
        self.sessions
            .lock()
            .map_err(|_| acp::Error::internal_error())?
            .get(id.0.as_ref())
            .cloned()
            .ok_or_else(|| acp::Error::resource_not_found(None))
    }

    async fn shutdown(&self) -> Result<()> {
        self.closing.store(true, Ordering::Release);
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("ACP session map unavailable"))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let runs = sessions
            .iter()
            .filter_map(|session| {
                session
                    .active
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .collect::<Vec<_>>();
        // Request cancellation before joining preparation/execution. Approval waits observe this
        // same signal even if the peer has gone away without replying to request_permission.
        tokio::task::spawn_blocking(move || {
            for run in runs {
                run.cancel();
            }
        })
        .await?;
        let tasks = std::mem::take(
            &mut *self
                .tasks
                .lock()
                .map_err(|_| anyhow!("ACP task owner unavailable"))?,
        );
        let mut failures = Vec::new();
        for task in tasks {
            collect_owned_failure(task.await, &mut failures);
        }
        // All foreground/preparation tasks have joined. Reuse the attachment's existing writer
        // and child owner; no path-based writer or synthetic child-completion state is created.
        for session in sessions {
            let cleanup = tokio::task::spawn_blocking(move || -> Result<()> {
                let background = session.attachment.agent_tool_background_runs()?;
                if !background.has_any() {
                    return Ok(());
                }
                let owner = session
                    .attachment
                    .application_operation_owner()
                    .context("ACP session operation owner unavailable during disconnect")?;
                let mut control_session = owner.attach_for_control()?;
                let mut handler = CleanupEvents;
                tokio::runtime::Handle::current().block_on(
                    background.shutdown_session_background_runs(
                        &mut control_session,
                        "ACP client disconnected",
                        &mut handler,
                    ),
                )
            })
            .await;
            match cleanup {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(format!("{error:#}")),
                Err(error) => failures.push(error.to_string()),
            }
        }
        ensure!(
            failures.is_empty(),
            "ACP owned task cleanup failed: {}",
            failures.join("; ")
        );
        Ok(())
    }
}

struct CleanupEvents;
impl sigil_kernel::EventHandler for CleanupEvents {
    fn handle(&mut self, _event: sigil_kernel::RunEvent) -> Result<()> {
        Ok(())
    }
}

fn prompt_text(blocks: Vec<acp::ContentBlock>) -> Result<String> {
    let mut parts = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            acp::ContentBlock::Text(text) => parts.push(text.text),
            acp::ContentBlock::ResourceLink(resource) => {
                // A reference is user-supplied context, not a read receipt or permission grant.
                // Keep URI/name boundaries even when they contain quotes or prompt markup.
                let reference = serde_json::json!({
                    "type": "resource_link", "uri": resource.uri, "name": resource.name,
                    "title": resource.title, "description": resource.description,
                    "mime_type": resource.mime_type, "size": resource.size,
                });
                parts.push(format!(
                    "ACP resource reference (not retrieved; access requires the existing tool permissions):\n{reference}"
                ));
            }
            _ => anyhow::bail!(
                "this ACP adapter accepts text and resource-link prompt content; unsupported content was not discarded"
            ),
        }
    }
    let prompt = parts.join("\n");
    ensure!(!prompt.trim().is_empty(), "ACP prompt is empty");
    Ok(prompt)
}

async fn execute_prompt(
    config: PathBuf,
    session: Arc<Session>,
    run: Arc<Run>,
    prompt: String,
    client: ConnectionTo<Client>,
) -> Result<(
    acp::PromptResponse,
    Option<sigil_runtime::application_run::ApplicationPostRunMaintenance>,
)> {
    let run_id = format!("acp-{}", uuid::Uuid::new_v4());
    let mut request = ApplicationRunRequest::non_interactive(config, &session.cwd, prompt, &run_id);
    request.session_path = Some(session.path.clone());
    request.session_attachment = Some(Arc::clone(&session.attachment));
    request.interaction = ApplicationRunInteraction::ExternallyInteractive;
    request.additional_mcp_servers = session
        .servers
        .lock()
        .map_err(|_| anyhow!("ACP MCP declarations unavailable"))?
        .clone();
    if *run.cancel.borrow() {
        return Ok((acp::PromptResponse::new(acp::StopReason::Cancelled), None));
    }
    // Preparation is an owned operation; cancel never drops this future and leaves its blocking
    // work or extension startup without settlement.
    let prepared = prepare_application_run(request, &session.services).await?;
    let (execution, control) = prepared.into_parts();
    if let Err(error) = run.bind(control).await {
        execution.settle_without_execution().await?;
        return Err(CleanupUnconfirmed(error).into());
    }
    let projection = Arc::new(Mutex::new(events::EventProjection::new(
        client.clone(),
        session.id.clone(),
        session.scope_id.clone(),
        run_id,
    )));
    let handler = events::EventHandler(Arc::clone(&projection));
    let approval = approval::PermissionHandler {
        client,
        session_id: session.id.clone(),
        cancelled: run.cancel.subscribe(),
    };
    let execution = execution.execute_on_owned_blocking(handler, approval);
    tokio::pin!(execution);
    let mut frame = tokio::time::interval(Duration::from_millis(32));
    let output = loop {
        tokio::select! {
            output = &mut execution => break output,
            _ = frame.tick() => {
                if let Err(error) = projection.lock().map_err(|_| anyhow!("ACP preview projection unavailable")).and_then(|mut projection| projection.poll_preview()) {
                    tracing::warn!(%error, "ACP preview delivery unavailable");
                }
            }
        }
    };
    let cancellation = run
        .finalize_cancellation(events::EventHandler(Arc::clone(&projection)))
        .await;
    let (terminal, maintenance) = match output {
        Ok(output) => (Ok(output.terminal_status), output.post_run_maintenance),
        Err(error) => (Err(error), None),
    };
    let reason = settled_stop_reason(terminal, cancellation)?;
    Ok((acp::PromptResponse::new(reason), maintenance))
}

#[derive(Debug)]
struct CleanupUnconfirmed(anyhow::Error);

impl std::fmt::Display for CleanupUnconfirmed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "ACP foreground cleanup unconfirmed: {:#}",
            self.0
        )
    }
}

impl std::error::Error for CleanupUnconfirmed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

fn settled_stop_reason(
    execution: Result<ApplicationRunTerminalStatus>,
    cancellation: Result<Option<sigil_kernel::RunCancellationTerminalOutcome>>,
) -> Result<acp::StopReason> {
    use sigil_kernel::RunCancellationTerminalOutcome;
    let execution = match execution {
        Err(error) if error.is::<sigil_runtime::ApplicationRunCleanupError>() => return Err(error),
        Ok(ApplicationRunTerminalStatus::Interrupted) => {
            return Err(CleanupUnconfirmed(anyhow!(
                "run was interrupted before cleanup confirmation"
            ))
            .into());
        }
        execution => execution,
    };
    match cancellation {
        Ok(Some(RunCancellationTerminalOutcome::Cancelled)) => {
            // The joined owner and durable cleanup receipt, rather than a cancel flag, decide
            // this outcome. Provider cancellation commonly returns an ordinary execution error.
            return Ok(acp::StopReason::Cancelled);
        }
        Ok(Some(RunCancellationTerminalOutcome::Interrupted)) => {
            return Err(
                CleanupUnconfirmed(anyhow!("cancellation cleanup could not be confirmed")).into(),
            );
        }
        Err(error) => return Err(CleanupUnconfirmed(error).into()),
        Ok(None) => {}
    }
    match execution? {
        ApplicationRunTerminalStatus::Succeeded => Ok(acp::StopReason::EndTurn),
        ApplicationRunTerminalStatus::Cancelled => Ok(acp::StopReason::Cancelled),
        ApplicationRunTerminalStatus::Interrupted => Err(CleanupUnconfirmed(anyhow!(
            "run was interrupted before cleanup confirmation"
        ))
        .into()),
        status => anyhow::bail!(
            "Sigil run stopped with {status:?}; its durable recovery state is preserved"
        ),
    }
}

fn prompt_settlement<T>(result: &Result<T>) -> Result<()> {
    if let Err(error) = result
        && (error.is::<CleanupUnconfirmed>()
            || error.is::<sigil_runtime::ApplicationRunCleanupError>())
    {
        return Err(anyhow!("{error:#}"));
    }
    Ok(())
}

fn collect_owned_failure(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
    failures: &mut Vec<String>,
) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => failures.push(format!("{error:#}")),
        Err(error) => failures.push(error.to_string()),
    }
}

fn protocol_error(error: impl std::fmt::Display) -> acp::Error {
    let mut response = acp::Error::internal_error();
    response.message = error.to_string();
    response
}

pub(crate) async fn serve(config: &Path) -> Result<()> {
    serve_transport(config, Stdio::new()).await
}

async fn serve_transport(
    config: &Path,
    transport: impl agent_client_protocol::ConnectTo<Agent> + 'static,
) -> Result<()> {
    let state = Arc::new(Adapter::new(config.to_path_buf()));
    let initialize_state = Arc::clone(&state);
    let new_state = Arc::clone(&state);
    let load_state = Arc::clone(&state);
    let prompt_state = Arc::clone(&state);
    let cancel_state = Arc::clone(&state);
    let result = Agent
        .builder()
        .name("sigil")
        .on_receive_request(
            async move |_request: acp::InitializeRequest, responder, _client| {
                initialize_state.initialized.store(true, Ordering::Release);
                responder.respond(
                    acp::InitializeResponse::new(
                        agent_client_protocol::schema::ProtocolVersion::V1,
                    )
                    .agent_info(acp::Implementation::new("sigil", env!("CARGO_PKG_VERSION")))
                    .agent_capabilities(acp::AgentCapabilities::new().load_session(true)),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest, responder, _client| {
                if let Err(error) = new_state.ready() {
                    return responder.respond_with_error(error);
                }
                let state = Arc::clone(&new_state);
                new_state.spawn(async move {
                    let result = state.new_session(request).await.map_err(protocol_error);
                    if let Err(error) = responder.respond_with_result(result) {
                        tracing::debug!(%error, "ACP new-session reply unavailable");
                    }
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::LoadSessionRequest, responder, client| {
                if let Err(error) = load_state.ready() {
                    return responder.respond_with_error(error);
                }
                let state = Arc::clone(&load_state);
                load_state.spawn(async move {
                    let result = state
                        .load_session(request, client)
                        .await
                        .map_err(protocol_error);
                    if let Err(error) = responder.respond_with_result(result) {
                        tracing::debug!(%error, "ACP load-session reply unavailable");
                    }
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::PromptRequest, responder, client| {
                let session = match prompt_state.session(&request.session_id) {
                    Ok(session) => session,
                    Err(error) => return responder.respond_with_error(error),
                };
                let prompt = match prompt_text(request.prompt) {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        let mut response = acp::Error::invalid_params();
                        response.message = error.to_string();
                        return responder.respond_with_error(response);
                    }
                };
                let run = Arc::new(Run::new());
                {
                    let mut active = session
                        .active
                        .lock()
                        .map_err(|_| acp::Error::internal_error())?;
                    if active.is_some() || session.loading.load(Ordering::Acquire) {
                        return responder.respond_with_error(acp::Error::new(
                            -32600,
                            "this session has an active prompt or history replay",
                        ));
                    }
                    *active = Some(Arc::clone(&run));
                }
                let config = prompt_state.config.clone();
                prompt_state.spawn(async move {
                    let result = execute_prompt(
                        config,
                        Arc::clone(&session),
                        Arc::clone(&run),
                        prompt,
                        client,
                    )
                    .await;
                    let settlement = prompt_settlement(&result);
                    let (result, maintenance) = match result {
                        Ok((response, maintenance)) => (Ok(response), maintenance),
                        Err(error) => (Err(protocol_error(error)), None),
                    };
                    {
                        let mut active = session
                            .active
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if active
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &run))
                        {
                            active.take();
                        }
                    }
                    if let Err(error) = responder.respond_with_result(result) {
                        tracing::debug!(%error, "ACP prompt reply unavailable");
                    }
                    // Title generation is non-critical. The same retained task still joins it
                    // on disconnect, after the foreground result and admission are released.
                    if let Some(maintenance) = maintenance
                        && let Err(error) = maintenance.execute().await
                    {
                        tracing::warn!(%error, "ACP post-run maintenance unavailable");
                    }
                    settlement
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |request: acp::CancelNotification, _client| {
                if let Ok(session) = cancel_state.session(&request.session_id) {
                    let run = session
                        .active
                        .lock()
                        .map_err(|_| acp::Error::internal_error())?
                        .clone();
                    if let Some(run) = run {
                        run.cancel.send_replace(true);
                        cancel_state.spawn(async move {
                            tokio::task::spawn_blocking(move || run.cancel())
                                .await
                                .context("ACP cancel owner failed")?;
                            Ok(())
                        })?;
                    }
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(transport)
        .await;
    let cleanup = state.shutdown().await;
    match (result, cleanup) {
        (_, Err(cleanup)) => Err(cleanup),
        (Err(error), Ok(())) => Err(anyhow!(error.to_string())),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(test)]
#[path = "tests/adapter_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/sdk_tests.rs"]
mod sdk_tests;
