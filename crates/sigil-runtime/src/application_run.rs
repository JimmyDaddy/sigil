use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutput, AgentRunTerminalReason, ApprovalHandler,
    AssistantMessageKind, ControlEntry, EgressDisclosurePresenter, EventHandler, InteractionMode,
    JsonlSessionStore, McpServerStartup, MessageRole, MutationEventRecorder, NoopEventHandler,
    PermissionMode, PublicRunEvent, PublicRunEventKind, ReasoningEffort, RootConfig,
    RunCancellationFinalizedEntry, RunCancellationHandle, RunCancellationOwner,
    RunCancellationRecorder, RunCancellationRequestedEntry, RunCancellationTarget,
    RunCancellationTerminalOutcome, RunEvent, RunQuiescenceOutcome, RunTaskGuard, Session,
    SessionLogEntry, TaskVerificationRerunRequest, ToolRegistryScope, VerificationProductView,
    WorkspaceTrust, rerun_task_verification_check, resolve_workspace_root, safe_persistence_text,
    verification_product_view, workspace_trust_from_entries,
};

use crate::{
    activate_eager_remote_mcp_server, attach_remote_mcp_activation_presenter,
    attach_session_url_capability_store,
    build_tool_surface_with_mutation_recorder_and_workspace_trust_and_network_admission,
    context_candidates_from_safe_sources, current_unix_time_ms, resolve_sigil_paths,
    secret_redactor_for_root_config, unsupported_mcp_elicitation_handler,
    unsupported_mcp_runtime_event_handler,
};

const DEFAULT_CANCELLATION_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default number of user-visible messages returned by one transcript page.
pub const DEFAULT_APPLICATION_TRANSCRIPT_PAGE_SIZE: usize = 50;
/// Maximum number of user-visible messages returned by one transcript page.
pub const MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE: usize = 100;
/// Maximum safe text bytes retained for one transcript message.
pub const MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES: usize = 64 * 1024;
/// Maximum safe text bytes retained across one transcript page.
pub const MAX_APPLICATION_TRANSCRIPT_PAGE_BYTES: usize = 512 * 1024;

/// Provider-neutral role exposed by the bounded application transcript projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationTranscriptRole {
    /// User-authored conversation input.
    User,
    /// Assistant-authored output, including explicitly classified progress/reasoning messages.
    Assistant,
    /// Result of one tool invocation.
    Tool,
}

/// One safe user-visible transcript message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTranscriptMessage {
    /// Stable one-based position among user-visible messages in the append-only session.
    pub ordinal: u64,
    /// Stable hashed message identity used only for reconciliation, not primary UI copy.
    pub message_id: String,
    /// Provider-neutral display role.
    pub role: ApplicationTranscriptRole,
    /// Sanitized and bounded text, when the durable message carried text.
    pub content: Option<String>,
    /// Assistant phase retained for correct final/progress/reasoning presentation.
    pub assistant_kind: Option<AssistantMessageKind>,
    /// Tool name resolved from the preceding assistant call without exposing tool arguments.
    pub tool_name: Option<String>,
    /// Number of safe attachment descriptors omitted from this text-only projection.
    pub image_attachment_count: u64,
    /// Whether text was shortened to the per-message bound.
    pub truncated: bool,
    /// Sanitized text size before truncation.
    pub original_content_bytes: u64,
}

/// One chronological, backwards-pageable transcript page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTranscriptPage {
    /// Durable session scope proven while reading the stream.
    pub session_scope_id: String,
    /// Total user-visible message count observed for this read.
    pub total_messages: u64,
    /// Chronologically ordered bounded page.
    pub messages: Vec<ApplicationTranscriptMessage>,
    /// Exclusive ordinal for the next older page.
    pub next_before: Option<u64>,
}

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
    /// The adapter resolves approval policy synchronously without waiting for later user input.
    AdapterManaged,
    /// The adapter has an external approval surface and an owned blocking run context.
    ExternallyInteractive,
}

impl ApplicationRunInteraction {
    fn kernel_mode(self) -> InteractionMode {
        match self {
            Self::NonInteractive => InteractionMode::Headless,
            Self::AdapterManaged => InteractionMode::Interactive,
            Self::ExternallyInteractive => InteractionMode::Interactive,
        }
    }
}

