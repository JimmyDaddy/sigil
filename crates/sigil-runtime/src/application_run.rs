use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use sigil_kernel::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutput, AgentRunTerminalReason, ApprovalHandler,
    EgressDisclosurePresenter, EventHandler, InteractionMode, JsonlSessionStore, McpServerStartup,
    MutationEventRecorder, PublicRunEvent, PublicRunEventKind, RootConfig,
    RunCancellationFinalizedEntry, RunCancellationHandle, RunCancellationOwner,
    RunCancellationRecorder, RunCancellationRequestedEntry, RunCancellationTarget,
    RunCancellationTerminalOutcome, RunEvent, RunQuiescenceOutcome, RunTaskGuard, Session,
    WorkspaceTrust, resolve_workspace_root, workspace_trust_from_entries,
};

use crate::{
    activate_eager_remote_mcp_server, attach_remote_mcp_activation_presenter,
    attach_session_url_capability_store,
    build_tool_registry_with_mutation_recorder_and_workspace_trust_and_network_admission,
    context_candidates_from_safe_sources, current_unix_time_ms, resolve_sigil_paths,
    secret_redactor_for_root_config, unsupported_mcp_elicitation_handler,
    unsupported_mcp_runtime_event_handler,
};

const DEFAULT_CANCELLATION_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Stable preparation failure class used by machine adapters without parsing error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRunPrepareErrorClass {
    /// Request shape was invalid before configuration or durable state was opened.
    InvalidInvocation,
    /// Root configuration or provider construction was invalid.
    Configuration,
    /// Durable session, tool, or extension assembly failed.
    Execution,
    /// The owned blocking preparation worker itself failed.
    Internal,
}

/// Typed application-run preparation failure with a deliberately bounded public display string.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationRunPrepareError {
    /// Invalid adapter request.
    #[error("invalid application run request: {message}")]
    InvalidInvocation {
        /// Safe request validation message.
        message: String,
    },
    /// Invalid root/provider configuration.
    #[error("application configuration is invalid")]
    Configuration {
        #[source]
        source: anyhow::Error,
    },
    /// Runtime/session/tool preparation failure.
    #[error("application run preparation failed")]
    Execution {
        #[source]
        source: anyhow::Error,
    },
    /// Blocking worker join failure.
    #[error("application run preparation worker failed")]
    Internal {
        #[source]
        source: anyhow::Error,
    },
}

impl ApplicationRunPrepareError {
    /// Returns the typed machine-routing class without inspecting source text.
    #[must_use]
    pub const fn class(&self) -> ApplicationRunPrepareErrorClass {
        match self {
            Self::InvalidInvocation { .. } => ApplicationRunPrepareErrorClass::InvalidInvocation,
            Self::Configuration { .. } => ApplicationRunPrepareErrorClass::Configuration,
            Self::Execution { .. } => ApplicationRunPrepareErrorClass::Execution,
            Self::Internal { .. } => ApplicationRunPrepareErrorClass::Internal,
        }
    }

    fn configuration(source: impl Into<anyhow::Error>) -> Self {
        Self::Configuration {
            source: source.into(),
        }
    }

    fn execution(source: impl Into<anyhow::Error>) -> Self {
        Self::Execution {
            source: source.into(),
        }
    }
}

/// Interaction contract used by one shared application run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRunInteraction {
    /// The adapter cannot wait for a later explicit user decision.
    NonInteractive,
    /// The adapter has an external approval surface and an owned blocking run context.
    ExternallyInteractive,
}

impl ApplicationRunInteraction {
    fn kernel_mode(self) -> InteractionMode {
        match self {
            Self::NonInteractive => InteractionMode::Headless,
            Self::ExternallyInteractive => InteractionMode::Interactive,
        }
    }
}

/// Input required to prepare one application run.
#[derive(Debug, Clone)]
pub struct ApplicationRunRequest {
    /// Resolved Sigil config path.
    pub config_path: PathBuf,
    /// Process launch working directory.
    pub launch_cwd: PathBuf,
    /// User prompt.
    pub prompt: String,
    /// Adapter-owned run identifier.
    pub run_id: String,
    /// Optional existing or preallocated durable V2 session path.
    pub session_path: Option<PathBuf>,
    /// Whether the adapter can provide explicit approvals after run start.
    pub interaction: ApplicationRunInteraction,
}

impl ApplicationRunRequest {
    /// Creates a non-interactive application run request with a new durable session.
    #[must_use]
    pub fn non_interactive(
        config_path: impl Into<PathBuf>,
        launch_cwd: impl Into<PathBuf>,
        prompt: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            prompt: prompt.into(),
            run_id: run_id.into(),
            session_path: None,
            interaction: ApplicationRunInteraction::NonInteractive,
        }
    }
}

/// Process-local foreground lease manager for durable session paths.
///
/// The append-only writer makes individual appends linear. This lease additionally prevents two
/// independently loaded session projections from executing foreground runs against the same path.
#[derive(Debug, Default)]
pub struct ApplicationSessionLeaseManager {
    active_paths: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl ApplicationSessionLeaseManager {
    /// Creates an empty foreground lease manager.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn acquire(&self, path: &Path) -> Result<ApplicationSessionLease> {
        let canonical = canonical_session_lease_path(path)?;
        let mut active = self
            .active_paths
            .lock()
            .map_err(|_| anyhow!("application session lease state is unavailable"))?;
        if !active.insert(canonical.clone()) {
            bail!(
                "application session already has an active foreground run: {}",
                path.display()
            );
        }
        Ok(ApplicationSessionLease {
            path: canonical,
            active_paths: Arc::clone(&self.active_paths),
        })
    }
}

#[derive(Debug)]
struct ApplicationSessionLease {
    path: PathBuf,
    active_paths: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl Drop for ApplicationSessionLease {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_paths.lock() {
            active.remove(&self.path);
        }
    }
}

/// Shared dependencies used while preparing application runs.
#[derive(Clone)]
pub struct ApplicationRunServices {
    disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
    session_leases: Arc<ApplicationSessionLeaseManager>,
}

impl std::fmt::Debug for ApplicationRunServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplicationRunServices")
            .field("disclosure_presenter", &"configured")
            .field("session_leases", &self.session_leases)
            .finish()
    }
}

impl ApplicationRunServices {
    /// Creates shared run services with a process-local foreground session lease manager.
    #[must_use]
    pub fn new(disclosure_presenter: Arc<dyn EgressDisclosurePresenter>) -> Self {
        Self {
            disclosure_presenter,
            session_leases: Arc::new(ApplicationSessionLeaseManager::new()),
        }
    }

    /// Creates shared run services with an injected session lease manager.
    #[must_use]
    pub fn with_session_leases(
        disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
        session_leases: Arc<ApplicationSessionLeaseManager>,
    ) -> Self {
        Self {
            disclosure_presenter,
            session_leases,
        }
    }
}

/// Sink for ordered provider-neutral application events.
pub trait ApplicationRunEventHandler {
    /// Handles one public event.
    ///
    /// # Errors
    ///
    /// Returns an error when the adapter cannot accept the event and execution should stop.
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()>;
}

/// Prepared application run and its root cancellation authority.
pub struct PreparedApplicationRun {
    execution: ApplicationRunExecution,
    control: ApplicationRunControl,
}

impl PreparedApplicationRun {
    /// Separates the execution payload from its root cancellation authority.
    ///
    /// The caller must keep `control` alive until the execution reaches a terminal state.
    #[must_use]
    pub fn into_parts(self) -> (ApplicationRunExecution, ApplicationRunControl) {
        (self.execution, self.control)
    }