/// Durable V2 session identity established for an adapter-owned routing session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSessionBinding {
    /// Durable scope derived from the canonical JSONL path.
    pub session_scope_id: String,
    /// Canonical durable JSONL path.
    pub session_log_path: PathBuf,
}

/// Provider-neutral facts needed to configure and explain the next application run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRunContextView {
    /// Provider identity durably frozen for this session.
    pub provider_name: String,
    /// Model identity durably frozen for this session.
    pub model_name: String,
    /// Models this application surface may bind for a new session with the same provider.
    pub available_models: Vec<String>,
    /// Configured permission mode used when a client does not override one run.
    pub default_permission_mode: PermissionMode,
    /// Exact reasoning-effort values implemented for this durable provider and model.
    pub available_reasoning_efforts: Vec<ReasoningEffort>,
    /// Configured default when it belongs to `available_reasoning_efforts`.
    pub default_reasoning_effort: Option<ReasoningEffort>,
    /// Opaque exact-provider/model capability binding echoed by an explicit run selection.
    pub reasoning_effort_binding: Option<String>,
    /// Effective context window when provider metadata or configuration proves one.
    pub context_window_tokens: Option<u32>,
    /// Prompt tokens recorded by the latest durable usage snapshot.
    pub last_prompt_tokens: Option<u64>,
    /// Source used to resolve the effective context window.
    pub context_window_source: crate::ContextWindowSource,
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
    /// Optional user-selected permission mode for this run.
    pub permission_mode: Option<PermissionMode>,
    /// Optional exact effort selected for this run.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Opaque binding returned with the run-context effort capability.
    pub reasoning_effort_binding: Option<String>,
    /// Optional adapter-owned hard constraints applied before provider dispatch.
    pub constraints: Option<ApplicationRunConstraints>,
}