    /// Returns the durable session id.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.execution.session_id
    }

    /// Returns the adapter-owned run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.execution.run_id
    }

    /// Returns the durable V2 session path.
    #[must_use]
    pub fn session_log_path(&self) -> &Path {
        &self.execution.session_log_path
    }
}

/// Root cancellation authority retained by the adapter while an application run is active.
pub struct ApplicationRunControl {
    owner: RunCancellationOwner,
    recorder: RunCancellationRecorder,
    events: ApplicationRunEventSequence,
    _session_lease: Arc<ApplicationSessionLease>,
}

impl std::fmt::Debug for ApplicationRunControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplicationRunControl")
            .field("scope_id", &self.owner.handle().scope_id())
            .finish_non_exhaustive()
    }
}

impl ApplicationRunControl {
    /// Returns the child-facing cancellation handle for diagnostics.
    #[must_use]
    pub fn handle(&self) -> RunCancellationHandle {
        self.owner.handle()
    }

    /// Durably requests cancellation, activates it, and unblocks adapter-owned approval waits.
    ///
    /// # Errors
    ///
    /// Returns an error when the run already reached a terminal phase or the durable request
    /// cannot be appended. Cancellation is still activated after an append failure so forward
    /// effects do not continue merely because audit storage failed.
    pub fn request_cancellation(
        &self,
        reason: impl Into<String>,
        timeout: Option<Duration>,
        unblock_approval: impl FnOnce(),
    ) -> std::result::Result<ApplicationCancellationTicket, ApplicationCancellationRequestError>
    {
        if !self.owner.reserve_cancel() {
            return Err(ApplicationCancellationRequestError::without_ticket(
                anyhow!("application run already reached a terminal cancellation phase"),
            ));
        }
        let requested_timeout = timeout.unwrap_or(DEFAULT_CANCELLATION_QUIESCENCE_TIMEOUT);
        let requested_at = Instant::now();
        let timeout = if requested_at.checked_add(requested_timeout).is_some() {
            requested_timeout
        } else {
            DEFAULT_CANCELLATION_QUIESCENCE_TIMEOUT
        };
        let deadline = requested_at + timeout;
        let requested_at_ms = current_unix_time_ms();
        let request = RunCancellationRequestedEntry {
            request_id: format!("cancel-{}", self.owner.handle().scope_id()),
            run_scope_id: self.owner.handle().scope_id().to_owned(),
            target: RunCancellationTarget::Run,
            reason: reason.into(),
            requested_at_ms,
            quiescence_deadline_ms: requested_at_ms
                .saturating_add(timeout.as_millis().try_into().unwrap_or(u64::MAX)),
        };
        let append = self.recorder.append_requested(&request);
        let activated = self.owner.activate_reserved_cancel();
        debug_assert!(
            activated,
            "reserved cancellation must activate exactly once"
        );
        unblock_approval();
        match append {
            Ok(_) => Ok(ApplicationCancellationTicket {
                request,
                deadline,
                request_recorded: true,
            }),
            Err(error) => Err(ApplicationCancellationRequestError::with_ticket(
                error.context("failed to persist application cancellation request"),
                ApplicationCancellationTicket {
                    request,
                    deadline,
                    request_recorded: false,
                },
            )),
        }
    }

    /// Waits for bounded quiescence and durably records the observed terminal cleanup state.
    ///
    /// `execution_joined` proves that the owned run task/thread reached its terminal boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the terminal cancellation record cannot be appended.
    pub async fn finalize_cancellation<H>(
        &self,
        ticket: ApplicationCancellationTicket,
        execution_joined: bool,
        handler: &mut H,
    ) -> Result<RunCancellationTerminalOutcome>
    where
        H: ApplicationRunEventHandler,
    {
        if !ticket.request_recorded {
            let _ = self
                .owner
                .wait_for_quiescence(ticket.remaining_timeout())
                .await;
            self.events.emit(
                handler,
                PublicRunEventKind::RunFailed {
                    error: "run interrupted because its cancellation request could not be audited"
                        .to_owned(),
                },
            )?;
            bail!("application cancellation request was not durably recorded");
        }
        let quiescence = self
            .owner
            .wait_for_quiescence(ticket.remaining_timeout())
            .await;
        let (outcome, cleanup_complete, active_effects, active_tasks, reason) = match quiescence {
            RunQuiescenceOutcome::Quiescent
                if execution_joined && self.owner.cleanup_complete() =>
            {
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    true,
                    0,
                    0,
                    "cancellation quiescence confirmed".to_owned(),
                )
            }
            RunQuiescenceOutcome::Quiescent => (
                RunCancellationTerminalOutcome::Interrupted,
                false,
                0,
                0,
                "run execution did not join before cancellation terminal".to_owned(),
            ),
            RunQuiescenceOutcome::TimedOut {
                active_effects,
                active_tasks,
            } => (
                RunCancellationTerminalOutcome::Interrupted,
                false,
                active_effects,
                active_tasks,
                "cancellation deadline exceeded; cleanup could not be confirmed".to_owned(),
            ),
        };
        self.recorder
            .append_finalized(&RunCancellationFinalizedEntry {
                request_id: ticket.request.request_id,
                run_scope_id: ticket.request.run_scope_id,
                outcome,
                cleanup_complete,
                active_effects,
                active_tasks,
                reason,
                finalized_at_ms: current_unix_time_ms(),
            })
            .context("failed to persist application cancellation terminal")?;
        let terminal = match outcome {
            RunCancellationTerminalOutcome::Cancelled => PublicRunEventKind::RunCancelled,
            RunCancellationTerminalOutcome::Interrupted => PublicRunEventKind::RunFailed {
                error: "run interrupted before cancellation cleanup could be confirmed".to_owned(),
            },
        };
        self.events.emit(handler, terminal)?;
        Ok(outcome)
    }
}

/// Durable cancellation request retained until cleanup reaches a terminal observation.
#[derive(Debug)]
pub struct ApplicationCancellationTicket {
    request: RunCancellationRequestedEntry,
    deadline: Instant,
    request_recorded: bool,
}

impl ApplicationCancellationTicket {
    /// Returns the time remaining before the cancellation request's bounded deadline.
    #[must_use]
    pub fn remaining_timeout(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

/// Cancellation activation failure that may still carry a ticket requiring quiescence cleanup.
#[derive(Debug)]
pub struct ApplicationCancellationRequestError {
    source: anyhow::Error,
    ticket: Option<Box<ApplicationCancellationTicket>>,
}

impl ApplicationCancellationRequestError {
    fn without_ticket(source: anyhow::Error) -> Self {
        Self {
            source,
            ticket: None,
        }
    }

    fn with_ticket(source: anyhow::Error, ticket: ApplicationCancellationTicket) -> Self {
        Self {
            source,
            ticket: Some(Box::new(ticket)),
        }
    }

    /// Returns a ticket when cancellation was activated despite an audit append failure.
    #[must_use]
    pub fn into_ticket(self) -> Option<ApplicationCancellationTicket> {
        self.ticket.map(|ticket| *ticket)
    }
}

impl fmt::Display for ApplicationCancellationRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.source)
    }
}

impl std::error::Error for ApplicationCancellationRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

/// One prepared provider/session/tool application execution.
pub struct ApplicationRunExecution {
    agent: Agent<Box<dyn sigil_kernel::Provider>>,
    session: Session,
    input: AgentRunInput,
    options: AgentRunOptions,
    session_id: String,
    run_id: String,
    prompt: String,
    session_log_path: PathBuf,
    cancellation_handle: RunCancellationHandle,
    root_task_guard: RunTaskGuard,
    warnings: Vec<String>,
    redactor: sigil_kernel::SecretRedactor,
    interaction: ApplicationRunInteraction,
    events: ApplicationRunEventSequence,
    _session_lease: Arc<ApplicationSessionLease>,
}