/// Provider-neutral hard constraints for one shared application run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRunConstraints {
    /// Maximum model turns for this run.
    pub max_turns: usize,
    /// Maximum output tokens sent on every provider request in this run.
    pub max_output_tokens: u32,
    /// Maximum tool surface visible to the provider and executable by the agent.
    pub tool_scope: ToolRegistryScope,
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
            permission_mode: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            constraints: None,
        }
    }

    /// Applies adapter-owned hard constraints without changing the persisted user configuration.
    #[must_use]
    pub fn with_constraints(mut self, constraints: ApplicationRunConstraints) -> Self {
        self.constraints = Some(constraints);
        self
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

    /// Returns whether the adapter event handler accepted a terminal public event for this run.
    ///
    /// Adapters that require durable delivery must only return success from their handler after
    /// the corresponding append is complete.
    ///
    /// # Errors
    ///
    /// Returns an error when the shared sequence state is unavailable.
    pub fn terminal_was_delivered(&self) -> Result<bool> {
        self.events.terminal_was_delivered()
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
        let reason = reason.into();
        let request = RunCancellationRequestedEntry {
            request_id: format!("cancel-{}", self.owner.handle().scope_id()),
            run_scope_id: self.owner.handle().scope_id().to_owned(),
            target: RunCancellationTarget::Run,
            reason: safe_persistence_text(&reason),
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
        mut input,
        run_id,
        prompt,
        interaction,
        redactor,
        tool_scope,
    } = prepared;
    let surface =
        build_tool_surface_with_mutation_recorder_and_workspace_trust_and_network_admission(
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
    input = attach_application_request_context(input, &surface.context_resolver, &prompt).await;
    let mut registry = surface.registry;
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
    if let Some(scope) = tool_scope {
        registry = constrain_application_tool_registry(registry, &scope)
            .map_err(ApplicationRunPrepareError::execution)?;
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

/// Creates or reopens the durable V2 session used by an adapter routing handle.
///
/// This operation establishes the session envelope and recovery state without assembling a
/// provider or starting an agent run. Foreground exclusivity remains owned by
/// `prepare_application_run` and its shared lease manager.
///
/// # Errors
///
/// Returns a typed preparation error when configuration or durable session recovery fails.
pub fn bind_application_session(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    bind_application_session_with_model(config_path, launch_cwd, session_path, None)
}

/// Creates or reopens a durable V2 session using an optional application-selected model.
///
/// The selected model establishes only a new session identity. Durable identity remains
/// authoritative when `session_path` already contains session state.
///
/// # Errors
///
/// Returns a typed preparation error when the model is unavailable for the configured provider,
/// or configuration/session recovery fails.
pub fn bind_application_session_with_model(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
    model_name: Option<&str>,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    let mut root_config =
        RootConfig::load(config_path).map_err(ApplicationRunPrepareError::configuration)?;
    if let Some(model_name) = model_name {
        let requested =
            crate::normalize_provider_model_alias(&root_config.agent.provider, model_name)
                .ok_or_else(|| ApplicationRunPrepareError::InvalidInvocation {
                    message: "application session model must not be empty".to_owned(),
                })?;
        let available =
            application_model_options(&root_config.agent.provider, &root_config.agent.model);
        if !available.contains(&requested) {
            return Err(ApplicationRunPrepareError::InvalidInvocation {
                message: format!("model {requested} is not available for the configured provider"),
            });
        }
        root_config.agent.model = requested;
    }
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let requested_path = session_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_application_session_path(&sigil_paths.session_log_dir));
    let canonical_path = canonical_session_lease_path(&requested_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let store =
        JsonlSessionStore::new(&canonical_path).map_err(ApplicationRunPrepareError::execution)?;
    let session =
        Session::load_from_store(root_config.agent.provider, root_config.agent.model, store)
            .map_err(ApplicationRunPrepareError::execution)?;
    Ok(ApplicationSessionBinding {
        session_scope_id: session.session_scope_id().to_owned(),
        session_log_path: canonical_path,
    })
}

fn application_model_options(provider_name: &str, current_model: &str) -> Vec<String> {
    let mut models = vec![current_model.to_owned()];
    if crate::normalize_provider_name(provider_name) == crate::DEEPSEEK_PROVIDER_KEY {
        models.push("deepseek-v4-flash".to_owned());
        models.push("deepseek-v4-pro".to_owned());
    }
    models.sort();
    models.dedup();
    models
}

/// Reopens one existing durable V2 session without creating a missing path.
///
/// Callers must first establish their own workspace/catalog authorization for `session_path`.
/// This second binding step rejects a final-component symlink, requires an existing regular file,
/// and reloads the durable stream before returning its canonical scope.
///
/// # Errors
///
/// Returns a typed preparation error when configuration cannot load, the existing source is not a
/// regular non-symlink file, or durable V2 recovery fails.
pub fn bind_existing_application_session(
    config_path: &Path,
    session_path: &Path,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    let root_config =
        RootConfig::load(config_path).map_err(ApplicationRunPrepareError::configuration)?;
    let metadata = std::fs::symlink_metadata(session_path)
        .with_context(|| {
            format!(
                "failed to inspect existing session {}",
                session_path.display()
            )
        })
        .map_err(ApplicationRunPrepareError::execution)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ApplicationRunPrepareError::execution(anyhow!(
            "existing application session must be a regular non-symlink file"
        )));
    }
    let canonical_path = session_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", session_path.display()))
        .map_err(ApplicationRunPrepareError::execution)?;
    let store =
        JsonlSessionStore::new(&canonical_path).map_err(ApplicationRunPrepareError::execution)?;
    let session =
        Session::load_from_store(root_config.agent.provider, root_config.agent.model, store)
            .map_err(ApplicationRunPrepareError::execution)?;
    Ok(ApplicationSessionBinding {
        session_scope_id: session.session_scope_id().to_owned(),
        session_log_path: canonical_path,
    })
}

/// Projects the current model and bounded context usage for one bound durable session.
///
/// The model comes from the durable session identity rather than current configuration. Usage is
/// absent until the provider has emitted at least one durable usage snapshot, so clients never
/// need to infer zero usage from missing telemetry.
///
/// # Errors
///
/// Returns an error when configuration or durable state cannot be decoded, or when the durable
/// scope differs from the adapter binding being queried.
pub fn application_run_context_view(
    config_path: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<ApplicationRunContextView> {
    if expected_session_scope_id.is_empty() {
        bail!("expected run-context session scope must not be empty");
    }
    let mut root_config = RootConfig::load(config_path)?;
    let store = JsonlSessionStore::new(session_path)?;
    let session =
        Session::load_from_store(root_config.agent.provider, root_config.agent.model, store)?;
    if session.session_scope_id() != expected_session_scope_id {
        bail!("durable run-context session scope does not match the bound session");
    }
    root_config.agent.provider = session.provider_name().to_owned();
    root_config.agent.model = session.model_name().to_owned();
    let resolved = crate::resolve_context_window_tokens(
        session.provider_name(),
        session.model_name(),
        root_config.compaction.context_window_tokens,
    );
    let has_usage = session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(_))
        )
    });
    let last_prompt_tokens = if has_usage {
        session
            .try_usage_stats_from_durable()?
            .map(|stats| stats.last_prompt_tokens)
    } else {
        None
    };
    let available_reasoning_efforts = crate::reasoning_effort::supported_reasoning_efforts(
        session.provider_name(),
        session.model_name(),
    );
    let default_reasoning_effort =
        crate::reasoning_effort::configured_default_reasoning_effort(&root_config);
    let reasoning_effort_binding = crate::reasoning_effort::reasoning_effort_binding(
        session.provider_name(),
        session.model_name(),
        &available_reasoning_efforts,
    );
    Ok(ApplicationRunContextView {
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
        available_models: application_model_options(session.provider_name(), session.model_name()),
        default_permission_mode: root_config.permission.mode,
        available_reasoning_efforts,
        default_reasoning_effort,
        reasoning_effort_binding,
        context_window_tokens: resolved.tokens,
        last_prompt_tokens,
        context_window_source: resolved.source,
    })
}