/// Provider-neutral terminal classification for one completed application run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRunTerminalStatus {
    /// A final assistant answer was accepted.
    Succeeded,
    /// The configured turn bound was reached without a final answer.
    Interrupted,
    /// A required delegation contract was not satisfied.
    Blocked,
}

/// Successful terminal output from one shared application run.
#[derive(Debug, Clone)]
pub struct ApplicationRunOutput {
    /// Durable session scope.
    pub session_id: String,
    /// Adapter-owned run id.
    pub run_id: String,
    /// Durable V2 JSONL path.
    pub session_log_path: PathBuf,
    /// Terminal application classification derived from durable kernel lifecycle semantics.
    pub terminal_status: ApplicationRunTerminalStatus,
    /// Kernel agent output.
    pub agent_output: AgentRunOutput,
}

impl ApplicationRunExecution {
    /// Executes the prepared run with adapter-provided event and approval handlers.
    ///
    /// Externally interactive approval handlers must run this future under an owned blocking run
    /// context because the kernel approval interface is synchronous.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider/session/tool path or adapter event sink fails.
    pub async fn execute<H, A>(
        self,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<ApplicationRunOutput>
    where
        H: ApplicationRunEventHandler + Send,
        A: ApprovalHandler + Send,
    {
        validate_execution_contract(self.interaction, approval_handler, false)?;
        self.execute_inner(handler, approval_handler).await
    }

    /// Executes an externally interactive run on an owned blocking worker.
    ///
    /// This keeps a synchronous explicit-approval wait off Tokio's async workers while provider
    /// and tool futures continue to use the current runtime handle.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval contract is not explicit, the blocking worker cannot
    /// join, or run execution fails.
    pub async fn execute_on_owned_blocking<H, A>(
        self,
        mut handler: H,
        mut approval_handler: A,
    ) -> Result<ApplicationRunOutput>
    where
        H: ApplicationRunEventHandler + Send + 'static,
        A: ApprovalHandler + Send + 'static,
    {
        validate_execution_contract(self.interaction, &approval_handler, true)?;
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            runtime.block_on(self.execute_inner(&mut handler, &mut approval_handler))
        })
        .await
        .context("application run owned blocking worker failed")?
    }

    async fn execute_inner<H, A>(
        mut self,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<ApplicationRunOutput>
    where
        H: ApplicationRunEventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let _root_task_guard = self.root_task_guard;
        let mut bridge = PublicApplicationEventBridge::new(self.events.clone(), handler);
        bridge.emit(PublicRunEventKind::RunStarted {
            prompt: self.prompt.clone(),
        })?;
        for warning in std::mem::take(&mut self.warnings) {
            bridge.emit(PublicRunEventKind::Notice { message: warning })?;
        }
        let run = self
            .agent
            .run_with_approval_input(
                &mut self.session,
                self.input,
                self.options,
                &mut bridge,
                approval_handler,
            )
            .await;
        match run {
            Ok(agent_output) => {
                let (terminal_status, terminal_event) =
                    application_terminal_projection(&agent_output);
                bridge.emit(terminal_event)?;
                Ok(ApplicationRunOutput {
                    session_id: self.session_id,
                    run_id: self.run_id,
                    session_log_path: self.session_log_path,
                    terminal_status,
                    agent_output,
                })
            }
            Err(error) if self.cancellation_handle.is_cancel_requested() => Err(error)
                .context("application run cancellation is pending terminal cleanup confirmation"),
            Err(error) => {
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                bridge.emit(PublicRunEventKind::RunFailed { error: safe_error })?;
                Err(error)
            }
        }
    }
}

/// Prepares the configured provider, durable session, tools, run options, and cancellation scope.
///
/// # Errors
///
/// Returns an error when config/session/provider/tool/MCP assembly fails or the durable session
/// already has an active foreground run under the supplied lease manager.
pub async fn prepare_application_run(
    request: ApplicationRunRequest,
    services: &ApplicationRunServices,
) -> std::result::Result<PreparedApplicationRun, ApplicationRunPrepareError> {
    if request.prompt.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "prompt must not be empty".to_owned(),
        });
    }
    if request.run_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "run id must not be empty".to_owned(),
        });
    }
    let session_leases = Arc::clone(&services.session_leases);
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_application_run_blocking(request, session_leases)
    })
    .await
    .map_err(|error| ApplicationRunPrepareError::Internal {
        source: anyhow!(error).context("application run blocking preparation task failed"),
    })??;
    let BlockingApplicationRunPreparation {
        root_config,
        workspace_root,
        session_path,
        session_lease,
        mutation_recorder,
        session,
        workspace_trust,
        cancellation_recorder,
        cancellation_owner,
        cancellation_handle,
        root_task_guard,
        provider,
        options,
        input,
        run_id,
        prompt,
        interaction,
        redactor,
    } = prepared;
    let mut registry =
        build_tool_registry_with_mutation_recorder_and_workspace_trust_and_network_admission(
            &root_config,
            &provider.capabilities(),
            workspace_root.clone(),
            mutation_recorder,
            workspace_trust,
            sigil_kernel::ExtensionProcessNetworkAdmission::new(
                options.permission_context.network_policy,
                false,
            ),
        )
        .await
        .map_err(ApplicationRunPrepareError::execution)?;
    let elicitation_handler = unsupported_mcp_elicitation_handler();
    let runtime_event_handler = unsupported_mcp_runtime_event_handler();
    attach_remote_mcp_activation_presenter(
        &mut registry,
        &root_config,
        &provider.capabilities(),
        workspace_root.clone(),
        Arc::clone(&elicitation_handler),
        runtime_event_handler,
        Arc::clone(&services.disclosure_presenter),
    );
    let eager_remote_servers = root_config
        .mcp_servers
        .iter()
        .filter(|server| {
            server.startup == McpServerStartup::Eager && server.streamable_http().is_some()
        })
        .map(|server| (server.name.clone(), server.required))
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
    for (server_name, required) in eager_remote_servers {
        let activation = activate_eager_remote_mcp_server(
            &mut registry,
            &root_config,
            &server_name,
            provider.capabilities().tool_name_max_chars,
            workspace_root.clone(),
            session
                .egress_audit_recorder()
                .map_err(ApplicationRunPrepareError::execution)?,
            Arc::clone(&services.disclosure_presenter),
            Arc::clone(&elicitation_handler),
        )
        .await;
        if let Err(error) = activation {
            if required {
                return Err(ApplicationRunPrepareError::execution(error));
            }
            warnings.push(optional_eager_mcp_warning(&redactor, &server_name, &error));
        }
    }
    let session_id = session.session_scope_id().to_owned();
    let events = ApplicationRunEventSequence::new(session_id.clone(), run_id.clone());
    Ok(PreparedApplicationRun {
        execution: ApplicationRunExecution {
            agent: Agent::new(provider, registry),
            session,
            input,
            options,
            session_id,
            run_id,
            prompt,
            session_log_path: session_path,
            cancellation_handle,
            root_task_guard,
            warnings,
            redactor,
            interaction,
            events: events.clone(),
            _session_lease: Arc::clone(&session_lease),
        },
        control: ApplicationRunControl {
            owner: cancellation_owner,
            recorder: cancellation_recorder,
            events,
            _session_lease: session_lease,
        },
    })
}

/// Creates the default durable V2 JSONL path for one new application session.
#[must_use]
pub fn default_application_session_path(session_log_dir: &Path) -> PathBuf {
    session_log_dir.join(format!("session-{}.jsonl", uuid::Uuid::new_v4()))
}