/// Reads the current shared verification product projection for one bound durable session.
///
/// This query decodes append-only session truth without creating adapter-owned verification
/// state or exposing the session path to a renderer.
///
/// # Errors
///
/// Returns an error when the durable stream cannot be decoded.
pub fn application_verification_view(
    session_path: &Path,
) -> Result<Option<VerificationProductView>> {
    let entries = JsonlSessionStore::read_entries(session_path)?;
    Ok(verification_product_view(&entries))
}

/// Reads one safe, bounded and backwards-pageable user transcript from durable session truth.
///
/// The projection deliberately excludes system/control data, tool arguments, resolved image bytes
/// and the source path. `before` is an exclusive one-based message ordinal so pagination remains
/// stable while the append-only stream grows.
///
/// # Errors
///
/// Returns an error when bounds are invalid, the durable scope differs from the expected binding,
/// or the V2 stream cannot be decoded safely.
pub fn application_session_transcript_page(
    session_path: &Path,
    expected_session_scope_id: &str,
    before: Option<u64>,
    limit: usize,
) -> Result<ApplicationTranscriptPage> {
    if expected_session_scope_id.is_empty() {
        bail!("expected transcript session scope must not be empty");
    }
    if !(1..=MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE).contains(&limit) {
        bail!("transcript page size must be between 1 and {MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE}");
    }
    if before == Some(0) {
        bail!("transcript before ordinal must be positive");
    }

    let records = JsonlSessionStore::read_event_records(session_path)?;
    let actual_session_scope_id = records
        .first()
        .map(|record| record.session_id().to_owned())
        .ok_or_else(|| anyhow!("durable transcript has no session identity"))?;
    if actual_session_scope_id != expected_session_scope_id
        || records
            .iter()
            .any(|record| record.session_id() != expected_session_scope_id)
    {
        bail!("durable transcript session scope does not match the bound session");
    }

    let entries = records
        .iter()
        .filter_map(|record| {
            record
                .stored_event()
                .payload
                .get("session_log_entry")
                .cloned()
        })
        .map(|value| {
            serde_json::from_value::<SessionLogEntry>(value)
                .context("failed to decode durable transcript session entry")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut tool_names = BTreeMap::new();
    let mut projected = Vec::new();
    for entry in entries {
        let (message, role, expected_role) = match entry {
            SessionLogEntry::User(message) => {
                (message, ApplicationTranscriptRole::User, MessageRole::User)
            }
            SessionLogEntry::Assistant(message) => {
                for call in &message.tool_calls {
                    tool_names.insert(
                        call.id.clone(),
                        truncate_application_transcript_text(
                            &safe_persistence_text(&call.name),
                            128,
                        ),
                    );
                }
                (
                    message,
                    ApplicationTranscriptRole::Assistant,
                    MessageRole::Assistant,
                )
            }
            SessionLogEntry::ToolResult(message) => {
                (message, ApplicationTranscriptRole::Tool, MessageRole::Tool)
            }
            SessionLogEntry::Control(_) => continue,
        };
        if message.role != expected_role {
            bail!("durable transcript entry role does not match its entry class");
        }
        let ordinal = u64::try_from(projected.len())
            .map_err(|_| anyhow!("transcript message count exceeds supported range"))?
            .saturating_add(1);
        let safe_content = message.content.as_deref().map(safe_persistence_text);
        let original_content_bytes = safe_content.as_ref().map_or(0, String::len);
        let truncated = original_content_bytes > MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES;
        let content = safe_content.map(|content| {
            truncate_application_transcript_text(&content, MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES)
        });
        let tool_name = message
            .tool_call_id
            .as_ref()
            .and_then(|call_id| tool_names.get(call_id))
            .cloned();
        projected.push(ApplicationTranscriptMessage {
            ordinal,
            message_id: safe_application_transcript_message_id(&message.id),
            role,
            content,
            assistant_kind: if role == ApplicationTranscriptRole::Assistant {
                message.assistant_kind
            } else {
                None
            },
            tool_name,
            image_attachment_count: u64::try_from(message.image_attachments.len())
                .map_err(|_| anyhow!("transcript attachment count exceeds supported range"))?,
            truncated,
            original_content_bytes: u64::try_from(original_content_bytes)
                .map_err(|_| anyhow!("transcript content size exceeds supported range"))?,
        });
    }

    let total_messages = u64::try_from(projected.len())
        .map_err(|_| anyhow!("transcript message count exceeds supported range"))?;
    let eligible_end = before.map_or(projected.len(), |before| {
        projected.partition_point(|message| message.ordinal < before)
    });
    let mut page_bytes = 0_usize;
    let mut messages = Vec::with_capacity(limit.min(eligible_end));
    for message in projected[..eligible_end].iter().rev() {
        if messages.len() == limit {
            break;
        }
        let message_bytes = message.content.as_ref().map_or(0, String::len);
        if !messages.is_empty()
            && page_bytes.saturating_add(message_bytes) > MAX_APPLICATION_TRANSCRIPT_PAGE_BYTES
        {
            break;
        }
        page_bytes = page_bytes.saturating_add(message_bytes);
        messages.push(message.clone());
    }
    messages.reverse();
    let next_before = messages
        .first()
        .filter(|message| message.ordinal > 1)
        .map(|message| message.ordinal);

    Ok(ApplicationTranscriptPage {
        session_scope_id: actual_session_scope_id,
        total_messages,
        messages,
        next_before,
    })
}

fn truncate_application_transcript_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn safe_application_transcript_message_id(value: &str) -> String {
    format!("message-sha256:{:x}", Sha256::digest(value.as_bytes()))
}

/// Reruns one exact verification recommendation through the shared execution backend and lease.
///
/// # Errors
///
/// Returns an error when the bound session identity drifted, another foreground operation owns the
/// session, the rendered verification binding is stale, or execution cannot reach a durable
/// terminal receipt.
pub async fn rerun_application_verification(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    services: &ApplicationRunServices,
    request: &TaskVerificationRerunRequest,
) -> Result<VerificationProductView> {
    let config_path = config_path.to_owned();
    let launch_cwd = launch_cwd.to_owned();
    let session_path = session_path.to_owned();
    let expected_session_scope_id = expected_session_scope_id.to_owned();
    let session_leases = Arc::clone(&services.session_leases);
    let request = request.clone();
    let preparation = tokio::task::spawn_blocking(move || {
        let root_config = RootConfig::load(&config_path)?;
        let workspace_root =
            resolve_workspace_root(&config_path, &launch_cwd, &root_config.workspace.root);
        let store = JsonlSessionStore::new(&session_path)?;
        let session_lease = session_leases.acquire(store.path())?;
        let session = Session::load_from_store(
            root_config.agent.provider.clone(),
            root_config.agent.model.clone(),
            store,
        )?;
        if session.session_scope_id() != expected_session_scope_id {
            bail!("durable session identity changed before verification rerun");
        }
        let execution_backend = crate::build_configured_execution_backend(&root_config)?;
        Ok::<_, anyhow::Error>((
            session,
            session_lease,
            workspace_root,
            execution_backend,
            request,
        ))
    })
    .await
    .map_err(|_| anyhow!("verification rerun preparation worker failed"))??;
    let (mut session, _session_lease, workspace_root, execution_backend, request) = preparation;
    let mut handler = NoopEventHandler;
    rerun_task_verification_check(
        &mut session,
        &mut handler,
        execution_backend.as_ref(),
        &workspace_root,
        &request,
    )
    .await?;
    verification_product_view(session.entries())
        .ok_or_else(|| anyhow!("verification rerun completed without a product projection"))
}

/// Durably records a cancellation that won the race with application-run preparation.
///
/// This path proves that no agent execution was admitted, so the terminal cleanup evidence is
/// immediately complete. The request/finalized pair remains append-only and idempotent across a
/// retry with the same run id.
///
/// # Errors
///
/// Returns a typed preparation error when configuration, session recovery, or either durable
/// cancellation append fails.
pub fn record_application_preparation_cancellation(
    config_path: &Path,
    session_path: &Path,
    run_id: &str,
    reason: &str,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    if run_id.trim().is_empty() || safe_persistence_text(run_id) != run_id {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "run id must be non-empty and persistence-safe".to_owned(),
        });
    }
    let root_config =
        RootConfig::load(config_path).map_err(ApplicationRunPrepareError::configuration)?;
    let canonical_path = canonical_session_lease_path(session_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let store =
        JsonlSessionStore::new(&canonical_path).map_err(ApplicationRunPrepareError::execution)?;
    let session =
        Session::load_from_store(root_config.agent.provider, root_config.agent.model, store)
            .map_err(ApplicationRunPrepareError::execution)?;
    let recorder = session
        .run_cancellation_recorder()
        .map_err(ApplicationRunPrepareError::execution)?;
    let recorded_at_ms = current_unix_time_ms();
    let request_id = format!("cancel-preparation-{run_id}");
    let run_scope_id = format!("application-preparation-{run_id}");
    recorder
        .append_requested(&RunCancellationRequestedEntry {
            request_id: request_id.clone(),
            run_scope_id: run_scope_id.clone(),
            target: RunCancellationTarget::Run,
            reason: safe_persistence_text(reason),
            requested_at_ms: recorded_at_ms,
            quiescence_deadline_ms: recorded_at_ms,
        })
        .map_err(ApplicationRunPrepareError::execution)?;
    recorder
        .append_finalized(&RunCancellationFinalizedEntry {
            request_id,
            run_scope_id,
            outcome: RunCancellationTerminalOutcome::Cancelled,
            cleanup_complete: true,
            active_effects: 0,
            active_tasks: 0,
            reason: "application preparation was cancelled before agent execution".to_owned(),
            finalized_at_ms: current_unix_time_ms(),
        })
        .map_err(ApplicationRunPrepareError::execution)?;
    Ok(ApplicationSessionBinding {
        session_scope_id: session.session_scope_id().to_owned(),
        session_log_path: canonical_path,
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

async fn attach_application_request_context(
    input: AgentRunInput,
    context_resolver: &crate::RequestContextResolver,
    prompt: &str,
) -> AgentRunInput {
    input.with_runtime_context(context_resolver.resolve(prompt).await.unwrap_or_default())
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
    tool_scope: Option<ToolRegistryScope>,
}

fn prepare_application_run_blocking(
    request: ApplicationRunRequest,
    session_leases: Arc<ApplicationSessionLeaseManager>,
) -> std::result::Result<BlockingApplicationRunPreparation, ApplicationRunPrepareError> {
    if let Some(constraints) = request.constraints.as_ref()
        && (constraints.max_turns == 0
            || constraints.max_output_tokens == 0
            || constraints.tool_scope.is_empty())
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "application run constraints must be non-zero and non-empty".to_owned(),
        });
    }
    let mut root_config = RootConfig::load(&request.config_path)
        .map_err(ApplicationRunPrepareError::configuration)?;
    let workspace_root = resolve_workspace_root(
        &request.config_path,
        &request.launch_cwd,
        &root_config.workspace.root,
    );
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let requested_session_path = request
        .session_path
        .clone()
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
    root_config.agent.provider = session.provider_name().to_owned();
    root_config.agent.model = session.model_name().to_owned();
    admit_application_reasoning_effort(&request, &root_config)?;
    let provider =
        crate::build_provider(&root_config).map_err(ApplicationRunPrepareError::configuration)?;
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
    let mut options = crate::build_run_options(
        &root_config,
        workspace_root.clone(),
        request.interaction.kernel_mode(),
    );
    if let Some(permission_mode) = request.permission_mode {
        options.permission_config.mode = permission_mode;
    }
    if let Some(reasoning_effort) = request.reasoning_effort {
        options.reasoning_effort = Some(reasoning_effort);
    }
    if let Some(constraints) = request.constraints.as_ref() {
        options.max_turns = Some(constraints.max_turns);
    }
    let mut input = AgentRunInput::user(request.prompt.clone())
        .with_logical_run_id(request.run_id.clone())
        .with_cancellation(cancellation_handle.clone());
    if let Some(constraints) = request.constraints.as_ref() {
        input = input.with_max_output_tokens(constraints.max_output_tokens);
    }
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
        tool_scope: request
            .constraints
            .map(|constraints| constraints.tool_scope),
    })
}