/// Builds provider input with safe repository context candidates.
#[must_use]
pub fn application_run_input(workspace_root: &Path, prompt: String) -> AgentRunInput {
    let runtime_context =
        context_candidates_from_safe_sources(workspace_root, &prompt, None).unwrap_or_default();
    AgentRunInput::user(prompt).with_runtime_context(runtime_context)
}

struct BlockingApplicationRunPreparation {
    root_config: RootConfig,
    workspace_root: PathBuf,
    session_path: PathBuf,
    session_lease: Arc<ApplicationSessionLease>,
    mutation_recorder: MutationEventRecorder,
    session: Session,
    workspace_trust: WorkspaceTrust,
    cancellation_recorder: RunCancellationRecorder,
    cancellation_owner: RunCancellationOwner,
    cancellation_handle: RunCancellationHandle,
    root_task_guard: RunTaskGuard,
    provider: Box<dyn sigil_kernel::Provider>,
    options: AgentRunOptions,
    input: AgentRunInput,
    run_id: String,
    prompt: String,
    interaction: ApplicationRunInteraction,
    redactor: sigil_kernel::SecretRedactor,
}

fn prepare_application_run_blocking(
    request: ApplicationRunRequest,
    session_leases: Arc<ApplicationSessionLeaseManager>,
) -> std::result::Result<BlockingApplicationRunPreparation, ApplicationRunPrepareError> {
    let root_config = RootConfig::load(&request.config_path)
        .map_err(ApplicationRunPrepareError::configuration)?;
    let provider =
        crate::build_provider(&root_config).map_err(ApplicationRunPrepareError::configuration)?;
    let workspace_root = resolve_workspace_root(
        &request.config_path,
        &request.launch_cwd,
        &root_config.workspace.root,
    );
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let requested_session_path = request
        .session_path
        .unwrap_or_else(|| default_application_session_path(&sigil_paths.session_log_dir));
    let session_store = JsonlSessionStore::new(&requested_session_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let session_path = session_store.path().to_owned();
    let session_lease = Arc::new(
        session_leases
            .acquire(&session_path)
            .map_err(ApplicationRunPrepareError::execution)?,
    );
    let mutation_recorder = MutationEventRecorder::new(session_store.clone());
    let (mut session, workspace_trust) = load_application_session_with_workspace_trust(
        root_config.agent.provider.clone(),
        root_config.agent.model.clone(),
        session_store,
        &workspace_root,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    attach_session_url_capability_store(&mut session)
        .map_err(ApplicationRunPrepareError::execution)?;

    let cancellation_recorder = session
        .run_cancellation_recorder()
        .map_err(ApplicationRunPrepareError::execution)?;
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let root_task_guard = cancellation_handle
        .register_task()
        .map_err(ApplicationRunPrepareError::execution)?;
    let options = crate::build_run_options(
        &root_config,
        workspace_root.clone(),
        request.interaction.kernel_mode(),
    );
    let input = application_run_input(&workspace_root, request.prompt.clone())
        .with_logical_run_id(request.run_id.clone())
        .with_cancellation(cancellation_handle.clone());
    let redactor = secret_redactor_for_root_config(&root_config);
    Ok(BlockingApplicationRunPreparation {
        root_config,
        workspace_root,
        session_path,
        session_lease,
        mutation_recorder,
        session,
        workspace_trust,
        cancellation_recorder,
        cancellation_owner,
        cancellation_handle,
        root_task_guard,
        provider,
        options,
        input,
        run_id: request.run_id,
        prompt: request.prompt,
        interaction: request.interaction,
        redactor,
    })
}

fn load_application_session_with_workspace_trust(
    provider_name: impl Into<String>,
    model_name: impl Into<String>,
    session_store: JsonlSessionStore,
    workspace_root: &Path,
) -> Result<(Session, WorkspaceTrust)> {
    let session = Session::load_from_store(provider_name, model_name, session_store)?;
    let workspace_trust = workspace_trust_from_entries(session.entries(), workspace_root)?;
    Ok((session, workspace_trust))
}

fn canonical_session_lease_path(path: &Path) -> Result<PathBuf> {
    if std::fs::symlink_metadata(path).is_ok() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("application session path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let canonical_parent = parent.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize application session directory {}",
            parent.display()
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        anyhow!(
            "application session path has no file name: {}",
            path.display()
        )
    })?;
    Ok(canonical_parent.join(file_name))
}

#[derive(Debug, Clone)]
struct ApplicationRunEventSequence {
    session_id: String,
    run_id: String,
    state: Arc<Mutex<ApplicationRunEventState>>,
}

#[derive(Debug, Default)]
struct ApplicationRunEventState {
    sequence: u64,
    terminal: bool,
}

impl ApplicationRunEventSequence {
    fn new(session_id: String, run_id: String) -> Self {
        Self {
            session_id,
            run_id,
            state: Arc::new(Mutex::new(ApplicationRunEventState::default())),
        }
    }

    fn emit<H>(&self, handler: &mut H, event: PublicRunEventKind) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        state.sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        let terminal = is_terminal_public_run_event(&event);
        if terminal {
            state.terminal = true;
        }
        handler.handle_public_event(PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            state.sequence,
            event,
        ))
    }
}

fn is_terminal_public_run_event(event: &PublicRunEventKind) -> bool {
    matches!(
        event,
        PublicRunEventKind::RunFinished { .. }
            | PublicRunEventKind::RunFailed { .. }
            | PublicRunEventKind::RunCancelled
    )
}

struct PublicApplicationEventBridge<'a, H> {
    events: ApplicationRunEventSequence,
    handler: &'a mut H,
}

impl<'a, H> PublicApplicationEventBridge<'a, H>
where
    H: ApplicationRunEventHandler,
{
    fn new(events: ApplicationRunEventSequence, handler: &'a mut H) -> Self {
        Self { events, handler }
    }

    fn emit(&mut self, event: PublicRunEventKind) -> Result<()> {
        self.events.emit(self.handler, event)
    }
}

impl<H> EventHandler for PublicApplicationEventBridge<'_, H>
where
    H: ApplicationRunEventHandler,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.emit(event.into())
    }
}

fn validate_execution_contract(
    interaction: ApplicationRunInteraction,
    approval_handler: &impl ApprovalHandler,
    owned_blocking_worker: bool,
) -> Result<()> {
    if interaction == ApplicationRunInteraction::ExternallyInteractive {
        if !owned_blocking_worker {
            bail!("externally interactive runs require an owned blocking execution worker");
        }
        if !approval_handler.approval_is_explicit_user_action() {
            bail!("externally interactive runs require an explicit-user-action approval handler");
        }
    }
    Ok(())
}

fn application_terminal_projection(
    output: &AgentRunOutput,
) -> (ApplicationRunTerminalStatus, PublicRunEventKind) {
    match output.outcome.terminal_reason {
        AgentRunTerminalReason::FinalAnswer => (
            ApplicationRunTerminalStatus::Succeeded,
            PublicRunEventKind::RunFinished {
                final_text: output.result.final_text.clone(),
            },
        ),
        AgentRunTerminalReason::MaxTurns => (
            ApplicationRunTerminalStatus::Interrupted,
            PublicRunEventKind::RunFailed {
                error: "run interrupted after reaching the configured turn limit".to_owned(),
            },
        ),
        AgentRunTerminalReason::DelegationUnsatisfied => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "run blocked because its required delegation was not satisfied".to_owned(),
            },
        ),
    }
}

fn optional_eager_mcp_warning(
    redactor: &sigil_kernel::SecretRedactor,
    server_name: &str,
    error: &anyhow::Error,
) -> String {
    let safe_error = redactor.redact_text(&format!("{error:#}"));
    format!("optional eager MCP server {server_name} failed: {safe_error}")
}

#[cfg(test)]
#[path = "tests/application_run_tests.rs"]
mod tests;