fn admit_application_reasoning_effort(
    request: &ApplicationRunRequest,
    root_config: &RootConfig,
) -> std::result::Result<(), ApplicationRunPrepareError> {
    match (
        request.reasoning_effort.as_ref(),
        request.reasoning_effort_binding.as_deref(),
    ) {
        (None, None) => return Ok(()),
        (None, Some(_)) | (Some(_), None) => {
            return Err(ApplicationRunPrepareError::InvalidInvocation {
                message: "reasoning effort and capability binding must be supplied together"
                    .to_owned(),
            });
        }
        (Some(_), Some(_)) => {}
    }
    let supported = crate::reasoning_effort::supported_reasoning_efforts(
        &root_config.agent.provider,
        &root_config.agent.model,
    );
    let expected_binding = crate::reasoning_effort::reasoning_effort_binding(
        &root_config.agent.provider,
        &root_config.agent.model,
        &supported,
    );
    if expected_binding.as_deref() != request.reasoning_effort_binding.as_deref() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "reasoning effort capability binding is stale".to_owned(),
        });
    }
    if request
        .reasoning_effort
        .as_ref()
        .is_none_or(|effort| !supported.contains(effort))
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "reasoning effort is unavailable for the bound provider and model".to_owned(),
        });
    }
    Ok(())
}

fn constrain_application_tool_registry(
    registry: sigil_kernel::ToolRegistry,
    scope: &ToolRegistryScope,
) -> Result<sigil_kernel::ToolRegistry> {
    if scope.is_empty() {
        bail!("application tool scope must not be empty");
    }
    for name in &scope.names {
        if registry.spec_for(name).is_none() {
            bail!("application tool scope contains unknown tool: {name}");
        }
    }
    for prefix in &scope.prefixes {
        if !registry
            .specs()
            .iter()
            .any(|spec| spec.name.starts_with(prefix))
        {
            bail!("application tool scope contains unmatched prefix: {prefix}");
        }
    }
    let scoped = registry.scoped(scope.clone()).into_registry();
    if scoped.specs().is_empty() {
        bail!("application tool scope produced an empty registry");
    }
    Ok(scoped)
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
        let sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        let terminal = is_terminal_public_run_event(&event);
        handler.handle_public_event(PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            sequence,
            event,
        ))?;
        state.sequence = sequence;
        if terminal {
            state.terminal = true;
        }
        Ok(())
    }

    fn terminal_was_delivered(&self) -> Result<bool> {
        self.state
            .lock()
            .map(|state| state.terminal)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
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
    match interaction {
        ApplicationRunInteraction::NonInteractive => {}
        ApplicationRunInteraction::AdapterManaged if !owned_blocking_worker => {
            bail!("adapter-managed runs require an owned blocking execution worker");
        }
        ApplicationRunInteraction::AdapterManaged => {}
        ApplicationRunInteraction::ExternallyInteractive if !owned_blocking_worker => {
            bail!("externally interactive runs require an owned blocking execution worker");
        }
        ApplicationRunInteraction::ExternallyInteractive
            if !approval_handler.approval_is_explicit_user_action() =>
        {
            bail!("externally interactive runs require an explicit-user-action approval handler");
        }
        ApplicationRunInteraction::ExternallyInteractive => {}
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
