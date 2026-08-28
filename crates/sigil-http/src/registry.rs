use std::{
    collections::{BTreeMap, BTreeSet},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    SessionRef, project_conversation_prompt_for_persistence, safe_persistence_text,
};
use thiserror::Error as ThisError;

use crate::{
    HttpCommandStoreError, HttpDurableCommandStore,
    command_store::{
        HTTP_DURABLE_COMMAND_PROMPT_OMISSION, HttpStoredCommandClaim, HttpStoredCommandCompletion,
        HttpStoredCommandIdentity, HttpStoredCommandKey,
    },
    driver::{
        HttpConversationDisplayDriverError, HttpConversationQueueDriverCommand,
        HttpConversationQueueDriverError, HttpConversationRecoveryDriverCommand,
        HttpConversationRecoveryDriverError, HttpIntentStackDriverError, HttpQueuedRunDriverStart,
        HttpRunAdmissionError, HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel,
        HttpRunDriverStart, HttpRunDriverTaskPause, HttpRunDriverTerminalTaskCancel,
        HttpSessionOpenBindingError, HttpToolArtifactReadDriverError,
        HttpUserInputDecisionDriverCommand,
    },
    dto::{
        HttpAgentActivityView, HttpApprovalCommandReceipt, HttpApprovalDecision,
        HttpApprovalDecisionRecord, HttpApprovalDecisionRequest, HttpApprovalLifecycleState,
        HttpApprovalLifecycleView, HttpApprovalRouteState, HttpCheckpointRestoreRequest,
        HttpCheckpointRestoreReview, HttpCompactionReview, HttpContinuityRecoveryAction,
        HttpConversationDisplayPage, HttpConversationQueueBlockedReason,
        HttpConversationQueueCommandAction, HttpConversationQueueCommandReceipt,
        HttpConversationQueueCommandRequest, HttpConversationQueuePromptMaterial,
        HttpConversationQueueView, HttpConversationRecoveryCommandAction,
        HttpConversationRecoveryCommandReceipt, HttpConversationRecoveryView,
        HttpForegroundRunOwner, HttpIntentDropCommandReceipt, HttpIntentDropPreview,
        HttpIntentDropPreviewRequest, HttpIntentDropRequest, HttpIntentStackView,
        HttpPendingApproval, HttpPermissionMode, HttpPlanDecisionCommandReceipt,
        HttpPlanDecisionRequest, HttpPlanReviewDetail, HttpReasoningEffort,
        HttpRunCancelCommandReceipt, HttpRunCancelRequest, HttpRunSnapshot,
        HttpRunStartCommandReceipt, HttpRunStartRequest, HttpRunStatus, HttpRunTerminalOutcome,
        HttpSessionBinding, HttpSessionContinuityView, HttpSessionCreateRequest,
        HttpSessionOpenRequest, HttpSessionRouteRecoveryAction, HttpSessionRouteRecoveryCode,
        HttpSessionRouteRecoveryView, HttpSessionSnapshot, HttpSessionTranscriptPage,
        HttpTaskIntegrationAcceptanceCommandReceipt, HttpTaskIntegrationReviewRequest,
        HttpTaskIntegrationReviewView, HttpTaskPauseCommandReceipt, HttpTaskPauseRequest,
        HttpTerminalLifecycleView, HttpTerminalTaskCancelCommandReceipt,
        HttpTerminalTaskCancelRequest, HttpToolArtifactPage, HttpToolArtifactReadRequest,
        HttpUserInputDecisionCommandReceipt, HttpUserInputDecisionRequest, HttpUserInputRequest,
        HttpVerificationRerunCommandReceipt, HttpVerificationRerunRequest, HttpVerificationView,
    },
    protocol::HttpCommandEnvelope,
};

const DEFAULT_IN_MEMORY_COMMAND_CAPACITY: usize = 4_096;
const MAX_SESSION_OPEN_REFERENCE_BYTES: usize = 512;
const MAX_SESSION_OPEN_ID_BYTES: usize = 512;
const MAX_SESSION_OPEN_LABEL_BYTES: usize = 160;
const MAX_CONVERSATION_QUEUE_ID_BYTES: usize = 512;
const MAX_CONVERSATION_QUEUE_PROMPT_BYTES: usize = 64 * 1024;
const MAX_CONVERSATION_RECOVERY_ID_BYTES: usize = 512;
const MAX_TASK_INTEGRATION_ID_BYTES: usize = 512;
const QUEUE_STALE_START_RESCHEDULE_LIMIT: usize = 3;
const MAX_PENDING_APPROVALS_PER_RUN: usize = 8;
const MAX_APPROVAL_LIFECYCLES_PER_RUN: usize = 32;
const MAX_CONTINUITY_TERMINAL_RUNS: usize = 16;

/// Errors returned by the HTTP session/run registry.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpRegistryError {
    /// The requested HTTP session does not exist.
    #[error("http session not found: {session_id}")]
    SessionNotFound { session_id: String },
    /// The requested HTTP run does not exist.
    #[error("http run not found: {run_id}")]
    RunNotFound { run_id: String },
    /// The run prompt is empty after trimming whitespace.
    #[error("http run start prompt must not be empty")]
    EmptyPrompt,
    /// The run did not include an explicit permission mode.
    #[error("http run start requires an explicit permission mode")]
    MissingPermissionMode,
    /// The Task continuation payload is empty, ambiguous, or combined with conversation fields.
    #[error("http Task continuation request is invalid")]
    InvalidTaskContinuation,
    /// The integration review identity is empty or exceeds the bounded command contract.
    #[error("http Task integration review request is invalid")]
    InvalidTaskIntegrationReview,
    /// The runtime driver could not establish a durable binding for the adapter session.
    #[error("http driver rejected durable binding for session {session_id}: {message}")]
    SessionBindingRejected { session_id: String, message: String },
    /// The runtime driver returned an invalid durable binding.
    #[error("http driver returned an invalid durable binding for session {session_id}: {message}")]
    InvalidSessionBinding { session_id: String, message: String },
    /// The durable reopen request did not contain a bounded direct-child reference and identity.
    #[error("durable session open request is invalid")]
    InvalidSessionOpenRequest,
    /// The requested durable source does not exist in current workspace truth.
    #[error("durable session was not found")]
    DurableSessionNotFound,
    /// The durable source exists but cannot currently be reopened as a ready V2 stream.
    #[error("durable session is not ready")]
    DurableSessionNotReady,
    /// The durable source identity changed after the client selected its catalog row.
    #[error("durable session identity changed")]
    DurableSessionIdentityChanged,
    /// Current lifecycle or durable stream validation could not complete.
    #[error("durable session is unavailable")]
    DurableSessionUnavailable,
    /// A durable catalog mutation already owns this session identity.
    #[error("durable session is being changed")]
    DurableSessionMutationActive,
    /// Another foreground run still owns this adapter session.
    #[error("http session {session_id} already has foreground run {run_id}")]
    SessionForegroundRunActive { session_id: String, run_id: String },
    /// Durable user input owns the session frontier while its continuation is unresolved.
    #[error("http session {session_id} is awaiting user input")]
    SessionAwaitingUserInput { session_id: String },
    /// The addressed user-input generation/hash no longer matches durable truth.
    #[error("http user input request is stale")]
    UserInputStale,
    /// A terminal run still owns process-local cleanup or the runtime session lease.
    #[error("http session {session_id} is still releasing run {run_id}")]
    SessionRunCleanupActive { session_id: String, run_id: String },
    /// Write admission or route recovery rejected the run before a run id was allocated.
    #[error("session run requires explicit recovery")]
    SessionRunRecoveryRequired {
        recovery: HttpSessionRouteRecoveryView,
    },
    /// A verification rerun already owns the session's foreground mutation lease.
    #[error("http session {session_id} already has an active verification rerun")]
    SessionVerificationActive { session_id: String },
    /// The run cannot accept this operation in its current state.
    #[error("http run {run_id} is not active")]
    RunNotActive { run_id: String },
    /// The exact Task pause request identity is malformed.
    #[error("http Task pause request is invalid")]
    InvalidTaskPauseRequest,
    /// The persistent terminal cancellation request is malformed.
    #[error("http terminal task cancellation request is invalid")]
    InvalidTerminalTaskCancelRequest,
    /// The addressed terminal task is absent from this run's bounded owner projection.
    #[error("http terminal task {task_id} was not found for run {run_id}")]
    TerminalTaskNotFound { run_id: String, task_id: String },
    /// The rendered terminal generation no longer matches current owner truth.
    #[error("http terminal task {task_id} generation changed for run {run_id}")]
    TerminalTaskGenerationChanged { run_id: String, task_id: String },
    /// The Intent Stack request body is malformed.
    #[error("http Intent Stack request is invalid")]
    IntentStackInvalidRequest,
    /// The exact Intent Stack request no longer matches current durable truth.
    #[error("http Intent Stack request is stale")]
    IntentStackStale,
    /// Current host permission or workspace trust does not authorize Intent Drop.
    #[error("http Intent Stack operation requires permission")]
    IntentStackPermissionRequired,
    /// Current durable state conflicts with the requested Intent operation.
    #[error("http Intent Stack operation conflicts with durable state")]
    IntentStackConflict,
    /// Durable Intent Stack projection or its application owner is unavailable.
    #[error("http Intent Stack is unavailable")]
    IntentStackUnavailable,
    /// The addressed run no longer owns the session's foreground event stream.
    #[error("http run {run_id} no longer owns foreground session {session_id}")]
    RunNoLongerForeground { session_id: String, run_id: String },
    /// The caller's opaque foreground owner revision is stale.
    #[error("http run {run_id} foreground owner changed for session {session_id}")]
    RunOwnerChanged { session_id: String, run_id: String },
    /// The approval request is malformed or the family rule could not be persisted.
    #[error("http approval request invalid: {message}")]
    InvalidApprovalRequest { message: String },
    /// The approval call id is not currently pending for the run.
    #[error("http approval not pending for run {run_id} call {call_id}")]
    ApprovalNotPending { run_id: String, call_id: String },
    /// The bounded run snapshot cannot safely retain another distinct pending approval.
    #[error("http run {run_id} reached the pending approval limit")]
    ApprovalCapacityExceeded { run_id: String },
    /// The underlying run driver rejected the registry operation.
    #[error("http driver rejected {operation} for run {run_id}: {message}")]
    DriverRejected {
        operation: &'static str,
        run_id: String,
        message: String,
    },
    /// The opaque canonical-display cursor is malformed or bound to another request scope.
    #[error("conversation display cursor is invalid")]
    ConversationDisplayCursorInvalid,
    /// The opaque canonical-display cursor no longer matches retained durable truth.
    #[error("conversation display cursor is stale")]
    ConversationDisplayCursorStale,
    /// The canonical durable display projection could not be proven safely.
    #[error("conversation display projection is unavailable")]
    ConversationDisplayUnavailable,
    /// The opaque tool artifact reference is malformed.
    #[error("tool artifact reference is invalid")]
    ToolArtifactReferenceInvalid,
    /// The tool artifact selector exceeds a fixed retrieval bound.
    #[error("tool artifact selector is invalid")]
    ToolArtifactSelectorInvalid,
    /// The tool artifact is absent, expired, or outside this logical session scope.
    #[error("tool artifact is unavailable")]
    ToolArtifactUnavailable,
    /// The tool artifact bytes do not match the durable descriptor.
    #[error("tool artifact failed integrity validation")]
    ToolArtifactCorrupt,
    /// The artifact retrieval policy does not authorize this display surface.
    #[error("tool artifact retrieval is not authorized")]
    ToolArtifactPolicyRevoked,
    /// The caller's queue generation no longer matches durable queue truth.
    #[error("conversation queue generation is stale")]
    ConversationQueueGenerationStale,
    /// The queue command contains an empty or over-bounded exact field.
    #[error("conversation queue command is invalid")]
    ConversationQueueInvalidCommand,
    /// The addressed queue entry already reached a terminal state.
    #[error("conversation queue entry is terminal")]
    ConversationQueueEntryTerminal,
    /// The foreground owner changed before an interrupt-and-run-next command was admitted.
    #[error("conversation queue foreground owner changed")]
    ConversationQueueOwnerLost,
    /// Current policy does not authorize the queue operation.
    #[error("conversation queue operation requires permission")]
    ConversationQueuePermissionRequired,
    /// The queue operation conflicts with current durable state.
    #[error("conversation queue operation conflicts with durable state")]
    ConversationQueueConflict,
    /// Exact prompt material was intentionally lost and must be entered again.
    #[error("conversation queue prompt requires reentry")]
    ConversationQueueRequiresReentry,
    /// The requested queue action is not supported by the application owner.
    #[error("conversation queue operation is unsupported")]
    ConversationQueueUnsupported,
    /// Durable queue truth or its application owner is unavailable.
    #[error("conversation queue is unavailable")]
    ConversationQueueUnavailable,
    /// The recovery command contains an empty or over-bounded exact identity.
    #[error("conversation recovery command is invalid")]
    ConversationRecoveryInvalidCommand,
    /// The checkpoint digest or fork turn digest no longer matches durable truth.
    #[error("conversation recovery binding is stale")]
    ConversationRecoveryStaleBinding,
    /// Fresh workspace or lifecycle truth prevents the requested recovery mutation.
    #[error("conversation recovery conflicts with current durable state")]
    ConversationRecoveryConflict,
    /// Durable recovery truth or its application owner is unavailable.
    #[error("conversation recovery is unavailable")]
    ConversationRecoveryUnavailable,
    /// The driver unwound after the registry had published a tentative operation.
    #[error("http driver panicked during {operation} for run {run_id}")]
    DriverPanicked {
        operation: &'static str,
        run_id: String,
    },
    /// The command envelope version is not supported.
    #[error("http command protocol version rejected: {message}")]
    UnsupportedProtocolVersion { message: String },
    /// The command envelope points to a different session than the addressed run.
    #[error(
        "http command session {command_session_id} does not match run {run_id} session {run_session_id}"
    )]
    CommandSessionMismatch {
        command_session_id: String,
        run_id: String,
        run_session_id: String,
    },
    /// The command envelope points to a different session than the addressed URL.
    #[error(
        "http command session {command_session_id} does not match path session {path_session_id}"
    )]
    CommandPathSessionMismatch {
        command_session_id: String,
        path_session_id: String,
    },
    /// The command was based on an older run stream sequence.
    #[error(
        "http command for run {run_id} is stale: expected stream sequence {expected}, current is {actual}"
    )]
    StaleCommandSequence {
        run_id: String,
        expected: u64,
        actual: u64,
    },
    /// The approval request id no longer matches the pending request.
    #[error("http approval request changed for run {run_id} call {call_id}")]
    ApprovalRequestChanged { run_id: String, call_id: String },
    /// The approval tool call hash no longer matches the pending request.
    #[error("http approval tool call changed for run {run_id} call {call_id}")]
    ApprovalToolCallChanged { run_id: String, call_id: String },
    /// The approval policy version no longer matches the pending request.
    #[error("http approval policy changed for run {run_id} call {call_id}")]
    ApprovalPolicyChanged { run_id: String, call_id: String },
    /// The approval expiry no longer matches the pending request.
    #[error("http approval expiry changed for run {run_id} call {call_id}")]
    ApprovalExpiryChanged { run_id: String, call_id: String },
    /// The requested decision is not available for this pending approval.
    #[error("http approval decision is unavailable for run {run_id} call {call_id}")]
    ApprovalDecisionUnavailable { run_id: String, call_id: String },
    /// The approval request expired before the user decision arrived.
    #[error("http approval expired for run {run_id} call {call_id}")]
    ApprovalExpired { run_id: String, call_id: String },
    /// A command key was reused with another operation, target, payload, or guard.
    #[error(
        "http command key conflict for session {session_id} client {client_id} command {command_id}"
    )]
    CommandKeyConflict {
        session_id: String,
        client_id: String,
        command_id: String,
    },
    /// A terminal callback contradicted an already recorded terminal outcome.
    #[error("http run {run_id} terminal conflict: current {current:?}, requested {requested:?}")]
    RunTerminalConflict {
        run_id: String,
        current: HttpRunStatus,
        requested: HttpRunTerminalOutcome,
    },
    /// A reserved command executor unwound before publishing an outcome.
    #[error("http command execution aborted before publishing its receipt")]
    CommandExecutionAborted,
    /// The parsed command could not be encoded for stable identity comparison.
    #[error("http command identity could not be encoded")]
    CommandIdentityEncodingFailed,
    /// The bounded fail-closed command identity window has reached capacity.
    #[error("http command registry is at its bounded identity capacity")]
    CommandRegistrySaturated,
    /// Durable command identity state could not be reserved or completed safely.
    #[error("http command identity persistence failed: {message}")]
    CommandIdentityPersistenceFailed { message: String },
    /// Graceful shutdown has stopped admission of new commands.
    #[error("http server is shutting down and is not accepting new commands")]
    ServerShuttingDown,
}

/// In-memory registry for HTTP adapter sessions, runs, cancellations, and approvals.
pub struct HttpSessionRunRegistry {
    state: Mutex<HttpRegistryState>,
    queue_command_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    driver: Arc<dyn HttpRunDriver>,
    command_store: Option<Arc<HttpDurableCommandStore>>,
    /// Persisted config path used to append `permission.commands.allow` family rules.
    config_path: Option<PathBuf>,
    in_memory_command_capacity: usize,
}

/// Process-local registry activity used by graceful-drain and concurrency diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpRegistryActivity {
    /// Retained in-flight and completed command identities.
    pub retained_commands: usize,
    /// Commands that have not published a completion yet.
    pub in_flight_commands: usize,
    /// Duplicate command callers currently waiting for a completion.
    pub command_waiters: usize,
    /// Distinct cancellation callers currently sharing one per-run driver operation.
    pub cancellation_waiters: usize,
}

impl HttpSessionRunRegistry {
    /// Creates a registry that delegates execution to `driver`.
    #[must_use]
    pub fn new(driver: Arc<dyn HttpRunDriver>) -> Self {
        Self {
            state: Mutex::new(HttpRegistryState::default()),
            queue_command_locks: Mutex::new(BTreeMap::new()),
            driver,
            command_store: None,
            config_path: None,
            in_memory_command_capacity: DEFAULT_IN_MEMORY_COMMAND_CAPACITY,
        }
    }

    /// Binds the persisted config path so `approve_for_family` decisions can append durable
    /// `permission.commands.allow` rules. Unit-test registries leave it unset and reject the
    /// family decision.
    #[must_use]
    pub fn with_config_path(mut self, config_path: PathBuf) -> Self {
        self.config_path = Some(config_path);
        self
    }

    /// Creates a production registry backed by a crash-safe command identity store.
    #[must_use]
    pub fn with_durable_command_store(
        driver: Arc<dyn HttpRunDriver>,
        command_store: Arc<HttpDurableCommandStore>,
    ) -> Self {
        let id_namespace = format!("e{}", command_store.server_epoch());
        Self {
            state: Mutex::new(HttpRegistryState::with_id_namespace(id_namespace)),
            queue_command_locks: Mutex::new(BTreeMap::new()),
            driver,
            command_store: Some(command_store),
            config_path: None,
            in_memory_command_capacity: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_in_memory_command_capacity(
        driver: Arc<dyn HttpRunDriver>,
        capacity: usize,
    ) -> Self {
        Self {
            state: Mutex::new(HttpRegistryState::default()),
            queue_command_locks: Mutex::new(BTreeMap::new()),
            driver,
            command_store: None,
            config_path: None,
            in_memory_command_capacity: capacity,
        }
    }

    /// Creates one HTTP adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime driver cannot establish a valid durable V2 session
    /// scope and absolute JSONL path.
    pub fn create_session(
        &self,
        request: HttpSessionCreateRequest,
    ) -> Result<HttpSessionSnapshot, HttpRegistryError> {
        let id = {
            let mut state = self.lock_state();
            state.ensure_accepting_commands()?;
            state.next_session_id()
        };
        let binding = self
            .driver
            .bind_session(&id, request.model_ref.as_ref())
            .map_err(|error| HttpRegistryError::SessionBindingRejected {
                session_id: id.clone(),
                message: error.message,
            })?;
        validate_session_binding(&id, &binding)?;
        let route_transition = binding.route_transition.clone();
        let mut state = self.lock_state();
        let session = HttpSessionState {
            id: id.clone(),
            label: request.label,
            run_ids: Vec::new(),
            binding,
            foreground_run_id: None,
            release_pending_run_id: None,
            foreground_owner_generation: 0,
            verification_in_progress: false,
            route_transition,
        };
        let snapshot = session.snapshot();
        state.sessions.insert(id, session);
        Ok(snapshot)
    }

    /// Reopens one current-workspace durable source as a process-local adapter session.
    ///
    /// Repeating the same proven durable scope returns the existing process-local snapshot. This
    /// operation creates no run and delegates source authorization to the runtime driver.
    ///
    /// # Errors
    ///
    /// Returns a bounded typed error for malformed identity, missing/non-ready/drifted sources,
    /// unavailable lifecycle truth, invalid driver bindings, or shutdown admission.
    pub fn open_session(
        &self,
        request: HttpSessionOpenRequest,
    ) -> Result<HttpSessionSnapshot, HttpRegistryError> {
        {
            let state = self.lock_state();
            state.ensure_accepting_commands()?;
        }
        let session_ref = validate_session_open_request(&request)?;
        let binding = self
            .driver
            .bind_existing_session(
                &session_ref,
                &request.session_id,
                request.recovery_binding.as_deref(),
            )
            .map_err(|error| match error {
                HttpSessionOpenBindingError::NotFound => HttpRegistryError::DurableSessionNotFound,
                HttpSessionOpenBindingError::NotReady => HttpRegistryError::DurableSessionNotReady,
                HttpSessionOpenBindingError::IdentityChanged => {
                    HttpRegistryError::DurableSessionIdentityChanged
                }
                HttpSessionOpenBindingError::AlreadyActive { recovery_binding } => {
                    http_run_admission_registry_error(HttpRunAdmissionError::SessionAlreadyActive {
                        recovery_binding,
                    })
                }
                HttpSessionOpenBindingError::Unavailable => {
                    HttpRegistryError::DurableSessionUnavailable
                }
            })?;
        let snapshot = {
            let mut state = self.lock_state();
            state.ensure_accepting_commands()?;
            if let Some(existing) = state
                .sessions
                .values_mut()
                .find(|session| session.binding.session_scope_id == binding.session_scope_id)
            {
                if existing.binding.session_log_path != binding.session_log_path {
                    return Err(HttpRegistryError::InvalidSessionBinding {
                        session_id: existing.id.clone(),
                        message: "durable scope resolved to another canonical path".to_owned(),
                    });
                }
                existing.route_transition = binding.route_transition.clone();
                existing.binding = binding;
                existing.snapshot()
            } else {
                let id = state.next_session_id();
                validate_session_binding(&id, &binding)?;
                let route_transition = binding.route_transition.clone();
                let session = HttpSessionState {
                    id: id.clone(),
                    label: request.label,
                    run_ids: Vec::new(),
                    binding,
                    foreground_run_id: None,
                    release_pending_run_id: None,
                    foreground_owner_generation: 0,
                    verification_in_progress: false,
                    route_transition,
                };
                let snapshot = session.snapshot();
                state.sessions.insert(id, session);
                snapshot
            }
        };
        // Reopening is also the restart recovery trigger for accepted user input and durable safe
        // queued work. Recovery failure must not hide an otherwise valid historical session from
        // the client; the unresolved snapshot remains visible and a later reopen retries it.
        if let Ok(Some(recovery)) = self.driver.recoverable_session_attention_command(&snapshot) {
            let _ = self.resume_recoverable_session_attention(&snapshot, &recovery);
        }
        let _ = self.schedule_next_queued_run(&snapshot.id);
        Ok(snapshot)
    }

    /// Re-enters a continuation whose answer is already durable without consulting the adapter
    /// command receipt cache.
    ///
    /// The original decision command may already be retained as `Completed` in this adapter while
    /// the durable request still proves that no continuation started. Replaying that receipt would
    /// strand the accepted answer. The session reducer is the authority for this recovery path:
    /// the driver only returns a command while the exact request remains `DecisionAccepted`, and
    /// the durable-session mutation guard serializes re-entry with every other foreground
    /// mutation.
    fn resume_recoverable_session_attention(
        &self,
        session: &HttpSessionSnapshot,
        recovery: &HttpUserInputDecisionDriverCommand,
    ) -> Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError> {
        let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.driver.user_input_decision(session, recovery)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "User input recovery",
            run_id: session.id.clone(),
        })?
        .map_err(|error| match error.kind {
            crate::HttpRunDriverErrorKind::StaleUserInput => HttpRegistryError::UserInputStale,
            crate::HttpRunDriverErrorKind::General => HttpRegistryError::DriverRejected {
                operation: "User input recovery",
                run_id: session.id.clone(),
                message: error.message,
            },
        });
        guard.finish(false);
        result
    }

    /// Lists HTTP adapter sessions in deterministic id order.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<HttpSessionSnapshot> {
        let state = self.lock_state();
        state
            .sessions
            .values()
            .map(HttpSessionState::snapshot)
            .collect()
    }

    pub(crate) fn reserve_durable_session_mutation(
        &self,
        durable_session_id: &str,
    ) -> Result<HttpDurableSessionMutationGuard<'_>, HttpRegistryError> {
        let mut state = self.lock_state();
        state.ensure_accepting_commands()?;
        if state.durable_session_mutations.contains(durable_session_id)
            || state
                .active_queue_session_commands
                .contains(durable_session_id)
        {
            return Err(HttpRegistryError::DurableSessionMutationActive);
        }
        for session in state
            .sessions
            .values()
            .filter(|session| session.binding.session_scope_id == durable_session_id)
        {
            if let Some(run_id) = session.foreground_run_id.as_ref() {
                return Err(HttpRegistryError::SessionForegroundRunActive {
                    session_id: session.id.clone(),
                    run_id: run_id.clone(),
                });
            }
            if let Some(run_id) = session.release_pending_run_id.as_ref() {
                return Err(HttpRegistryError::SessionRunCleanupActive {
                    session_id: session.id.clone(),
                    run_id: run_id.clone(),
                });
            }
            if session.verification_in_progress {
                return Err(HttpRegistryError::SessionVerificationActive {
                    session_id: session.id.clone(),
                });
            }
        }
        state
            .durable_session_mutations
            .insert(durable_session_id.to_owned());
        Ok(HttpDurableSessionMutationGuard {
            registry: self,
            durable_session_id: durable_session_id.to_owned(),
            released: false,
        })
    }

    /// Claims the session foreground slot for a supervised background run (plan review revision).
    ///
    /// The slot blocks concurrent durable mutations, queued commands, and new foreground runs for
    /// the whole revision, mirroring the reservation a foreground run holds while it executes.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is missing or already owns a foreground run.
    pub(crate) fn bind_supervised_session_run(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<(), HttpRegistryError> {
        let mut state = self.lock_state();
        state.ensure_accepting_commands()?;
        let session = state.sessions.get_mut(session_id).ok_or_else(|| {
            HttpRegistryError::SessionNotFound {
                session_id: session_id.to_owned(),
            }
        })?;
        if let Some(existing) = session.foreground_run_id.as_ref() {
            return Err(HttpRegistryError::SessionForegroundRunActive {
                session_id: session_id.to_owned(),
                run_id: existing.clone(),
            });
        }
        session.foreground_run_id = Some(run_id.to_owned());
        session.foreground_owner_generation = session.foreground_owner_generation.saturating_add(1);
        Ok(())
    }

    /// Atomically registers an adapter-visible supervised continuation and claims its foreground
    /// slot. The caller must either start an owned worker or invoke
    /// [`Self::rollback_supervised_session_run_registration`] before returning an error.
    pub(crate) fn register_supervised_session_run(
        &self,
        session_id: &str,
        run_id: &str,
        permission_mode: HttpPermissionMode,
        prompt_preview: &str,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let mut state = self.lock_state();
        state.ensure_accepting_commands()?;
        if state.runs.contains_key(run_id) {
            return Err(HttpRegistryError::DriverRejected {
                operation: "register supervised run",
                run_id: run_id.to_owned(),
                message: "run identity is already registered".to_owned(),
            });
        }
        let durable_session_id = state
            .sessions
            .get(session_id)
            .ok_or_else(|| HttpRegistryError::SessionNotFound {
                session_id: session_id.to_owned(),
            })?
            .binding
            .session_scope_id
            .clone();
        if !state
            .durable_session_mutations
            .contains(&durable_session_id)
        {
            return Err(HttpRegistryError::DurableSessionMutationActive);
        }
        let session = state.sessions.get_mut(session_id).ok_or_else(|| {
            HttpRegistryError::SessionNotFound {
                session_id: session_id.to_owned(),
            }
        })?;
        if let Some(existing) = session.foreground_run_id.as_ref() {
            return Err(HttpRegistryError::SessionForegroundRunActive {
                session_id: session_id.to_owned(),
                run_id: existing.clone(),
            });
        }
        if let Some(existing) = session.release_pending_run_id.as_ref() {
            return Err(HttpRegistryError::SessionRunCleanupActive {
                session_id: session_id.to_owned(),
                run_id: existing.clone(),
            });
        }
        let mut run = HttpRunState::new(
            run_id.to_owned(),
            session_id.to_owned(),
            permission_mode,
            None,
            safe_persistence_text(prompt_preview),
        );
        run.status = HttpRunStatus::Running;
        let snapshot = run.snapshot();
        session.run_ids.push(run_id.to_owned());
        session.foreground_run_id = Some(run_id.to_owned());
        session.foreground_owner_generation = session.foreground_owner_generation.saturating_add(1);
        state.runs.insert(run_id.to_owned(), run);
        Ok(snapshot)
    }

    /// Rolls back a supervised continuation registration before its worker starts.
    pub(crate) fn rollback_supervised_session_run_registration(
        &self,
        session_id: &str,
        run_id: &str,
    ) {
        let mut state = self.lock_state();
        if state
            .runs
            .get(run_id)
            .is_some_and(|run| run.session_id == session_id && run.status == HttpRunStatus::Running)
        {
            state.runs.remove(run_id);
            if let Some(session) = state.sessions.get_mut(session_id) {
                session.run_ids.retain(|current| current != run_id);
                if session.foreground_run_id.as_deref() == Some(run_id) {
                    session.foreground_run_id = None;
                    session.foreground_owner_generation =
                        session.foreground_owner_generation.saturating_add(1);
                }
            }
        }
    }

    /// Releases the session foreground slot claimed by a supervised background run.
    pub(crate) fn unbind_supervised_session_run(&self, session_id: &str, run_id: &str) {
        let mut state = self.lock_state();
        if let Some(session) = state.sessions.get_mut(session_id)
            && session.foreground_run_id.as_deref() == Some(run_id)
        {
            session.foreground_run_id = None;
            session.foreground_owner_generation =
                session.foreground_owner_generation.saturating_add(1);
        }
    }

    pub(crate) fn acquire_durable_session_mutation_attachment(
        &self,
        durable_session_id: &str,
        session_log_path: &Path,
    ) -> Result<crate::HttpDurableSessionAttachmentGuard, HttpRegistryError> {
        self.driver
            .acquire_durable_session_mutation_attachment(durable_session_id, session_log_path)
            .map_err(http_run_admission_registry_error)
    }

    pub(crate) fn durable_session_mutation_is_blocked(&self, durable_session_id: &str) -> bool {
        let state = self.lock_state();
        if !state.accepting_commands
            || state.durable_session_mutations.contains(durable_session_id)
            || state
                .active_queue_session_commands
                .contains(durable_session_id)
        {
            return true;
        }
        state.sessions.values().any(|session| {
            session.binding.session_scope_id == durable_session_id
                && (session.foreground_run_id.is_some()
                    || session.release_pending_run_id.is_some()
                    || session.verification_in_progress)
        })
    }

    /// Returns one HTTP adapter session snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when `session_id` is unknown.
    pub fn get_session(&self, session_id: &str) -> Result<HttpSessionSnapshot, HttpRegistryError> {
        let state = self.lock_state();
        state
            .sessions
            .get(session_id)
            .map(HttpSessionState::snapshot)
            .ok_or_else(|| HttpRegistryError::SessionNotFound {
                session_id: session_id.to_owned(),
            })
    }

    /// Builds the host-bound transport-neutral application client for one HTTP session.
    ///
    /// The client is created only after this registry resolves the process-local session handle;
    /// the driver then injects the current application scope, managed journals, and live
    /// connection identity. The HTTP payload cannot select any of those values.
    pub(crate) fn application_client(
        &self,
        session_id: &str,
        client_id: &str,
    ) -> Result<crate::application_bridge::HttpApplicationClient, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        self.driver
            .application_client(&session, client_id)
            .map_err(|error| HttpRegistryError::DriverRejected {
                operation: "application client",
                run_id: session_id.to_owned(),
                message: error.message,
            })
    }

    pub(crate) fn session_foreground_owner(
        &self,
        session_id: &str,
    ) -> Result<Option<HttpForegroundRunOwner>, HttpRegistryError> {
        let state = self.lock_state();
        let session =
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?;
        Ok(session.foreground_owner())
    }

    pub(crate) fn record_session_route_transition(
        &self,
        durable_session_scope_id: &str,
        transition: crate::HttpSessionRouteTransitionView,
    ) -> Result<(), HttpRegistryError> {
        let mut state = self.lock_state();
        let session = state
            .sessions
            .values_mut()
            .find(|session| session.binding.session_scope_id == durable_session_scope_id)
            .ok_or_else(|| HttpRegistryError::SessionNotFound {
                session_id: durable_session_scope_id.to_owned(),
            })?;
        session.route_transition = Some(transition.clone());
        session.binding.route_transition = Some(transition);
        session.binding.route_recovery = None;
        Ok(())
    }

    /// Probes durable frontier and process-local foreground ownership for one session.
    ///
    /// The durable projection is read before the final owner snapshot. Attach admission must echo
    /// the returned opaque owner revision and re-probe immediately before opening a live follower.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown or the durable frontier cannot be proven.
    pub fn session_continuity(
        &self,
        session_id: &str,
    ) -> Result<HttpSessionContinuityView, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        let durable_frontier =
            catch_unwind(AssertUnwindSafe(|| self.driver.session_frontier(&session)))
                .map_err(|_| HttpRegistryError::DriverPanicked {
                    operation: "continuity frontier",
                    run_id: session_id.to_owned(),
                })?
                .map_err(|error| HttpRegistryError::DriverRejected {
                    operation: "continuity frontier",
                    run_id: session_id.to_owned(),
                    message: error.message,
                })?;
        let state = self.lock_state();
        let current =
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?;
        let foreground_owner = current.foreground_owner();
        let terminal_run_candidates = current
            .run_ids
            .iter()
            .enumerate()
            .filter_map(|(index, run_id)| state.runs.get(run_id).map(|run| (index, run)))
            .filter(|(_, run)| !run.terminal_tasks.is_empty())
            .collect::<Vec<_>>();
        let mut selected = terminal_run_candidates
            .iter()
            .rev()
            .filter(|(_, run)| {
                run.terminal_tasks
                    .values()
                    .any(|task| !task.status.is_terminal())
            })
            .take(MAX_CONTINUITY_TERMINAL_RUNS)
            .copied()
            .collect::<Vec<_>>();
        let mut selected_ids = selected
            .iter()
            .map(|(_, run)| run.id.clone())
            .collect::<BTreeSet<_>>();
        for candidate in terminal_run_candidates.iter().rev() {
            if selected.len() >= MAX_CONTINUITY_TERMINAL_RUNS {
                break;
            }
            if selected_ids.insert(candidate.1.id.clone()) {
                selected.push(*candidate);
            }
        }
        selected.sort_by_key(|(index, _)| *index);
        let retained_terminal_runs = selected
            .into_iter()
            .map(|(_, run)| run.snapshot())
            .collect::<Vec<_>>();
        let recovery_actions = if foreground_owner.is_some() {
            vec![
                HttpContinuityRecoveryAction::RetryCurrent,
                HttpContinuityRecoveryAction::ContinueReadOnly,
            ]
        } else {
            Vec::new()
        };
        Ok(HttpSessionContinuityView {
            durable_session_scope_id: current.binding.session_scope_id.clone(),
            durable_frontier,
            foreground_owner,
            retained_terminal_runs,
            recovery_actions,
        })
    }

    /// Admits one exact replay-plus-live follower against the current foreground owner.
    ///
    /// The session, run and opaque owner revision are compared under one registry state lock.
    /// Subscribe to the event bus before invoking this method so a terminal event racing after
    /// this admission linearization point cannot be missed.
    ///
    /// # Errors
    ///
    /// Returns an error when the session or run is unknown, the run is not the current foreground
    /// owner, or the supplied owner revision is stale.
    pub fn admit_run_event_stream(
        &self,
        session_id: &str,
        run_id: &str,
        owner_revision: &str,
    ) -> Result<(HttpSessionSnapshot, HttpRunSnapshot), HttpRegistryError> {
        let state = self.lock_state();
        let run = state
            .runs
            .get(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        let session =
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?;
        if run.session_id != session_id || session.foreground_run_id.as_deref() != Some(run_id) {
            return Err(HttpRegistryError::RunNoLongerForeground {
                session_id: session_id.to_owned(),
                run_id: run_id.to_owned(),
            });
        }
        if session.foreground_owner_revision() != owner_revision {
            return Err(HttpRegistryError::RunOwnerChanged {
                session_id: session_id.to_owned(),
                run_id: run_id.to_owned(),
            });
        }
        Ok((session.snapshot(), run.snapshot()))
    }

    /// Projects one bounded chronological transcript page for an existing adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, the durable scope drifted, or safe projection
    /// cannot complete.
    pub fn transcript_page(
        &self,
        session_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<HttpSessionTranscriptPage, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| {
            self.driver.transcript_page(&session, before, limit)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "transcript view",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| HttpRegistryError::DriverRejected {
            operation: "transcript view",
            run_id: session_id.to_owned(),
            message: error.message,
        })
    }

    /// Projects one canonical durable conversation page and validates its foreground anchor.
    ///
    /// The driver owns durable scope validation and the exact public run-event watermark. The
    /// registry never guesses a live sequence from its command stream; it only clears an anchor
    /// whose run no longer owns the session after projection completes.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, the cursor is stale, or durable projection
    /// cannot be proven safely.
    pub fn conversation_display_page(
        &self,
        session_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HttpConversationDisplayPage, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        let mut page = catch_unwind(AssertUnwindSafe(|| {
            self.driver
                .conversation_display_page(&session, cursor, limit)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "conversation display",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| match error {
            HttpConversationDisplayDriverError::InvalidCursor => {
                HttpRegistryError::ConversationDisplayCursorInvalid
            }
            HttpConversationDisplayDriverError::StaleCursor => {
                HttpRegistryError::ConversationDisplayCursorStale
            }
            HttpConversationDisplayDriverError::Unavailable => {
                HttpRegistryError::ConversationDisplayUnavailable
            }
        })?;

        let state = self.lock_state();
        let current =
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?;
        if page
            .live_provisional_anchor
            .as_ref()
            .is_some_and(|anchor| current.foreground_run_id.as_deref() != Some(&anchor.run_id))
        {
            page.live_provisional_anchor = None;
        }
        Ok(page)
    }

    /// Reads one typed artifact page after resolving the opaque reference in the addressed session.
    ///
    /// # Errors
    ///
    /// Returns a bounded typed rejection when the request, session scope, policy, or integrity
    /// proof cannot be validated.
    pub fn tool_artifact_page(
        &self,
        session_id: &str,
        request: &HttpToolArtifactReadRequest,
    ) -> Result<HttpToolArtifactPage, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        let page = catch_unwind(AssertUnwindSafe(|| {
            self.driver.tool_artifact_page(&session, request)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "tool artifact read",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| match error {
            HttpToolArtifactReadDriverError::InvalidReference => {
                HttpRegistryError::ToolArtifactReferenceInvalid
            }
            HttpToolArtifactReadDriverError::InvalidSelector => {
                HttpRegistryError::ToolArtifactSelectorInvalid
            }
            HttpToolArtifactReadDriverError::Unavailable => {
                HttpRegistryError::ToolArtifactUnavailable
            }
            HttpToolArtifactReadDriverError::Corrupt => HttpRegistryError::ToolArtifactCorrupt,
            HttpToolArtifactReadDriverError::PolicyRevoked => {
                HttpRegistryError::ToolArtifactPolicyRevoked
            }
        })?;
        page.validate(session_id, request)
            .map_err(|_| HttpRegistryError::ToolArtifactUnavailable)?;
        Ok(page)
    }

    /// Projects typed run configuration and context usage for an existing adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, its durable scope drifted, or projection
    /// cannot complete.
    pub fn run_context_view(
        &self,
        session_id: &str,
    ) -> Result<crate::HttpRunContextView, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| self.driver.run_context_view(&session)))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "run-context view",
                run_id: session_id.to_owned(),
            })?
            .map_err(|error| HttpRegistryError::DriverRejected {
                operation: "run-context view",
                run_id: session_id.to_owned(),
                message: error.message,
            })
    }

    /// Projects safe, bounded child-agent lifecycle and result-handoff state.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, its durable scope drifted, or projection
    /// cannot complete.
    pub fn agent_activity_view(
        &self,
        session_id: &str,
    ) -> Result<HttpAgentActivityView, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| {
            self.driver.agent_activity_view(&session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "agent activity view",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| HttpRegistryError::DriverRejected {
            operation: "agent activity view",
            run_id: session_id.to_owned(),
            message: error.message,
        })
    }

    /// Projects checkpoint restore and conversation-fork choices from durable session truth.
    ///
    /// # Errors
    ///
    /// Returns an error when the adapter session is unknown or the application owner cannot
    /// validate the durable recovery stream.
    pub fn conversation_recovery(
        &self,
        session_id: &str,
    ) -> Result<HttpConversationRecoveryView, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| {
            self.driver.conversation_recovery_view(&session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "conversation recovery view",
            run_id: session_id.to_owned(),
        })?
        .map_err(recovery_driver_registry_error)
    }

    /// Produces a fresh reverse-diff and conflict review without mutating workspace truth.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown sessions, active durable mutations, stale checkpoint
    /// bindings, or unavailable application recovery state.
    pub fn checkpoint_restore_review(
        &self,
        session_id: &str,
        request: HttpCheckpointRestoreRequest,
    ) -> Result<HttpCheckpointRestoreReview, HttpRegistryError> {
        validate_checkpoint_restore_request(&request)?;
        let session = self.get_session(session_id)?;
        let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.driver.checkpoint_restore_review(&session, &request)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "checkpoint restore preview",
            run_id: session_id.to_owned(),
        })?
        .map_err(recovery_driver_registry_error);
        guard.finish(false);
        result
    }

    /// Builds one fresh portable compaction preview without activating a compaction lifecycle.
    ///
    /// Cache-aware V3 still records its bounded semantic-summary provider attempt.
    ///
    /// The driver retains exact process-local target material only when the returned admission is
    /// ready. A later apply must echo that opaque preview binding through the durable command
    /// route.
    pub fn conversation_compaction_review(
        &self,
        session_id: &str,
    ) -> Result<HttpCompactionReview, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.driver.conversation_compaction_review(&session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "conversation compaction preview",
            run_id: session_id.to_owned(),
        })?
        .map_err(recovery_driver_registry_error);
        guard.finish(false);
        result
    }

    /// Applies one exactly-bound compaction, restore, or conversation fork under durable mutation
    /// exclusion.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol or identity conflicts, stale recovery bindings, active
    /// session ownership, fresh restore conflicts, or unavailable lifecycle state.
    pub fn command_conversation_recovery(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpConversationRecoveryCommandAction>,
    ) -> Result<HttpConversationRecoveryCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }
        validate_conversation_recovery_command(&command.payload)?;

        let request = HttpReservedCommand::recovery(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_recovery(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let action = command.payload.kind();
        let result = (|| {
            let session = self.get_session(session_id)?;
            let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
            let driver_command = HttpConversationRecoveryDriverCommand {
                command_id: command.command_id.clone(),
                client_id: command.client_id.clone(),
                action: command.payload.clone(),
            };
            let output = catch_unwind(AssertUnwindSafe(|| {
                self.driver
                    .mutate_conversation_recovery(&session, &driver_command)
            }))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "conversation recovery mutation",
                run_id: session_id.to_owned(),
            })?
            .map_err(recovery_driver_registry_error);
            guard.finish(false);
            let output = output?;
            Ok(HttpConversationRecoveryCommandReceipt {
                command_id: command.command_id.clone(),
                client_id: command.client_id.clone(),
                session_id: command.session_id.clone(),
                action,
                compaction: output.compaction,
                compaction_review: output.compaction_review,
                tool_output_shrink: output.tool_output_shrink,
                restore: output.restore,
                fork: output.fork,
                recovery: output.recovery,
                correlation_id: command.correlation_id.clone(),
                replayed: false,
            })
        })();
        completion.complete(HttpCommandCompletion::Recovery(Box::new(result.clone())))?;
        result
    }

    /// Projects the bounded durable follow-up queue for one adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown or the application owner cannot prove queue
    /// state and process-local material availability.
    pub fn conversation_queue(
        &self,
        session_id: &str,
    ) -> Result<HttpConversationQueueView, HttpRegistryError> {
        let (session, foreground_owner) = {
            let state = self.lock_state();
            let current = state.sessions.get(session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                }
            })?;
            (current.snapshot(), current.foreground_owner())
        };
        catch_unwind(AssertUnwindSafe(|| {
            self.driver
                .conversation_queue_view(&session, foreground_owner.as_ref())
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "conversation queue view",
            run_id: session_id.to_owned(),
        })?
        .map_err(queue_driver_registry_error)
    }

    /// Applies one idempotent queue command under the exact durable queue generation.
    ///
    /// Mutation receipts are completed before best-effort scheduler admission so a successful
    /// durable write never becomes an un-replayable failed command merely because execution could
    /// not start immediately. Interrupt-and-run-next additionally binds the exact foreground owner,
    /// waits for the production supervisor to release its session lease, and only then admits the
    /// next queued run.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol or identity conflicts, stale queue generations, owner drift,
    /// unavailable exact prompt material, or a rejected queue operation.
    pub fn command_conversation_queue(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpConversationQueueCommandRequest>,
    ) -> Result<HttpConversationQueueCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }
        validate_conversation_queue_command(&command.payload)?;

        let request = HttpReservedCommand::queue(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_queue(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let action = command.payload.action.kind();
        let expected_generation = command.payload.expected_generation.clone();
        let is_interrupt = matches!(
            command.payload.action,
            HttpConversationQueueCommandAction::InterruptAndRunNext { .. }
        );
        let queue_command_lock = self.queue_command_lock(session_id);
        let queue_command_guard = Some(
            queue_command_lock
                .lock()
                .expect("per-session queue command lock should not be poisoned"),
        );
        let result = (|| {
            let (session, foreground_owner, queue_session_guard) = {
                let mut state = self.lock_state();
                state.ensure_accepting_commands()?;
                let durable_session_id = state
                    .sessions
                    .get(session_id)
                    .ok_or_else(|| HttpRegistryError::SessionNotFound {
                        session_id: session_id.to_owned(),
                    })?
                    .binding
                    .session_scope_id
                    .clone();
                if state
                    .durable_session_mutations
                    .contains(&durable_session_id)
                    || !state
                        .active_queue_session_commands
                        .insert(durable_session_id.clone())
                {
                    return Err(HttpRegistryError::DurableSessionMutationActive);
                }
                let current = state.sessions.get(session_id).ok_or_else(|| {
                    HttpRegistryError::SessionNotFound {
                        session_id: session_id.to_owned(),
                    }
                })?;
                (
                    current.snapshot(),
                    current.foreground_owner(),
                    HttpQueueSessionCommandGuard {
                        registry: self,
                        durable_session_id,
                    },
                )
            };
            // Held until the closure ends so the per-session queue mutation lock stays exclusive.
            let _queue_session_guard = queue_session_guard;

            let interrupt_owner = match &command.payload.action {
                HttpConversationQueueCommandAction::InterruptAndRunNext {
                    foreground_run_id,
                    foreground_owner_revision,
                } => {
                    let owner = foreground_owner
                        .as_ref()
                        .filter(|owner| {
                            owner.run_id == *foreground_run_id
                                && owner.owner_revision == *foreground_owner_revision
                        })
                        .cloned()
                        .ok_or(HttpRegistryError::ConversationQueueOwnerLost)?;
                    Some(owner)
                }
                _ => None,
            };

            let driver_command = HttpConversationQueueDriverCommand {
                command_id: command.command_id.clone(),
                client_id: command.client_id.clone(),
                request: command.payload.clone(),
            };
            let queue = catch_unwind(AssertUnwindSafe(|| {
                self.driver.mutate_conversation_queue(
                    &session,
                    foreground_owner.as_ref(),
                    &driver_command,
                )
            }))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "conversation queue mutation",
                run_id: session_id.to_owned(),
            })?
            .map_err(queue_driver_registry_error)?;

            // InterruptAndRunNext is non-destructive: the foreground run is never cancelled.
            // The head queue item is delivered by the kernel's safe-point injection while the
            // run continues, or by the next idle dispatch after it finishes.

            Ok(HttpConversationQueueCommandReceipt {
                command_id: command.command_id.clone(),
                client_id: command.client_id.clone(),
                session_id: command.session_id.clone(),
                action,
                expected_generation: expected_generation.clone(),
                generation: queue.generation.clone(),
                interrupt_owner,
                queue,
                correlation_id: command.correlation_id.clone(),
                replayed: false,
            })
        })();
        drop(queue_command_guard);
        completion.complete(HttpCommandCompletion::Queue(Box::new(result.clone())))?;

        if result.is_ok() && !is_interrupt {
            // Queue state is already durable and replayable. Scheduler admission is intentionally
            // best-effort and will be retried after release, resume, reopen, or a later mutation.
            let _ = self.schedule_next_queued_run(session_id);
        }
        result
    }

    /// Admits and starts the next queue-owned foreground run without using the public run route.
    ///
    /// The application driver returns a secret-free admission derived from current durable truth.
    /// The registry then rechecks single-foreground ownership before registering the logical
    /// dispatch run. Promotion CAS and provider execution remain driver-owned.
    fn schedule_next_queued_run(
        &self,
        session_id: &str,
    ) -> Result<Option<HttpRunSnapshot>, HttpRegistryError> {
        self.schedule_next_queued_run_with_retries(session_id, QUEUE_STALE_START_RESCHEDULE_LIMIT)
    }

    fn schedule_next_queued_run_with_retries(
        &self,
        session_id: &str,
        retries_remaining: usize,
    ) -> Result<Option<HttpRunSnapshot>, HttpRegistryError> {
        let session = {
            let state = self.lock_state();
            state.ensure_accepting_commands()?;
            let current = state.sessions.get(session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                }
            })?;
            if current.foreground_run_id.is_some()
                || current.release_pending_run_id.is_some()
                || current.verification_in_progress
                || state
                    .durable_session_mutations
                    .contains(&current.binding.session_scope_id)
            {
                return Ok(None);
            }
            current.snapshot()
        };

        let awaiting_user_input = catch_unwind(AssertUnwindSafe(|| {
            self.driver.has_unresolved_user_input(&session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "queued user input admission",
            run_id: session_id.to_owned(),
        })?
        .map_err(|_| HttpRegistryError::DurableSessionUnavailable)?;
        if awaiting_user_input {
            return Ok(None);
        }

        let admission = catch_unwind(AssertUnwindSafe(|| {
            self.driver.next_queued_run_admission(&session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "queued run admission",
            run_id: session_id.to_owned(),
        })?
        .map_err(queue_driver_registry_error)?;
        let Some(admission) = admission else {
            return Ok(None);
        };
        let release_barrier = self.driver.requires_run_release_barrier();
        if admission.entry_id.trim().is_empty()
            || admission.dispatch_run_id.trim().is_empty()
            || admission.generation.0.trim().is_empty()
        {
            return Err(HttpRegistryError::ConversationQueueUnavailable);
        }

        let (session_snapshot, run_snapshot) = {
            let mut state = self.lock_state();
            state.ensure_accepting_commands()?;
            if state.runs.contains_key(&admission.dispatch_run_id) {
                return Err(HttpRegistryError::ConversationQueueConflict);
            }
            let durable_session_id = state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?
                .binding
                .session_scope_id
                .clone();
            if state
                .durable_session_mutations
                .contains(&durable_session_id)
            {
                return Ok(None);
            }
            let current = state.sessions.get_mut(session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                }
            })?;
            if current.foreground_run_id.is_some()
                || current.release_pending_run_id.is_some()
                || current.verification_in_progress
            {
                return Ok(None);
            }
            let run = HttpRunState::new(
                admission.dispatch_run_id.clone(),
                session_id.to_owned(),
                admission.permission_mode,
                admission.reasoning_effort,
                prompt_preview(&admission.prompt_preview),
            );
            current.run_ids.push(admission.dispatch_run_id.clone());
            current.foreground_owner_generation =
                current.foreground_owner_generation.saturating_add(1);
            current.foreground_run_id = Some(admission.dispatch_run_id.clone());
            current.release_pending_run_id =
                release_barrier.then(|| admission.dispatch_run_id.clone());
            let session_snapshot = current.snapshot();
            let run_snapshot = run.snapshot();
            state.runs.insert(admission.dispatch_run_id.clone(), run);
            (session_snapshot, run_snapshot)
        };

        let start = HttpQueuedRunDriverStart {
            session: session_snapshot,
            run: run_snapshot,
            admission,
        };
        let run_id = start.run.id.clone();
        let failed_admission = start.admission.clone();
        match catch_unwind(AssertUnwindSafe(|| self.driver.start_queued_run(start))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.lock_state().rollback_queued_run_registration(&run_id);
                let rejection = HttpRegistryError::DriverRejected {
                    operation: "queued start",
                    run_id,
                    message: error.message,
                };
                let latest = self.conversation_queue(session_id)?;
                let Some(latest_entry_id) = latest.next_dispatchable_entry_id.as_deref() else {
                    return Ok(None);
                };
                let admission_drifted = latest.generation != failed_admission.generation
                    || latest_entry_id != failed_admission.entry_id;
                if admission_drifted && retries_remaining > 0 {
                    return self
                        .schedule_next_queued_run_with_retries(session_id, retries_remaining - 1);
                }
                return Err(rejection);
            }
            Err(_) => {
                self.lock_state().mark_run_driver_uncertain(&run_id)?;
                return Err(HttpRegistryError::DriverPanicked {
                    operation: "queued start",
                    run_id,
                });
            }
        }

        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(&run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.clone(),
            })?;
        if run.status == HttpRunStatus::Starting {
            run.status = HttpRunStatus::Running;
        }
        Ok(Some(run.snapshot()))
    }

    /// Starts one run inside an existing HTTP adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, the prompt is empty, permission mode is missing,
    /// or the driver rejects the run.
    pub fn start_run(
        &self,
        session_id: &str,
        request: HttpRunStartRequest,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        match request.task_continuation.as_ref() {
            Some(task) => {
                if task.task_id.trim().is_empty()
                    || task
                        .guidance
                        .as_deref()
                        .is_some_and(|guidance| guidance.trim().is_empty())
                    || !request.prompt.trim().is_empty()
                    || request.model_ref.is_some()
                    || request.model_selection_binding.is_some()
                    || request.route_recovery_binding.is_some()
                    || request.reasoning_effort.is_some()
                    || request.reasoning_effort_binding.is_some()
                    || request.skill_binding.is_some()
                    || request.agent_binding.is_some()
                {
                    return Err(HttpRegistryError::InvalidTaskContinuation);
                }
            }
            None if request.prompt.trim().is_empty() => {
                return Err(HttpRegistryError::EmptyPrompt);
            }
            None => {}
        }
        let admission_session = {
            let state = self.lock_state();
            state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?
                .snapshot()
        };
        let awaiting_user_input = catch_unwind(AssertUnwindSafe(|| {
            self.driver.has_unresolved_user_input(&admission_session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "user input admission",
            run_id: session_id.to_owned(),
        })?
        .map_err(|_| HttpRegistryError::DurableSessionUnavailable)?;
        if awaiting_user_input {
            return Err(HttpRegistryError::SessionAwaitingUserInput {
                session_id: session_id.to_owned(),
            });
        }
        catch_unwind(AssertUnwindSafe(|| {
            self.driver.admit_run_start(&admission_session, &request)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "run admission",
            run_id: session_id.to_owned(),
        })?
        .map_err(http_run_admission_registry_error)?;
        let permission_mode = request
            .permission_mode
            .ok_or(HttpRegistryError::MissingPermissionMode)?;
        let model_ref = request.model_ref;
        let model_selection_binding = request.model_selection_binding;
        let route_recovery_binding = request.route_recovery_binding;
        let reasoning_effort = request.reasoning_effort;
        let reasoning_effort_binding = request.reasoning_effort_binding;
        let skill_binding = request.skill_binding;
        let agent_binding = request.agent_binding;
        let task_continuation = request.task_continuation;
        let prompt = task_continuation.as_ref().map_or(request.prompt, |task| {
            task.guidance.as_deref().map_or_else(
                || format!("Continue task {}", task.task_id.trim()),
                safe_persistence_text,
            )
        });
        let release_barrier = self.driver.requires_run_release_barrier();
        let (run_id, session_snapshot, run_snapshot) = {
            let mut state = self.lock_state();
            state.ensure_accepting_commands()?;
            let run_id = state.next_run_id();
            let durable_session_id = state
                .sessions
                .get(session_id)
                .ok_or_else(|| HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                })?
                .binding
                .session_scope_id
                .clone();
            if state
                .durable_session_mutations
                .contains(&durable_session_id)
            {
                return Err(HttpRegistryError::DurableSessionMutationActive);
            }
            let session = state.sessions.get_mut(session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                }
            })?;
            if let Some(run_id) = session.foreground_run_id.as_ref() {
                return Err(HttpRegistryError::SessionForegroundRunActive {
                    session_id: session_id.to_owned(),
                    run_id: run_id.clone(),
                });
            }
            if let Some(run_id) = session.release_pending_run_id.as_ref() {
                return Err(HttpRegistryError::SessionRunCleanupActive {
                    session_id: session_id.to_owned(),
                    run_id: run_id.clone(),
                });
            }
            if session.verification_in_progress {
                return Err(HttpRegistryError::SessionVerificationActive {
                    session_id: session_id.to_owned(),
                });
            }
            let run = HttpRunState::new(
                run_id.clone(),
                session_id.to_owned(),
                permission_mode,
                reasoning_effort,
                prompt_preview(&prompt),
            );
            session.run_ids.push(run_id.clone());
            session.foreground_owner_generation =
                session.foreground_owner_generation.saturating_add(1);
            session.foreground_run_id = Some(run_id.clone());
            session.release_pending_run_id = release_barrier.then(|| run_id.clone());
            let session_snapshot = session.snapshot();
            let run_snapshot = run.snapshot();
            state.runs.insert(run_id.clone(), run);
            (run_id, session_snapshot, run_snapshot)
        };

        let start = HttpRunDriverStart {
            session: session_snapshot,
            run: run_snapshot,
            prompt,
            model_ref,
            model_selection_binding,
            route_recovery_binding,
            reasoning_effort_binding,
            skill_binding,
            agent_binding,
            task_continuation,
        };
        match catch_unwind(AssertUnwindSafe(|| self.driver.start_run(start))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let mut state = self.lock_state();
                if state
                    .runs
                    .get(&run_id)
                    .is_some_and(|run| !run.status.is_terminal())
                {
                    state.transition_run_terminal(&run_id, HttpRunTerminalOutcome::Failed)?;
                }
                state.clear_run_release_barrier(&run_id);
                return Err(HttpRegistryError::DriverRejected {
                    operation: "start",
                    run_id,
                    message: error.message,
                });
            }
            Err(_) => {
                self.lock_state().mark_run_driver_uncertain(&run_id)?;
                return Err(HttpRegistryError::DriverPanicked {
                    operation: "start",
                    run_id,
                });
            }
        }

        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(&run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.clone(),
            })?;
        if run.status == HttpRunStatus::Starting {
            run.status = HttpRunStatus::Running;
        }
        Ok(run.snapshot())
    }

    /// Records one typed terminal lifecycle and releases the owning session foreground lease.
    ///
    /// Repeating the same terminal outcome is idempotent. A contradictory terminal callback fails
    /// closed and leaves the first terminal unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown or already has another terminal outcome.
    pub fn record_run_terminal(
        &self,
        run_id: &str,
        outcome: HttpRunTerminalOutcome,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        self.lock_state().transition_run_terminal(run_id, outcome)
    }

    /// Reconciles an already-settled retained stream before committing the foreground terminal.
    ///
    /// The callback runs only when every terminal task is terminal. If reconciliation fails, the
    /// run transition is left uncommitted so the same terminal outcome can be retried without a
    /// process-local terminal / durable-open split.
    pub fn record_run_terminal_with_reconciliation<F>(
        &self,
        run_id: &str,
        outcome: HttpRunTerminalOutcome,
        reconcile: F,
    ) -> Result<HttpRunSnapshot, HttpRegistryError>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let mut state = self.lock_state();
        let requested = outcome.status();
        let (current, snapshot, should_reconcile) = {
            let run = state
                .runs
                .get(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.status.is_terminal() && run.status != requested {
                return Err(HttpRegistryError::RunTerminalConflict {
                    run_id: run_id.to_owned(),
                    current: run.status,
                    requested: outcome,
                });
            }
            (
                run.status,
                run.snapshot(),
                run.terminal_tasks
                    .values()
                    .all(|task| task.status.is_terminal()),
            )
        };
        if should_reconcile {
            reconcile().map_err(|message| HttpRegistryError::DriverRejected {
                operation: "reconcile terminal run stream",
                run_id: run_id.to_owned(),
                message,
            })?;
        }
        if current.is_terminal() {
            return Ok(snapshot);
        }
        state.transition_run_terminal(run_id, outcome)
    }

    /// Applies one generation-aware terminal lifecycle update to a run snapshot.
    ///
    /// `Ok(None)` means the generation was already projected. Terminal tasks may outlive the
    /// foreground model run, so a terminal run continues to accept newer owner generations.
    pub fn record_terminal_lifecycle(
        &self,
        run_id: &str,
        event: &sigil_kernel::TerminalLifecycleEvent,
    ) -> Result<Option<u64>, HttpRegistryError> {
        self.record_terminal_lifecycle_with_publication(run_id, event, |_, _| Ok(()))
    }

    /// Durably publishes and then commits one terminal lifecycle projection under the run lock.
    ///
    /// The publication callback receives the exact next stream sequence and whether this event
    /// must atomically close a foreground-terminal stream. Registry generation and sequence state
    /// are committed only after the callback succeeds, so a transient protocol journal failure can
    /// retry the same owner generation without creating a permanent gap.
    pub fn record_terminal_lifecycle_with_publication<F>(
        &self,
        run_id: &str,
        event: &sigil_kernel::TerminalLifecycleEvent,
        publish: F,
    ) -> Result<Option<u64>, HttpRegistryError>
    where
        F: FnOnce(u64, bool) -> Result<(), String>,
    {
        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        let task_id = event.task_id.as_str();
        if run
            .terminal_tasks
            .get(task_id)
            .is_some_and(|current| current.generation >= event.generation)
        {
            return Ok(None);
        }
        let candidate = HttpTerminalLifecycleView::from(event);
        let all_tasks_terminal = candidate.status.is_terminal()
            && run.terminal_tasks.iter().all(|(existing_id, existing)| {
                existing_id == task_id || existing.status.is_terminal()
            });
        let close_stream_after_publication = run.status.is_terminal() && all_tasks_terminal;
        let sequence = run.stream_sequence.saturating_add(1);
        publish(sequence, close_stream_after_publication).map_err(|message| {
            HttpRegistryError::DriverRejected {
                operation: "publish terminal lifecycle",
                run_id: run_id.to_owned(),
                message,
            }
        })?;
        run.terminal_tasks.insert(task_id.to_owned(), candidate);
        run.stream_sequence = sequence;
        Ok(Some(sequence))
    }

    /// Notifies the queue scheduler after a driver-owned supervisor released its session lease.
    ///
    /// A durable terminal callback alone is intentionally insufficient: the next queued run is
    /// admitted only after the production owner has completed process-local cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown, has not reached terminal state, or queue
    /// admission fails. No public run-start command is synthesized.
    pub fn record_run_released(
        &self,
        run_id: &str,
    ) -> Result<Option<HttpRunSnapshot>, HttpRegistryError> {
        let session_id = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if !run.status.is_terminal() && run.status != HttpRunStatus::ExecutionUncertain {
                return Err(HttpRegistryError::RunNotActive {
                    run_id: run_id.to_owned(),
                });
            }
            let session_id = run.session_id.clone();
            let session = state.sessions.get_mut(&session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.clone(),
                }
            })?;
            if let Some(pending_run_id) = session.release_pending_run_id.as_deref()
                && pending_run_id != run_id
            {
                // A concurrent release callback already admitted the next owned run. Repeating
                // the old release notification must not clear the new owner's barrier.
                return Ok(None);
            }
            session.release_pending_run_id = None;
            session_id
        };
        self.schedule_next_queued_run(&session_id)
    }

    /// Quarantines a run whose owned production execution task unwound without a durable terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown.
    pub fn record_run_execution_uncertain(
        &self,
        run_id: &str,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        self.lock_state().mark_run_driver_uncertain(run_id)
    }

    /// Returns a point-in-time process-local activity snapshot.
    #[must_use]
    pub fn activity(&self) -> HttpRegistryActivity {
        let state = self.lock_state();
        HttpRegistryActivity {
            retained_commands: state.command_reservations.len(),
            in_flight_commands: state
                .command_reservations
                .values()
                .filter(|reservation| !reservation.is_complete())
                .count(),
            command_waiters: state
                .command_reservations
                .values()
                .map(|reservation| reservation.waiter_count())
                .sum(),
            cancellation_waiters: state
                .runs
                .values()
                .filter_map(|run| run.cancel_operation.as_ref())
                .map(|operation| operation.waiter_count())
                .sum(),
        }
    }

    /// Stops admission of new command identities while preserving replay for already reserved keys.
    pub fn begin_shutdown(&self) {
        self.lock_state().accepting_commands = false;
    }

    /// Cooperatively cancels every active run through the normal driver control path.
    ///
    /// # Errors
    ///
    /// Returns the first cancellation failure after attempting every active run.
    pub fn cancel_active_runs(&self, reason: &str) -> Result<(), HttpRegistryError> {
        let run_ids = {
            let state = self.lock_state();
            state
                .runs
                .values()
                .filter(|run| {
                    !run.status.is_terminal() && run.status != HttpRunStatus::ExecutionUncertain
                })
                .map(|run| run.id.clone())
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for run_id in run_ids {
            if let Err(error) = self.cancel_run_with_reason(&run_id, Some(reason.to_owned()))
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Waits for all driver-owned run supervisors to release their owners.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver cannot prove idle state before the deadline.
    pub fn wait_for_driver_idle(&self, timeout: Duration) -> Result<(), HttpRegistryError> {
        self.driver
            .wait_for_idle(timeout)
            .map_err(|error| HttpRegistryError::DriverRejected {
                operation: "shutdown drain",
                run_id: "all".to_owned(),
                message: error.message,
            })
    }

    /// Starts one run from a command envelope with retry de-duplication.
    ///
    /// # Errors
    ///
    /// Returns an error when the command version is unsupported, the command session does not
    /// match the path session, the session/run request is invalid, or the driver rejects startup.
    pub fn start_run_command(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpRunStartRequest>,
    ) -> Result<HttpRunStartCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }

        // The shipping route is application-owned. The legacy command store below is compiled
        // only for synthetic unit-test drivers until the remaining HTTP command families migrate.
        #[cfg(not(test))]
        {
            let client = self.application_client(session_id, &command.client_id)?;
            self.start_run_command_via_application(session_id, command, client)
        }
        #[cfg(test)]
        if let Ok(client) = self.application_client(session_id, &command.client_id) {
            return self.start_run_command_via_application(session_id, command, client);
        }

        #[cfg(test)]
        {
            let request = HttpReservedCommand::start(session_id, &command)?;
            let reservation =
                match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                    HttpCommandClaim::Execute(reservation) => reservation,
                    HttpCommandClaim::Wait(reservation) => return reservation.wait_for_start(),
                };
            let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
            let result = self.start_run(session_id, command.payload).map(|run| {
                let foreground_owner = self
                    .lock_state()
                    .sessions
                    .get(session_id)
                    .and_then(HttpSessionState::foreground_owner)
                    .filter(|owner| owner.run_id == run.id);
                HttpRunStartCommandReceipt {
                    command_id: command.command_id,
                    client_id: command.client_id,
                    session_id: command.session_id,
                    correlation_id: command.correlation_id,
                    run,
                    foreground_owner,
                    replayed: false,
                }
            });
            completion.complete(HttpCommandCompletion::Start(result.clone()))?;
            result
        }
    }

    fn start_run_command_via_application(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpRunStartRequest>,
        client: crate::application_bridge::HttpApplicationClient,
    ) -> Result<HttpRunStartCommandReceipt, HttpRegistryError> {
        client.refresh().map_err(application_registry_error)?;
        let prompt = if command.payload.prompt.trim().is_empty() {
            None
        } else {
            Some(
                sigil_application::SafeText::new(command.payload.prompt.clone())
                    .map_err(application_registry_error)?,
            )
        };
        let options = crate::application_bridge::application_run_start_options(&command.payload)
            .map_err(application_registry_error)?;
        let receipt = client
            .execute(
                &command.command_id,
                sigil_application::ApplicationCommand::Conversation(
                    sigil_application::ConversationCommand::SubmitPrompt {
                        prompt,
                        options: Some(Box::new(options)),
                    },
                ),
            )
            .map_err(application_registry_error)?;
        let (run_id, replayed) = match &receipt {
            sigil_application::ApplicationCommandReceipt::Uncertain(receipt) => (
                receipt
                    .recovery_binding
                    .strip_prefix("http-run-start:")
                    .ok_or_else(|| {
                        application_registry_error(
                            sigil_application::ApplicationError::CorruptProjection(
                                "HTTP run-start recovery binding is malformed".to_owned(),
                            ),
                        )
                    })?
                    .to_owned(),
                false,
            ),
            sigil_application::ApplicationCommandReceipt::ReplayedUncertain(receipt) => (
                receipt
                    .recovery_binding
                    .strip_prefix("http-run-start:")
                    .ok_or_else(|| {
                        application_registry_error(
                            sigil_application::ApplicationError::CorruptProjection(
                                "HTTP run-start recovery binding is malformed".to_owned(),
                            ),
                        )
                    })?
                    .to_owned(),
                true,
            ),
            sigil_application::ApplicationCommandReceipt::Rejected(rejection) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application run start",
                    run_id: session_id.to_owned(),
                    message: rejection.reason.clone(),
                });
            }
            sigil_application::ApplicationCommandReceipt::PayloadConflict(_) => {
                return Err(HttpRegistryError::CommandKeyConflict {
                    session_id: command.session_id,
                    client_id: command.client_id,
                    command_id: command.command_id,
                });
            }
            sigil_application::ApplicationCommandReceipt::InFlight(_) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application run start",
                    run_id: session_id.to_owned(),
                    message: "application run start is already in flight".to_owned(),
                });
            }
            sigil_application::ApplicationCommandReceipt::Settled(_)
            | sigil_application::ApplicationCommandReceipt::Replayed(_) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application run start",
                    run_id: session_id.to_owned(),
                    message: "application run start returned an invalid settled receipt".to_owned(),
                });
            }
        };
        let run = self.get_run(&run_id)?;
        Ok(HttpRunStartCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            correlation_id: command.correlation_id,
            foreground_owner: self.session_foreground_owner(session_id)?,
            run,
            replayed,
        })
    }

    /// Returns one HTTP adapter run snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when `run_id` is unknown.
    pub fn get_run(&self, run_id: &str) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let state = self.lock_state();
        state
            .runs
            .get(run_id)
            .map(HttpRunState::snapshot)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })
    }

    /// Projects the current shared verification view for one adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown or durable projection fails.
    pub fn verification_view(
        &self,
        session_id: &str,
    ) -> Result<Option<HttpVerificationView>, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| self.driver.verification_view(&session)))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "verification view",
                run_id: session_id.to_owned(),
            })?
            .map_err(|error| HttpRegistryError::DriverRejected {
                operation: "verification view",
                run_id: session_id.to_owned(),
                message: error.message,
            })
    }

    /// Projects the current adapter-neutral Intent Stack for one session.
    pub fn intent_stack_view(
        &self,
        session_id: &str,
    ) -> Result<HttpIntentStackView, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| self.driver.intent_stack_view(&session)))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "Intent Stack view",
                run_id: session_id.to_owned(),
            })?
            .map_err(intent_stack_driver_registry_error)
    }

    /// Produces one fresh exact Intent Drop preview under durable mutation exclusion.
    pub fn preview_intent_drop(
        &self,
        session_id: &str,
        request: HttpIntentDropPreviewRequest,
    ) -> Result<HttpIntentDropPreview, HttpRegistryError> {
        request
            .intent_ref
            .validate()
            .map_err(|_| HttpRegistryError::IntentStackInvalidRequest)?;
        let session = self.get_session(session_id)?;
        let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.driver
                .preview_intent_drop(&session, &request.intent_ref)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "Intent Drop preview",
            run_id: session_id.to_owned(),
        })?
        .map_err(intent_stack_driver_registry_error);
        guard.finish(false);
        result
    }

    /// Executes one exact Intent Drop from an idempotent command envelope.
    pub fn execute_intent_drop_command(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpIntentDropRequest>,
    ) -> Result<HttpIntentDropCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }
        let request = HttpReservedCommand::intent_drop(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_intent_drop(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result = (|| {
            let session = self.get_session(session_id)?;
            let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
            let execution = catch_unwind(AssertUnwindSafe(|| {
                self.driver.execute_intent_drop(&session, &command.payload)
            }))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "Intent Drop",
                run_id: session_id.to_owned(),
            })?
            .map_err(intent_stack_driver_registry_error);
            guard.finish(false);
            Ok(HttpIntentDropCommandReceipt {
                command_id: command.command_id.clone(),
                client_id: command.client_id.clone(),
                session_id: command.session_id.clone(),
                correlation_id: command.correlation_id.clone(),
                execution: execution?,
                replayed: false,
            })
        })();
        completion.complete(HttpCommandCompletion::IntentDrop(Box::new(result.clone())))?;
        result
    }

    /// Executes one exact verification rerun from an idempotent command envelope.
    ///
    /// The session cannot admit an agent run or another verification rerun until the driver
    /// returns a durable terminal projection.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol/key conflicts, active foreground work, stale verification
    /// bindings, or driver failure.
    pub fn rerun_verification_command(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpVerificationRerunRequest>,
    ) -> Result<HttpVerificationRerunCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }
        let request = HttpReservedCommand::verification(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_verification(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result = (|| {
            let session = {
                let mut state = self.lock_state();
                state.ensure_accepting_commands()?;
                let durable_session_id = state
                    .sessions
                    .get(session_id)
                    .ok_or_else(|| HttpRegistryError::SessionNotFound {
                        session_id: session_id.to_owned(),
                    })?
                    .binding
                    .session_scope_id
                    .clone();
                if state
                    .durable_session_mutations
                    .contains(&durable_session_id)
                {
                    return Err(HttpRegistryError::DurableSessionMutationActive);
                }
                let session = state.sessions.get_mut(session_id).ok_or_else(|| {
                    HttpRegistryError::SessionNotFound {
                        session_id: session_id.to_owned(),
                    }
                })?;
                if let Some(run_id) = session.foreground_run_id.as_ref() {
                    return Err(HttpRegistryError::SessionForegroundRunActive {
                        session_id: session_id.to_owned(),
                        run_id: run_id.clone(),
                    });
                }
                if let Some(run_id) = session.release_pending_run_id.as_ref() {
                    return Err(HttpRegistryError::SessionRunCleanupActive {
                        session_id: session_id.to_owned(),
                        run_id: run_id.clone(),
                    });
                }
                if session.verification_in_progress {
                    return Err(HttpRegistryError::SessionVerificationActive {
                        session_id: session_id.to_owned(),
                    });
                }
                session.verification_in_progress = true;
                session.snapshot()
            };
            let driver_result = catch_unwind(AssertUnwindSafe(|| {
                self.driver.rerun_verification(&session, &command.payload)
            }));
            if let Some(session) = self.lock_state().sessions.get_mut(session_id) {
                session.verification_in_progress = false;
            }
            let verification = match driver_result {
                Ok(Ok(view)) => view,
                Ok(Err(error)) => {
                    return Err(HttpRegistryError::DriverRejected {
                        operation: "verification rerun",
                        run_id: session_id.to_owned(),
                        message: error.message,
                    });
                }
                Err(_) => {
                    return Err(HttpRegistryError::DriverPanicked {
                        operation: "verification rerun",
                        run_id: session_id.to_owned(),
                    });
                }
            };
            Ok(HttpVerificationRerunCommandReceipt {
                command_id: command.command_id,
                client_id: command.client_id,
                session_id: command.session_id,
                correlation_id: command.correlation_id,
                verification,
                replayed: false,
            })
        })();
        completion.complete(HttpCommandCompletion::Verification(Box::new(
            result.clone(),
        )))?;
        result
    }

    /// Projects one exact current Task integration review under durable mutation exclusion.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown or busy sessions, invalid durable artifacts, or unavailable
    /// application integration support.
    pub fn task_integration_review(
        &self,
        session_id: &str,
    ) -> Result<Option<HttpTaskIntegrationReviewView>, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.driver.task_integration_review(&session)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "Task integration review",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| HttpRegistryError::DriverRejected {
            operation: "Task integration review",
            run_id: session_id.to_owned(),
            message: error.message,
        });
        guard.finish(false);
        result
    }

    /// Accepts one exact Task integration review from an idempotent command envelope.
    ///
    /// The durable-session mutation guard excludes runs, queue mutations, recovery, verification,
    /// and duplicate integration acceptance while promotion and parent checks are in flight.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol/key conflicts, active foreground work, stale review
    /// identity, promotion or parent-check failure, or unavailable application integration.
    pub fn accept_task_integration_command(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpTaskIntegrationReviewRequest>,
    ) -> Result<HttpTaskIntegrationAcceptanceCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }
        validate_task_integration_review_request(&command.payload)?;
        let request = HttpReservedCommand::integration(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_integration(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result = (|| {
            let session = self.get_session(session_id)?;
            let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
            let acceptance = catch_unwind(AssertUnwindSafe(|| {
                self.driver
                    .accept_task_integration(&session, &command.payload)
            }))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "Task integration acceptance",
                run_id: session_id.to_owned(),
            })?
            .map_err(|error| HttpRegistryError::DriverRejected {
                operation: "Task integration acceptance",
                run_id: session_id.to_owned(),
                message: error.message,
            });
            guard.finish(false);
            Ok(HttpTaskIntegrationAcceptanceCommandReceipt {
                command_id: command.command_id.clone(),
                client_id: command.client_id.clone(),
                session_id: command.session_id.clone(),
                correlation_id: command.correlation_id.clone(),
                acceptance: acceptance?,
                replayed: false,
            })
        })();
        completion.complete(HttpCommandCompletion::Integration(Box::new(result.clone())))?;
        result
    }

    /// Requests cancellation for a running HTTP adapter run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown, terminal, or the driver rejects cancellation.
    pub fn cancel_run(&self, run_id: &str) -> Result<HttpRunSnapshot, HttpRegistryError> {
        self.cancel_run_with_reason(run_id, None)
    }

    pub(crate) fn cancel_run_with_reason(
        &self,
        run_id: &str,
        reason: Option<String>,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let claim = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.status.is_terminal() || run.status == HttpRunStatus::ExecutionUncertain {
                return Err(HttpRegistryError::RunNotActive {
                    run_id: run_id.to_owned(),
                });
            }
            if run.pause_operation.is_some() {
                return Err(HttpRegistryError::RunNotActive {
                    run_id: run_id.to_owned(),
                });
            }
            if let Some(operation) = run.cancel_operation.as_ref() {
                HttpCancelClaim::Wait(Arc::clone(operation))
            } else {
                let operation = Arc::new(HttpCancelOperation::new());
                run.previous_status = Some(run.status);
                run.status = HttpRunStatus::CancelRequested;
                run.cancel_operation = Some(Arc::clone(&operation));
                run.advance_stream_sequence();
                HttpCancelClaim::Execute {
                    operation,
                    cancel: HttpRunDriverCancel {
                        session_id: run.session_id.clone(),
                        run_id: run.id.clone(),
                        reason,
                    },
                }
            }
        };
        let (operation, cancel) = match claim {
            HttpCancelClaim::Execute { operation, cancel } => (operation, cancel),
            HttpCancelClaim::Wait(operation) => {
                operation.wait()?;
                return self.get_run(run_id);
            }
        };

        let driver_result = catch_unwind(AssertUnwindSafe(|| self.driver.cancel_run(cancel)));
        match driver_result {
            Ok(Ok(())) => {
                operation.complete(Ok(()));
                self.get_run(run_id)
            }
            Ok(Err(error)) => {
                let registry_error = HttpRegistryError::DriverRejected {
                    operation: "cancel",
                    run_id: run_id.to_owned(),
                    message: error.message,
                };
                let mut state = self.lock_state();
                if let Some(run) = state.runs.get_mut(run_id)
                    && run
                        .cancel_operation
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &operation))
                {
                    run.cancel_operation = None;
                    run.restore_previous_status_if_cancel_requested();
                }
                drop(state);
                operation.complete(Err(registry_error.clone()));
                Err(registry_error)
            }
            Err(_) => {
                let registry_error = HttpRegistryError::DriverPanicked {
                    operation: "cancel",
                    run_id: run_id.to_owned(),
                };
                self.lock_state().mark_run_driver_uncertain(run_id)?;
                operation.complete(Err(registry_error.clone()));
                Err(registry_error)
            }
        }
    }

    /// Requests cancellation from a command envelope with retry de-duplication.
    ///
    /// # Errors
    ///
    /// Returns an error when the command version is unsupported, the command points to a different
    /// session, the optimistic state guard is stale, or the normal cancellation path rejects it.
    pub fn cancel_run_command(
        &self,
        run_id: &str,
        command: HttpCommandEnvelope<HttpRunCancelRequest>,
    ) -> Result<HttpRunCancelCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;

        // Production drivers make the transport-neutral application journal authoritative for
        // this route. Synthetic test drivers do not necessarily implement that port, so their
        // legacy envelope store remains a cfg(test)-only compatibility path while the production
        // binary has no fallback to a second writable reservation authority.
        #[cfg(not(test))]
        {
            let client = self.application_client(&command.session_id, &command.client_id)?;
            self.cancel_run_command_via_application(run_id, command, client)
        }
        #[cfg(test)]
        if let Ok(client) = self.application_client(&command.session_id, &command.client_id) {
            return self.cancel_run_command_via_application(run_id, command, client);
        }

        #[cfg(test)]
        {
            let request = HttpReservedCommand::cancel(run_id, &command)?;
            let reservation =
                match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                    HttpCommandClaim::Execute(reservation) => reservation,
                    HttpCommandClaim::Wait(reservation) => return reservation.wait_for_cancel(),
                };
            let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
            let result = (|| {
                {
                    let state = self.lock_state();
                    let run =
                        state
                            .runs
                            .get(run_id)
                            .ok_or_else(|| HttpRegistryError::RunNotFound {
                                run_id: run_id.to_owned(),
                            })?;
                    if run.session_id != command.session_id {
                        return Err(HttpRegistryError::CommandSessionMismatch {
                            command_session_id: command.session_id,
                            run_id: run_id.to_owned(),
                            run_session_id: run.session_id.clone(),
                        });
                    }
                    if let Some(expected) = command.expected_stream_sequence
                        && expected != run.stream_sequence
                    {
                        return Err(HttpRegistryError::StaleCommandSequence {
                            run_id: run_id.to_owned(),
                            expected,
                            actual: run.stream_sequence,
                        });
                    }
                }
                let run = self.cancel_run_with_reason(run_id, command.payload.reason.clone())?;
                Ok(HttpRunCancelCommandReceipt {
                    command_id: command.command_id,
                    client_id: command.client_id,
                    session_id: command.session_id,
                    expected_stream_sequence: command.expected_stream_sequence,
                    correlation_id: command.correlation_id,
                    run,
                    replayed: false,
                })
            })();
            completion.complete(HttpCommandCompletion::Cancel(result.clone()))?;
            result
        }
    }

    fn cancel_run_command_via_application(
        &self,
        run_id: &str,
        command: HttpCommandEnvelope<HttpRunCancelRequest>,
        client: crate::application_bridge::HttpApplicationClient,
    ) -> Result<HttpRunCancelCommandReceipt, HttpRegistryError> {
        {
            let state = self.lock_state();
            let run = state
                .runs
                .get(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.session_id != command.session_id {
                return Err(HttpRegistryError::CommandSessionMismatch {
                    command_session_id: command.session_id,
                    run_id: run_id.to_owned(),
                    run_session_id: run.session_id.clone(),
                });
            }
            if let Some(expected) = command.expected_stream_sequence
                && expected != run.stream_sequence
            {
                return Err(HttpRegistryError::StaleCommandSequence {
                    run_id: run_id.to_owned(),
                    expected,
                    actual: run.stream_sequence,
                });
            }
        }

        client.refresh().map_err(application_registry_error)?;
        let reason = command
            .payload
            .reason
            .as_ref()
            .map(|reason| {
                sigil_application::SafeText::new(reason.clone()).map_err(application_registry_error)
            })
            .transpose()?;
        let receipt = client
            .execute(
                &command.command_id,
                sigil_application::ApplicationCommand::Run(sigil_application::RunCommand::Cancel {
                    binding: run_id.to_owned(),
                    reason,
                }),
            )
            .map_err(application_registry_error)?;
        let replayed = matches!(
            &receipt,
            sigil_application::ApplicationCommandReceipt::Replayed(_)
                | sigil_application::ApplicationCommandReceipt::ReplayedUncertain(_)
        );
        match receipt {
            sigil_application::ApplicationCommandReceipt::Settled(_)
            | sigil_application::ApplicationCommandReceipt::Replayed(_)
            | sigil_application::ApplicationCommandReceipt::Uncertain(_)
            | sigil_application::ApplicationCommandReceipt::ReplayedUncertain(_) => {
                Ok(HttpRunCancelCommandReceipt {
                    command_id: command.command_id,
                    client_id: command.client_id,
                    session_id: command.session_id,
                    expected_stream_sequence: command.expected_stream_sequence,
                    correlation_id: command.correlation_id,
                    run: self.get_run(run_id)?,
                    replayed,
                })
            }
            sigil_application::ApplicationCommandReceipt::Rejected(rejection) => {
                Err(HttpRegistryError::DriverRejected {
                    operation: "application cancel",
                    run_id: run_id.to_owned(),
                    message: rejection.reason,
                })
            }
            sigil_application::ApplicationCommandReceipt::PayloadConflict(_) => {
                Err(HttpRegistryError::CommandKeyConflict {
                    session_id: command.session_id,
                    client_id: command.client_id,
                    command_id: command.command_id,
                })
            }
            sigil_application::ApplicationCommandReceipt::InFlight(_) => {
                Err(HttpRegistryError::DriverRejected {
                    operation: "application cancel",
                    run_id: run_id.to_owned(),
                    message: "application cancellation is already in flight".to_owned(),
                })
            }
        }
    }

    /// Requests an exact Task pause from a command envelope with retry de-duplication.
    ///
    /// # Errors
    ///
    /// Returns an error when the request identity is malformed, the command is stale, the run is
    /// not active, or the driver cannot prove the exact task/plan/scope binding.
    pub fn pause_task_command(
        &self,
        run_id: &str,
        command: HttpCommandEnvelope<HttpTaskPauseRequest>,
    ) -> Result<HttpTaskPauseCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if !command.payload.has_exact_identity() {
            return Err(HttpRegistryError::InvalidTaskPauseRequest);
        }
        let request = HttpReservedCommand::pause(run_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_pause(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result = (|| {
            {
                let state = self.lock_state();
                let run = state
                    .runs
                    .get(run_id)
                    .ok_or_else(|| HttpRegistryError::RunNotFound {
                        run_id: run_id.to_owned(),
                    })?;
                if run.session_id != command.session_id {
                    return Err(HttpRegistryError::CommandSessionMismatch {
                        command_session_id: command.session_id.clone(),
                        run_id: run_id.to_owned(),
                        run_session_id: run.session_id.clone(),
                    });
                }
                if let Some(expected) = command.expected_stream_sequence
                    && expected != run.stream_sequence
                {
                    return Err(HttpRegistryError::StaleCommandSequence {
                        run_id: run_id.to_owned(),
                        expected,
                        actual: run.stream_sequence,
                    });
                }
            }
            let run = self.pause_task(run_id, command.payload.clone())?;
            Ok(HttpTaskPauseCommandReceipt {
                command_id: command.command_id,
                client_id: command.client_id,
                session_id: command.session_id,
                expected_stream_sequence: command.expected_stream_sequence,
                correlation_id: command.correlation_id,
                task_id: command.payload.task_id.as_str().to_owned(),
                execution: command.payload.execution.clone(),
                run,
                replayed: false,
            })
        })();
        completion.complete(HttpCommandCompletion::Pause(result.clone()))?;
        result
    }

    /// Cancels one exact persistent terminal task without reopening its foreground model run.
    ///
    /// The command is generation- and session-bound. Retrying after a confirmed terminal owner
    /// state returns that same bounded state and does not execute another side effect.
    /// Applies one exact typed plan decision idempotently.
    ///
    /// # Errors
    ///
    /// Returns an error for protocol/key conflicts, a stale plan, conflicting durable facts, or an
    /// unavailable session.
    pub fn plan_decision_command(
        &self,
        session_id: &str,
        command: HttpCommandEnvelope<HttpPlanDecisionRequest>,
    ) -> Result<HttpPlanDecisionCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }
        let request = HttpReservedCommand::plan_decision(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_plan_decision(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result = (|| -> Result<HttpPlanDecisionCommandReceipt, HttpRegistryError> {
            let session = self.get_session(session_id)?;
            let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
            let receipt = catch_unwind(AssertUnwindSafe(|| {
                self.driver.plan_decision(&session, &command.payload)
            }))
            .map_err(|_| HttpRegistryError::DriverPanicked {
                operation: "Plan decision",
                run_id: session_id.to_owned(),
            })?
            .map_err(|error| HttpRegistryError::DriverRejected {
                operation: "Plan decision",
                run_id: session_id.to_owned(),
                message: error.message,
            })?;
            guard.finish(false);
            let mut receipt = receipt;
            receipt.command_id = command.command_id.clone();
            receipt.client_id = command.client_id.clone();
            receipt.session_id = command.session_id.clone();
            receipt.replayed = false;
            Ok(receipt)
        })();
        match result {
            Ok(receipt) => {
                completion.complete(HttpCommandCompletion::PlanDecision(Box::new(Ok(
                    receipt.clone()
                ))))?;
                Ok(receipt)
            }
            Err(error) => {
                completion.complete(HttpCommandCompletion::PlanDecision(Box::new(Err(
                    error.clone()
                ))))?;
                Err(error)
            }
        }
    }

    /// Reads one complete immutable plan detail bound to the selected artifact hash.
    pub fn plan_review_detail(
        &self,
        session_id: &str,
        plan_id: &str,
        expected_plan_hash: &str,
    ) -> Result<HttpPlanReviewDetail, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| {
            self.driver
                .plan_review_detail(&session, plan_id, expected_plan_hash)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "Plan detail",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| HttpRegistryError::DriverRejected {
            operation: "Plan detail",
            run_id: session_id.to_owned(),
            message: error.message,
        })
    }

    /// Reads one exact immutable durable user-input request.
    pub fn user_input_request(
        &self,
        session_id: &str,
        request_id: &str,
        generation: u32,
        expected_request_hash: &str,
    ) -> Result<HttpUserInputRequest, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        catch_unwind(AssertUnwindSafe(|| {
            self.driver
                .user_input_request(&session, request_id, generation, expected_request_hash)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "User input detail",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| HttpRegistryError::DriverRejected {
            operation: "User input detail",
            run_id: session_id.to_owned(),
            message: error.message,
        })
    }

    /// Applies one exact durable user-input decision idempotently.
    pub fn user_input_decision_command(
        &self,
        session_id: &str,
        request_id: &str,
        command: HttpCommandEnvelope<HttpUserInputDecisionRequest>,
    ) -> Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        if command.session_id != session_id {
            return Err(HttpRegistryError::CommandPathSessionMismatch {
                command_session_id: command.session_id.clone(),
                path_session_id: session_id.to_owned(),
            });
        }

        #[cfg(not(test))]
        {
            let client = self.application_client(session_id, &command.client_id)?;
            self.user_input_decision_command_via_application(
                session_id, request_id, command, client,
            )
        }
        #[cfg(test)]
        if let Ok(client) = self.application_client(session_id, &command.client_id) {
            return self.user_input_decision_command_via_application(
                session_id, request_id, command, client,
            );
        }

        #[cfg(test)]
        {
            let command_key = HttpCommandKey::from_envelope(&command);
            let request =
                HttpReservedCommand::user_input_decision(session_id, request_id, &command)?;
            let reservation = match self.reserve_command(command_key.clone(), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => {
                    return reservation.wait_for_user_input_decision();
                }
            };
            let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
            let result = (|| -> Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError> {
                self.user_input_decision_from_application(
                    session_id,
                    request_id,
                    &command.command_id,
                    &command.client_id,
                    command.payload.clone(),
                )
            })();
            let completion_result = completion.complete(HttpCommandCompletion::UserInputDecision(
                Box::new(result.clone()),
            ));
            if result.is_err() || completion_result.is_err() {
                self.release_retryable_user_input_reservation(&command_key, &reservation);
            }
            completion_result?;
            result
        }
    }

    fn user_input_decision_command_via_application(
        &self,
        session_id: &str,
        request_id: &str,
        command: HttpCommandEnvelope<HttpUserInputDecisionRequest>,
        client: crate::application_bridge::HttpApplicationClient,
    ) -> Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError> {
        client.refresh().map_err(application_registry_error)?;
        let request = command.payload.clone();
        let expected_request_hash =
            sigil_application::SafeText::new(request.expected_request_hash.clone())
                .map_err(application_registry_error)?;
        let permission_mode = request.permission_mode.map(|mode| match mode {
            HttpPermissionMode::ReadOnly => sigil_application::ApplicationPermissionMode::ReadOnly,
            HttpPermissionMode::Manual => sigil_application::ApplicationPermissionMode::Manual,
            HttpPermissionMode::AutoEdit => sigil_application::ApplicationPermissionMode::AutoEdit,
            HttpPermissionMode::DangerFullAccess => {
                sigil_application::ApplicationPermissionMode::DangerFullAccess
            }
        });
        let receipt = client
            .execute(
                &command.command_id,
                sigil_application::ApplicationCommand::UserInput(
                    sigil_application::UserInputCommand::Resolve {
                        binding: request_id.to_owned(),
                        generation: request.generation,
                        expected_request_hash,
                        decision: request.decision.clone(),
                        permission_mode,
                    },
                ),
            )
            .map_err(application_registry_error)?;
        let (continuation_run_id, replayed) = match &receipt {
            sigil_application::ApplicationCommandReceipt::Uncertain(receipt) => (
                parse_application_user_input_recovery(&receipt.recovery_binding, request_id)?,
                false,
            ),
            sigil_application::ApplicationCommandReceipt::ReplayedUncertain(receipt) => (
                parse_application_user_input_recovery(&receipt.recovery_binding, request_id)?,
                true,
            ),
            sigil_application::ApplicationCommandReceipt::Rejected(rejection) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application user input decision",
                    run_id: session_id.to_owned(),
                    message: rejection.reason.clone(),
                });
            }
            sigil_application::ApplicationCommandReceipt::PayloadConflict(_) => {
                return Err(HttpRegistryError::CommandKeyConflict {
                    session_id: command.session_id,
                    client_id: command.client_id,
                    command_id: command.command_id,
                });
            }
            sigil_application::ApplicationCommandReceipt::InFlight(_) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application user input decision",
                    run_id: session_id.to_owned(),
                    message: "application user input decision is already in flight".to_owned(),
                });
            }
            sigil_application::ApplicationCommandReceipt::Settled(_)
            | sigil_application::ApplicationCommandReceipt::Replayed(_) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application user input decision",
                    run_id: session_id.to_owned(),
                    message: "application user input returned an invalid settled receipt"
                        .to_owned(),
                });
            }
        };
        let request_view = self.user_input_request(
            session_id,
            request_id,
            request.generation,
            &request.expected_request_hash,
        )?;
        Ok(HttpUserInputDecisionCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            request: request_view,
            continuation_run_id,
            replayed,
        })
    }

    pub(crate) fn user_input_decision_from_application(
        &self,
        session_id: &str,
        request_id: &str,
        command_id: &str,
        client_id: &str,
        request: HttpUserInputDecisionRequest,
    ) -> Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError> {
        let session = self.get_session(session_id)?;
        let guard = self.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
        let driver_command = HttpUserInputDecisionDriverCommand {
            command_id: command_id.to_owned(),
            client_id: client_id.to_owned(),
            request_id: request_id.to_owned(),
            request,
        };
        let receipt = catch_unwind(AssertUnwindSafe(|| {
            self.driver.user_input_decision(&session, &driver_command)
        }))
        .map_err(|_| HttpRegistryError::DriverPanicked {
            operation: "User input decision",
            run_id: session_id.to_owned(),
        })?
        .map_err(|error| match error.kind {
            crate::HttpRunDriverErrorKind::StaleUserInput => HttpRegistryError::UserInputStale,
            crate::HttpRunDriverErrorKind::General => HttpRegistryError::DriverRejected {
                operation: "User input decision",
                run_id: session_id.to_owned(),
                message: error.message,
            },
        })?;
        guard.finish(false);
        let mut receipt = receipt;
        receipt.command_id = command_id.to_owned();
        receipt.client_id = client_id.to_owned();
        receipt.session_id = session_id.to_owned();
        receipt.replayed = false;
        Ok(receipt)
    }

    pub fn cancel_terminal_task_command(
        &self,
        run_id: &str,
        command: HttpCommandEnvelope<HttpTerminalTaskCancelRequest>,
    ) -> Result<HttpTerminalTaskCancelCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;
        command
            .payload
            .validate()
            .map_err(|_| HttpRegistryError::InvalidTerminalTaskCancelRequest)?;
        let request = HttpReservedCommand::terminal_cancel(run_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => {
                    return reservation.wait_for_terminal_cancel();
                }
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result = (|| {
            let current = {
                let state = self.lock_state();
                let run = state
                    .runs
                    .get(run_id)
                    .ok_or_else(|| HttpRegistryError::RunNotFound {
                        run_id: run_id.to_owned(),
                    })?;
                if run.session_id != command.session_id {
                    return Err(HttpRegistryError::CommandSessionMismatch {
                        command_session_id: command.session_id.clone(),
                        run_id: run_id.to_owned(),
                        run_session_id: run.session_id.clone(),
                    });
                }
                if let Some(expected) = command.expected_stream_sequence
                    && expected != run.stream_sequence
                {
                    return Err(HttpRegistryError::StaleCommandSequence {
                        run_id: run_id.to_owned(),
                        expected,
                        actual: run.stream_sequence,
                    });
                }
                let task = run
                    .terminal_tasks
                    .get(&command.payload.task_id)
                    .cloned()
                    .ok_or_else(|| HttpRegistryError::TerminalTaskNotFound {
                        run_id: run_id.to_owned(),
                        task_id: command.payload.task_id.clone(),
                    })?;
                if task.generation != command.payload.expected_generation {
                    return Err(HttpRegistryError::TerminalTaskGenerationChanged {
                        run_id: run_id.to_owned(),
                        task_id: command.payload.task_id.clone(),
                    });
                }
                task
            };
            let replayed = current.status.is_terminal();
            let terminal_task = if replayed {
                current
            } else {
                catch_unwind(AssertUnwindSafe(|| {
                    self.driver
                        .cancel_terminal_task(HttpRunDriverTerminalTaskCancel {
                            session_id: command.session_id.clone(),
                            run_id: run_id.to_owned(),
                            task_id: command.payload.task_id.clone(),
                            expected_generation: command.payload.expected_generation,
                        })
                }))
                .map_err(|_| HttpRegistryError::DriverPanicked {
                    operation: "cancel terminal task",
                    run_id: run_id.to_owned(),
                })?
                .map_err(|error| HttpRegistryError::DriverRejected {
                    operation: "cancel terminal task",
                    run_id: run_id.to_owned(),
                    message: error.message,
                })?
            };
            if terminal_task.task_id != command.payload.task_id
                || terminal_task.generation < command.payload.expected_generation
                || !terminal_task.status.is_terminal()
            {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "cancel terminal task",
                    run_id: run_id.to_owned(),
                    message: "terminal owner returned an invalid cancellation receipt".to_owned(),
                });
            }
            Ok(HttpTerminalTaskCancelCommandReceipt {
                command_id: command.command_id,
                client_id: command.client_id,
                session_id: command.session_id,
                expected_stream_sequence: command.expected_stream_sequence,
                correlation_id: command.correlation_id,
                run_id: run_id.to_owned(),
                terminal_task,
                replayed,
            })
        })();
        completion.complete(HttpCommandCompletion::TerminalCancel(result.clone()))?;
        result
    }

    fn pause_task(
        &self,
        run_id: &str,
        request: HttpTaskPauseRequest,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let claim = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if !matches!(
                run.status,
                HttpRunStatus::Running | HttpRunStatus::WaitingForApproval
            ) || run.cancel_operation.is_some()
            {
                return Err(HttpRegistryError::RunNotActive {
                    run_id: run_id.to_owned(),
                });
            }
            if let Some(operation) = run.pause_operation.as_ref() {
                HttpPauseClaim::Wait(Arc::clone(operation))
            } else {
                let operation = Arc::new(HttpCancelOperation::new());
                run.previous_status = Some(run.status);
                run.status = HttpRunStatus::PauseRequested;
                run.pause_operation = Some(Arc::clone(&operation));
                run.advance_stream_sequence();
                HttpPauseClaim::Execute {
                    operation,
                    pause: HttpRunDriverTaskPause {
                        session_id: run.session_id.clone(),
                        run_id: run.id.clone(),
                        request,
                    },
                }
            }
        };
        let (operation, pause) = match claim {
            HttpPauseClaim::Execute { operation, pause } => (operation, pause),
            HttpPauseClaim::Wait(operation) => {
                operation.wait()?;
                return self.get_run(run_id);
            }
        };

        let driver_result = catch_unwind(AssertUnwindSafe(|| self.driver.pause_task(pause)));
        match driver_result {
            Ok(Ok(())) => {
                operation.complete(Ok(()));
                self.get_run(run_id)
            }
            Ok(Err(error)) => {
                let registry_error = HttpRegistryError::DriverRejected {
                    operation: "Task pause",
                    run_id: run_id.to_owned(),
                    message: error.message,
                };
                let mut state = self.lock_state();
                if let Some(run) = state.runs.get_mut(run_id)
                    && run
                        .pause_operation
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &operation))
                {
                    run.pause_operation = None;
                    run.restore_previous_status_if_pause_requested();
                }
                drop(state);
                operation.complete(Err(registry_error.clone()));
                Err(registry_error)
            }
            Err(_) => {
                let registry_error = HttpRegistryError::DriverPanicked {
                    operation: "Task pause",
                    run_id: run_id.to_owned(),
                };
                self.lock_state().mark_run_driver_uncertain(run_id)?;
                operation.complete(Err(registry_error.clone()));
                Err(registry_error)
            }
        }
    }

    /// Registers one pending approval for an active run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown or cannot accept approval work.
    pub fn register_approval_request(
        &self,
        run_id: &str,
        approval: HttpPendingApproval,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        if let Some(error) = run.approval_route_error(run_id, true) {
            return Err(error);
        }
        if !run.pending_approvals.contains_key(&approval.call_id)
            && run.pending_approvals.len() >= MAX_PENDING_APPROVALS_PER_RUN
        {
            return Err(HttpRegistryError::ApprovalCapacityExceeded {
                run_id: run_id.to_owned(),
            });
        }
        let call_id = approval.call_id.clone();
        run.pending_approvals.insert(call_id.clone(), approval);
        run.approval_lifecycles.remove(&call_id);
        run.status = HttpRunStatus::WaitingForApproval;
        run.advance_stream_sequence();
        Ok(run.snapshot())
    }

    /// Rebinds a pre-registered approval projection to the public sequence allocated by the
    /// durable event sequencer. Approval routing is registered before publication so the event
    /// bus never needs to acquire the registry lock while holding its publication lock.
    pub(crate) fn update_approval_event_sequence(
        &self,
        run_id: &str,
        call_id: &str,
        approval_request_id: &str,
        event_sequence: u64,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(pending) = state
            .runs
            .get_mut(run_id)
            .and_then(|run| run.pending_approvals.get_mut(call_id))
            .filter(|pending| pending.approval_request_id == approval_request_id)
        else {
            // A racing user decision or terminal transition may already have consumed the
            // projection after observing the live event. Routing was still registered before
            // publication, so there is no missing approval window to repair.
            return false;
        };
        pending.display.event_sequence = event_sequence;
        true
    }

    /// Removes an approval request whose adapter-owned wait expired before a decision arrived.
    ///
    /// The method is idempotent for a request already removed by a racing decision or terminal
    /// transition. It never removes an approval that has already entered driver delivery.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown.
    pub fn expire_approval_request(
        &self,
        run_id: &str,
        call_id: &str,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        self.expire_approval_request_matching(run_id, call_id, None)
    }

    /// Expires only the exact kernel-owned approval request, leaving a newer request for the same
    /// call untouched.
    pub fn expire_approval_request_exact(
        &self,
        run_id: &str,
        call_id: &str,
        approval_request_id: &str,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        self.expire_approval_request_matching(run_id, call_id, Some(approval_request_id))
    }

    /// Records the durable resolution of one exact approval request for snapshot recovery.
    pub(crate) fn record_approval_resolution(
        &self,
        run_id: &str,
        call_id: &str,
        approval_request_id: &str,
        approved: bool,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        let exact_request = run
            .approval_lifecycles
            .get(call_id)
            .is_some_and(|lifecycle| lifecycle.approval.approval_request_id == approval_request_id);
        if exact_request
            && run.update_approval_lifecycle_state(
                call_id,
                if approved {
                    HttpApprovalLifecycleState::Resolved
                } else {
                    HttpApprovalLifecycleState::Terminal
                },
            )
        {
            run.advance_stream_sequence();
        }
        Ok(run.snapshot())
    }

    /// Records one exact tool execution transition for approval lifecycle recovery.
    pub(crate) fn record_tool_execution_lifecycle(
        &self,
        run_id: &str,
        execution: &sigil_kernel::ToolExecutionEntry,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        let next = match execution.status {
            sigil_kernel::ToolExecutionStatus::Started => {
                HttpApprovalLifecycleState::ExecutionStarted
            }
            sigil_kernel::ToolExecutionStatus::Completed
            | sigil_kernel::ToolExecutionStatus::Failed
            | sigil_kernel::ToolExecutionStatus::Cancelled
            | sigil_kernel::ToolExecutionStatus::Interrupted => {
                HttpApprovalLifecycleState::Terminal
            }
        };
        if run.update_approval_lifecycle_state(&execution.call_id, next) {
            run.advance_stream_sequence();
        }
        Ok(run.snapshot())
    }

    fn expire_approval_request_matching(
        &self,
        run_id: &str,
        call_id: &str,
        approval_request_id: Option<&str>,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        if run.status.is_terminal() {
            return Ok(run.snapshot());
        }
        let matches = run.pending_approvals.get(call_id).is_some_and(|pending| {
            approval_request_id
                .map(|request_id| pending.approval_request_id == request_id)
                .unwrap_or(true)
        });
        if matches && run.pending_approvals.remove(call_id).is_some() {
            if run.pending_approvals.is_empty()
                && run.in_flight_approvals.is_empty()
                && run.status == HttpRunStatus::WaitingForApproval
            {
                run.status = HttpRunStatus::Running;
            }
            run.advance_stream_sequence();
        }
        Ok(run.snapshot())
    }

    /// Routes one envelope-protected user approval command to an active run.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is stale, duplicated with an unsupported version, points
    /// to the wrong session, or fails normal approval routing checks.
    pub fn submit_approval_command(
        &self,
        run_id: &str,
        call_id: &str,
        command: HttpCommandEnvelope<HttpApprovalDecisionRequest>,
    ) -> Result<HttpApprovalCommandReceipt, HttpRegistryError> {
        command.ensure_supported().map_err(|error| {
            HttpRegistryError::UnsupportedProtocolVersion {
                message: error.to_string(),
            }
        })?;

        #[cfg(not(test))]
        {
            let run = self.get_run(run_id)?;
            if run.session_id != command.session_id {
                return Err(HttpRegistryError::CommandSessionMismatch {
                    command_session_id: command.session_id.clone(),
                    run_id: run_id.to_owned(),
                    run_session_id: run.session_id,
                });
            }
            let client = self.application_client(&run.session_id, &command.client_id)?;
            self.submit_approval_command_via_application(run_id, call_id, command, client)
        }
        #[cfg(test)]
        if let Ok(run) = self.get_run(run_id)
            && let Ok(client) = self.application_client(&run.session_id, &command.client_id)
        {
            return self.submit_approval_command_via_application(run_id, call_id, command, client);
        }

        #[cfg(test)]
        {
            let request = HttpReservedCommand::approval(run_id, call_id, &command)?;
            let reservation =
                match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                    HttpCommandClaim::Execute(reservation) => reservation,
                    HttpCommandClaim::Wait(reservation) => return reservation.wait_for_approval(),
                };
            let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
            let result =
                (|| {
                    {
                        let state = self.lock_state();
                        let run = state.runs.get(run_id).ok_or_else(|| {
                            HttpRegistryError::RunNotFound {
                                run_id: run_id.to_owned(),
                            }
                        })?;
                        if run.session_id != command.session_id {
                            return Err(HttpRegistryError::CommandSessionMismatch {
                                command_session_id: command.session_id,
                                run_id: run_id.to_owned(),
                                run_session_id: run.session_id.clone(),
                            });
                        }
                        if let Some(expected) = command.expected_stream_sequence
                            && expected != run.stream_sequence
                        {
                            return Err(HttpRegistryError::StaleCommandSequence {
                                run_id: run_id.to_owned(),
                                expected,
                                actual: run.stream_sequence,
                            });
                        }
                    }
                    let approval_request_id = command.payload.approval_request_id.clone();
                    let outcome =
                        self.submit_approval_decision_with_route(run_id, call_id, command.payload)?;
                    Ok(HttpApprovalCommandReceipt {
                        command_id: command.command_id,
                        client_id: command.client_id,
                        session_id: command.session_id,
                        run_id: run_id.to_owned(),
                        call_id: call_id.to_owned(),
                        approval_request_id,
                        expected_stream_sequence: command.expected_stream_sequence,
                        correlation_id: command.correlation_id,
                        decision: outcome.decision,
                        route_state: outcome.route_state,
                        registry_revision: outcome.registry_revision,
                        replayed: false,
                    })
                })();
            completion.complete(HttpCommandCompletion::Approval(result.clone()))?;
            result
        }
    }

    fn submit_approval_command_via_application(
        &self,
        run_id: &str,
        call_id: &str,
        command: HttpCommandEnvelope<HttpApprovalDecisionRequest>,
        client: crate::application_bridge::HttpApplicationClient,
    ) -> Result<HttpApprovalCommandReceipt, HttpRegistryError> {
        client.refresh().map_err(application_registry_error)?;
        let request = command.payload.clone();
        if request.approval_request_id.trim().is_empty()
            || request.tool_call_hash.trim().is_empty()
            || request.policy_version.trim().is_empty()
        {
            return Err(HttpRegistryError::InvalidApprovalRequest {
                message: "approval command guard fields must not be empty".to_owned(),
            });
        }
        let binding = format!("{run_id}:{call_id}:{}", request.approval_request_id);
        let mut resolution = crate::application_bridge::application_approval_resolution(&request)
            .map_err(application_registry_error)?;
        resolution.expected_stream_sequence = command.expected_stream_sequence;
        let accepted = !matches!(
            resolution.decision,
            sigil_application::ApplicationApprovalDecision::Deny
        );
        let receipt = client
            .execute(
                &command.command_id,
                sigil_application::ApplicationCommand::Approval(
                    sigil_application::ApprovalCommand::Resolve {
                        binding,
                        accepted,
                        resolution: Some(Box::new(resolution)),
                    },
                ),
            )
            .map_err(application_registry_error)?;
        let ((route_state, registry_revision), replayed) = match &receipt {
            sigil_application::ApplicationCommandReceipt::Uncertain(receipt) => (
                parse_application_approval_recovery(&receipt.recovery_binding)?,
                false,
            ),
            sigil_application::ApplicationCommandReceipt::ReplayedUncertain(receipt) => (
                parse_application_approval_recovery(&receipt.recovery_binding)?,
                true,
            ),
            sigil_application::ApplicationCommandReceipt::Rejected(rejection) => {
                return Err(HttpRegistryError::InvalidApprovalRequest {
                    message: rejection.reason.clone(),
                });
            }
            sigil_application::ApplicationCommandReceipt::PayloadConflict(_) => {
                return Err(HttpRegistryError::CommandKeyConflict {
                    session_id: command.session_id,
                    client_id: command.client_id,
                    command_id: command.command_id,
                });
            }
            sigil_application::ApplicationCommandReceipt::InFlight(_) => {
                return Err(HttpRegistryError::ApprovalDecisionUnavailable {
                    run_id: run_id.to_owned(),
                    call_id: call_id.to_owned(),
                });
            }
            sigil_application::ApplicationCommandReceipt::Settled(_)
            | sigil_application::ApplicationCommandReceipt::Replayed(_) => {
                return Err(HttpRegistryError::DriverRejected {
                    operation: "application approval",
                    run_id: run_id.to_owned(),
                    message: "application approval returned an invalid settled receipt".to_owned(),
                });
            }
        };
        Ok(HttpApprovalCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
            approval_request_id: request.approval_request_id.clone(),
            expected_stream_sequence: command.expected_stream_sequence,
            correlation_id: command.correlation_id,
            decision: HttpApprovalDecisionRecord {
                run_id: run_id.to_owned(),
                call_id: call_id.to_owned(),
                decision: request.decision.to_user_decision(),
                family_pattern: request.family_pattern,
                reason: request.reason,
            },
            route_state,
            registry_revision,
            replayed,
        })
    }

    pub(crate) fn submit_approval_decision_from_application(
        &self,
        run_id: &str,
        call_id: &str,
        request: HttpApprovalDecisionRequest,
        expected_stream_sequence: Option<u64>,
    ) -> Result<(HttpApprovalRouteState, u64), HttpRegistryError> {
        if let Some(expected) = expected_stream_sequence {
            let actual = self.get_run(run_id)?.stream_sequence;
            if expected != actual {
                return Err(HttpRegistryError::StaleCommandSequence {
                    run_id: run_id.to_owned(),
                    expected,
                    actual,
                });
            }
        }
        let outcome = self.submit_approval_decision_with_route(run_id, call_id, request)?;
        Ok((outcome.route_state, outcome.registry_revision))
    }

    /// Routes one user approval decision to an active run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run or call is unknown, the run cannot accept approval work, or the
    /// driver rejects the decision.
    pub fn submit_approval_decision(
        &self,
        run_id: &str,
        call_id: &str,
        request: HttpApprovalDecisionRequest,
    ) -> Result<HttpApprovalDecisionRecord, HttpRegistryError> {
        let outcome = self.submit_approval_decision_with_route(run_id, call_id, request)?;
        if outcome.route_state == HttpApprovalRouteState::DeliveryUncertain {
            return Err(HttpRegistryError::DriverPanicked {
                operation: "approval",
                run_id: run_id.to_owned(),
            });
        }
        Ok(outcome.decision)
    }

    fn submit_approval_decision_with_route(
        &self,
        run_id: &str,
        call_id: &str,
        request: HttpApprovalDecisionRequest,
    ) -> Result<HttpApprovalRouteOutcome, HttpRegistryError> {
        // An `approve_for_family` decision persists the durable rule BEFORE the pending approval
        // is consumed, so a persist failure leaves the approval pending and recoverable.
        if request.decision == HttpApprovalDecision::ApproveForFamily {
            let pattern = request.family_pattern.clone().ok_or_else(|| {
                HttpRegistryError::InvalidApprovalRequest {
                    message: "approve_for_family requires a family_pattern".to_owned(),
                }
            })?;
            let config_path = self.config_path.clone().ok_or_else(|| {
                HttpRegistryError::InvalidApprovalRequest {
                    message: "approve_for_family requires a bound config path".to_owned(),
                }
            })?;
            sigil_runtime::command_permission::append_command_allow_pattern(&config_path, &pattern)
                .map_err(|error| HttpRegistryError::InvalidApprovalRequest {
                    message: format!("failed to persist command family rule: {error}"),
                })?;
        }
        let (session_id, record) = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if let Some(error) = run.approval_route_error(run_id, false) {
                return Err(error);
            }
            let pending = run.pending_approvals.get(call_id).ok_or_else(|| {
                HttpRegistryError::ApprovalNotPending {
                    run_id: run_id.to_owned(),
                    call_id: call_id.to_owned(),
                }
            })?;
            validate_approval_guard(run_id, call_id, pending, &request, current_unix_time_ms())?;
            let pending = run.pending_approvals.remove(call_id).ok_or_else(|| {
                HttpRegistryError::ApprovalNotPending {
                    run_id: run_id.to_owned(),
                    call_id: call_id.to_owned(),
                }
            })?;
            run.record_approval_lifecycle(pending.clone(), HttpApprovalLifecycleState::Resolving);
            run.in_flight_approvals.insert(call_id.to_owned(), pending);
            let record = HttpApprovalDecisionRecord {
                run_id: run_id.to_owned(),
                call_id: call_id.to_owned(),
                decision: request.decision.to_user_decision(),
                family_pattern: request.family_pattern.clone(),
                reason: request.reason,
            };
            (run.session_id.clone(), record)
        };

        let approval = HttpRunDriverApproval {
            session_id,
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
            approval_request_id: request.approval_request_id,
            decision: record.clone(),
        };
        match catch_unwind(AssertUnwindSafe(|| self.driver.submit_approval(approval))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let mut state = self.lock_state();
                if let Some(run) = state.runs.get_mut(run_id) {
                    run.restore_in_flight_approval(call_id);
                }
                return Err(HttpRegistryError::DriverRejected {
                    operation: "approval",
                    run_id: run_id.to_owned(),
                    message: error.message,
                });
            }
            Err(_) => {
                let snapshot = self.lock_state().mark_run_driver_uncertain(run_id)?;
                return Ok(HttpApprovalRouteOutcome {
                    decision: record,
                    route_state: HttpApprovalRouteState::DeliveryUncertain,
                    registry_revision: snapshot.stream_sequence,
                });
            }
        }

        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        if run.status.is_terminal() {
            return Ok(HttpApprovalRouteOutcome {
                decision: record,
                route_state: HttpApprovalRouteState::Terminal,
                registry_revision: run.stream_sequence,
            });
        }
        run.in_flight_approvals.remove(call_id);
        run.update_approval_lifecycle_state(call_id, HttpApprovalLifecycleState::DecisionAccepted);
        run.approval_decisions.push(record.clone());
        if run.pending_approvals.is_empty()
            && run.in_flight_approvals.is_empty()
            && run.status == HttpRunStatus::WaitingForApproval
        {
            run.status = HttpRunStatus::Running;
        }
        run.advance_stream_sequence();
        Ok(HttpApprovalRouteOutcome {
            decision: record,
            route_state: HttpApprovalRouteState::DecisionAccepted,
            registry_revision: run.stream_sequence,
        })
    }

    fn reserve_command(
        &self,
        key: HttpCommandKey,
        request: HttpReservedCommand,
    ) -> Result<HttpCommandClaim, HttpRegistryError> {
        let mut state = self.lock_state();
        if let Some(existing) = state.command_reservations.get(&key) {
            if existing.request != request {
                return Err(HttpRegistryError::CommandKeyConflict {
                    session_id: key.session_id,
                    client_id: key.client_id,
                    command_id: key.command_id,
                });
            }
            return Ok(HttpCommandClaim::Wait(Arc::clone(existing)));
        }
        if !state.accepting_commands {
            return Err(HttpRegistryError::ServerShuttingDown);
        }
        let stored_identity = request.stored_identity(&key);
        if let Some(command_store) = &self.command_store {
            let claim = command_store
                .reserve(stored_identity.clone())
                .map_err(command_store_registry_error)?;
            match claim {
                HttpStoredCommandClaim::Conflict => {
                    return Err(HttpRegistryError::CommandKeyConflict {
                        session_id: key.session_id,
                        client_id: key.client_id,
                        command_id: key.command_id,
                    });
                }
                HttpStoredCommandClaim::Existing(completion) => {
                    let reservation = Arc::new(HttpCommandReservation::completed(
                        request,
                        Arc::clone(command_store),
                        stored_identity,
                        *completion,
                    ));
                    state
                        .command_reservations
                        .insert(key, Arc::clone(&reservation));
                    return Ok(HttpCommandClaim::Wait(reservation));
                }
                HttpStoredCommandClaim::Execute => {}
            }
        } else if state.command_reservations.len() >= self.in_memory_command_capacity {
            return Err(HttpRegistryError::CommandRegistrySaturated);
        }
        let reservation = Arc::new(HttpCommandReservation::new(
            request,
            self.command_store.clone(),
            stored_identity,
        ));
        state
            .command_reservations
            .insert(key.clone(), Arc::clone(&reservation));
        Ok(HttpCommandClaim::Execute(reservation))
    }

    #[cfg(test)]
    fn release_retryable_user_input_reservation(
        &self,
        key: &HttpCommandKey,
        reservation: &Arc<HttpCommandReservation>,
    ) {
        let mut state = self.lock_state();
        if state
            .command_reservations
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, reservation))
        {
            state.command_reservations.remove(key);
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, HttpRegistryState> {
        self.state
            .lock()
            .expect("http registry state lock should not be poisoned")
    }

    fn queue_command_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .queue_command_locks
            .lock()
            .expect("http queue command lock registry should not be poisoned");
        Arc::clone(
            locks
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }
}

fn http_run_admission_registry_error(error: HttpRunAdmissionError) -> HttpRegistryError {
    let recovery = match error {
        HttpRunAdmissionError::RouteRecovery(recovery) => recovery,
        HttpRunAdmissionError::SessionAlreadyActive { recovery_binding } => {
            HttpSessionRouteRecoveryView {
                code: HttpSessionRouteRecoveryCode::SessionAlreadyActive,
                allowed_actions: vec![
                    HttpSessionRouteRecoveryAction::RetrySessionAttach,
                    HttpSessionRouteRecoveryAction::StartNewSession,
                    HttpSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding,
                retryable: true,
            }
        }
        HttpRunAdmissionError::Unavailable => {
            return HttpRegistryError::DurableSessionUnavailable;
        }
    };
    HttpRegistryError::SessionRunRecoveryRequired { recovery }
}

struct HttpRegistryState {
    sessions: BTreeMap<String, HttpSessionState>,
    runs: BTreeMap<String, HttpRunState>,
    command_reservations: BTreeMap<HttpCommandKey, Arc<HttpCommandReservation>>,
    next_session_number: u64,
    next_run_number: u64,
    accepting_commands: bool,
    id_namespace: Option<String>,
    durable_session_mutations: BTreeSet<String>,
    active_queue_session_commands: BTreeSet<String>,
}

impl Default for HttpRegistryState {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            runs: BTreeMap::new(),
            command_reservations: BTreeMap::new(),
            next_session_number: 0,
            next_run_number: 0,
            accepting_commands: true,
            id_namespace: None,
            durable_session_mutations: BTreeSet::new(),
            active_queue_session_commands: BTreeSet::new(),
        }
    }
}

struct HttpQueueSessionCommandGuard<'a> {
    registry: &'a HttpSessionRunRegistry,
    durable_session_id: String,
}

impl Drop for HttpQueueSessionCommandGuard<'_> {
    fn drop(&mut self) {
        self.registry
            .lock_state()
            .active_queue_session_commands
            .remove(&self.durable_session_id);
    }
}

pub(crate) struct HttpDurableSessionMutationGuard<'a> {
    registry: &'a HttpSessionRunRegistry,
    durable_session_id: String,
    released: bool,
}

impl HttpDurableSessionMutationGuard<'_> {
    pub(crate) fn finish(mut self, evict_adapter_sessions: bool) {
        if evict_adapter_sessions {
            self.registry
                .driver
                .purge_session_local_state(&self.durable_session_id);
        }
        let mut state = self.registry.lock_state();
        let adapter_session_ids = if evict_adapter_sessions {
            let adapter_session_ids = state
                .sessions
                .values()
                .filter(|session| session.binding.session_scope_id == self.durable_session_id)
                .map(|session| session.id.clone())
                .collect::<BTreeSet<_>>();
            state
                .sessions
                .retain(|session_id, _| !adapter_session_ids.contains(session_id));
            state
                .runs
                .retain(|_, run| !adapter_session_ids.contains(&run.session_id));
            state
                .command_reservations
                .retain(|key, _| !adapter_session_ids.contains(&key.session_id));
            adapter_session_ids
        } else {
            BTreeSet::new()
        };
        state
            .durable_session_mutations
            .remove(&self.durable_session_id);
        drop(state);
        if !adapter_session_ids.is_empty() {
            self.registry
                .queue_command_locks
                .lock()
                .expect("http queue command lock registry should not be poisoned")
                .retain(|session_id, _| !adapter_session_ids.contains(session_id));
        }
        self.released = true;
    }
}

impl Drop for HttpDurableSessionMutationGuard<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.registry
            .lock_state()
            .durable_session_mutations
            .remove(&self.durable_session_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpCommandKey {
    session_id: String,
    client_id: String,
    command_id: String,
}

impl HttpCommandKey {
    fn from_envelope<T>(command: &HttpCommandEnvelope<T>) -> Self {
        Self {
            session_id: command.session_id.clone(),
            client_id: command.client_id.clone(),
            command_id: command.command_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpCommandKind {
    #[cfg(test)]
    Start,
    #[cfg(test)]
    Cancel,
    Pause,
    TerminalCancel,
    #[cfg(test)]
    Approval,
    Verification,
    Integration,
    PlanDecision,
    #[cfg(test)]
    UserInputDecision,
    IntentDrop,
    Queue,
    Recovery,
}

impl HttpCommandKind {
    const fn label(self) -> &'static [u8] {
        match self {
            #[cfg(test)]
            Self::Start => b"start",
            #[cfg(test)]
            Self::Cancel => b"cancel",
            Self::Pause => b"pause",
            Self::TerminalCancel => b"terminal_cancel",
            #[cfg(test)]
            Self::Approval => b"approval",
            Self::Verification => b"verification",
            Self::Integration => b"integration",
            Self::PlanDecision => b"plan-decision",
            #[cfg(test)]
            Self::UserInputDecision => b"user-input-decision",
            Self::IntentDrop => b"intent_drop",
            Self::Queue => b"queue",
            Self::Recovery => b"recovery",
        }
    }

    const fn name(self) -> &'static str {
        match self {
            #[cfg(test)]
            Self::Start => "start",
            #[cfg(test)]
            Self::Cancel => "cancel",
            Self::Pause => "pause",
            Self::TerminalCancel => "terminal_cancel",
            #[cfg(test)]
            Self::Approval => "approval",
            Self::Verification => "verification",
            Self::Integration => "integration",
            Self::PlanDecision => "plan_decision",
            #[cfg(test)]
            Self::UserInputDecision => "user_input_decision",
            Self::IntentDrop => "intent_drop",
            Self::Queue => "queue",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpReservedCommand {
    kind: HttpCommandKind,
    fingerprint: [u8; 32],
}

impl HttpReservedCommand {
    #[cfg(test)]
    fn start(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpRunStartRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Start, &[path_session_id], command)
    }

    #[cfg(test)]
    fn cancel(
        run_id: &str,
        command: &HttpCommandEnvelope<HttpRunCancelRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Cancel, &[run_id], command)
    }

    fn pause(
        run_id: &str,
        command: &HttpCommandEnvelope<HttpTaskPauseRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Pause, &[run_id], command)
    }

    fn terminal_cancel(
        run_id: &str,
        command: &HttpCommandEnvelope<HttpTerminalTaskCancelRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::TerminalCancel, &[run_id], command)
    }

    #[cfg(test)]
    fn approval(
        run_id: &str,
        call_id: &str,
        command: &HttpCommandEnvelope<HttpApprovalDecisionRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Approval, &[run_id, call_id], command)
    }

    fn verification(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpVerificationRerunRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Verification, &[path_session_id], command)
    }

    fn integration(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpTaskIntegrationReviewRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Integration, &[path_session_id], command)
    }

    fn plan_decision(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpPlanDecisionRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::PlanDecision, &[path_session_id], command)
    }

    #[cfg(test)]
    fn user_input_decision(
        path_session_id: &str,
        request_id: &str,
        command: &HttpCommandEnvelope<HttpUserInputDecisionRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(
            HttpCommandKind::UserInputDecision,
            &[path_session_id, request_id],
            command,
        )
    }

    fn intent_drop(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpIntentDropRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::IntentDrop, &[path_session_id], command)
    }

    fn queue(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpConversationQueueCommandRequest>,
    ) -> Result<Self, HttpRegistryError> {
        let encoded = secret_safe_queue_command_fingerprint_payload(command)?;
        Self::new_encoded(HttpCommandKind::Queue, &[path_session_id], &encoded)
    }

    fn recovery(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpConversationRecoveryCommandAction>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Recovery, &[path_session_id], command)
    }

    fn new<T>(
        kind: HttpCommandKind,
        targets: &[&str],
        command: &HttpCommandEnvelope<T>,
    ) -> Result<Self, HttpRegistryError>
    where
        T: Serialize,
    {
        let encoded = serde_json::to_vec(command)
            .map_err(|_| HttpRegistryError::CommandIdentityEncodingFailed)?;
        Self::new_encoded(kind, targets, &encoded)
    }

    fn new_encoded(
        kind: HttpCommandKind,
        targets: &[&str],
        encoded: &[u8],
    ) -> Result<Self, HttpRegistryError> {
        let mut hasher = Sha256::new();
        update_command_fingerprint_part(&mut hasher, kind.label());
        for target in targets {
            update_command_fingerprint_part(&mut hasher, target.as_bytes());
        }
        update_command_fingerprint_part(&mut hasher, encoded);
        Ok(Self {
            kind,
            fingerprint: hasher.finalize().into(),
        })
    }

    fn stored_identity(&self, key: &HttpCommandKey) -> HttpStoredCommandIdentity {
        HttpStoredCommandIdentity {
            key: HttpStoredCommandKey {
                session_id: key.session_id.clone(),
                client_id: key.client_id.clone(),
                command_id: key.command_id.clone(),
            },
            kind: self.kind.name().to_owned(),
            fingerprint_sha256: hex_lower(&self.fingerprint),
        }
    }
}

#[derive(Debug, Clone)]
enum HttpCommandCompletion {
    Start(Result<HttpRunStartCommandReceipt, HttpRegistryError>),
    Cancel(Result<HttpRunCancelCommandReceipt, HttpRegistryError>),
    Pause(Result<HttpTaskPauseCommandReceipt, HttpRegistryError>),
    TerminalCancel(Result<HttpTerminalTaskCancelCommandReceipt, HttpRegistryError>),
    Approval(Result<HttpApprovalCommandReceipt, HttpRegistryError>),
    Verification(Box<Result<HttpVerificationRerunCommandReceipt, HttpRegistryError>>),
    Integration(Box<Result<HttpTaskIntegrationAcceptanceCommandReceipt, HttpRegistryError>>),
    PlanDecision(Box<Result<HttpPlanDecisionCommandReceipt, HttpRegistryError>>),
    UserInputDecision(Box<Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError>>),
    IntentDrop(Box<Result<HttpIntentDropCommandReceipt, HttpRegistryError>>),
    Queue(Box<Result<HttpConversationQueueCommandReceipt, HttpRegistryError>>),
    Recovery(Box<Result<HttpConversationRecoveryCommandReceipt, HttpRegistryError>>),
    Aborted,
}

impl HttpCommandCompletion {
    fn stored_projection(&self) -> HttpStoredCommandCompletion {
        match self {
            Self::Start(Ok(receipt)) => {
                let mut receipt = receipt.clone();
                project_stored_run_snapshot(&mut receipt.run);
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Start(receipt)
            }
            Self::Cancel(Ok(receipt)) => {
                let mut receipt = receipt.clone();
                project_stored_run_snapshot(&mut receipt.run);
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Cancel(receipt)
            }
            Self::Pause(Ok(receipt)) => {
                let mut receipt = receipt.clone();
                project_stored_run_snapshot(&mut receipt.run);
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Pause(receipt)
            }
            Self::TerminalCancel(Ok(receipt)) => {
                let mut receipt = receipt.clone();
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::TerminalCancel(receipt)
            }
            Self::Approval(Ok(receipt)) => {
                let mut receipt = receipt.clone();
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                receipt.decision.reason = receipt
                    .decision
                    .reason
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Approval(receipt)
            }
            Self::Verification(result) if result.is_ok() => {
                let mut receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful verification completion should contain a receipt")
                    .clone();
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Verification(Box::new(receipt))
            }
            Self::Integration(result) if result.is_ok() => {
                let mut receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful integration completion should contain a receipt")
                    .clone();
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                receipt.acceptance.promotion_cleanup_error = receipt
                    .acceptance
                    .promotion_cleanup_error
                    .map(|value| safe_persistence_text(&value));
                receipt.acceptance.parent_cleanup_error = receipt
                    .acceptance
                    .parent_cleanup_error
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Integration(Box::new(receipt))
            }
            Self::PlanDecision(result) if result.is_ok() => {
                let receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful plan decision completion should contain a receipt")
                    .clone();
                HttpStoredCommandCompletion::PlanDecision(receipt)
            }
            Self::UserInputDecision(result) if result.is_ok() => {
                let receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful user-input completion should contain a receipt")
                    .clone();
                HttpStoredCommandCompletion::UserInputDecision(receipt)
            }
            Self::IntentDrop(result) if result.is_ok() => {
                let mut receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful Intent Drop completion should contain a receipt")
                    .clone();
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::IntentDrop(Box::new(receipt))
            }
            Self::Queue(result) if result.is_ok() => {
                let mut receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful queue completion should contain a receipt")
                    .clone();
                project_stored_conversation_queue(&mut receipt.queue);
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Queue(Box::new(receipt))
            }
            Self::Recovery(result) if result.is_ok() => {
                let mut receipt = result
                    .as_ref()
                    .as_ref()
                    .expect("successful recovery completion should contain a receipt")
                    .clone();
                receipt.correlation_id = receipt
                    .correlation_id
                    .map(|value| safe_persistence_text(&value));
                HttpStoredCommandCompletion::Recovery(Box::new(receipt))
            }
            Self::Start(Err(_))
            | Self::Cancel(Err(_))
            | Self::Pause(Err(_))
            | Self::TerminalCancel(Err(_))
            | Self::Approval(Err(_))
            | Self::Verification(_)
            | Self::Integration(_)
            | Self::IntentDrop(_)
            | Self::Queue(_)
            | Self::Recovery(_)
            | Self::PlanDecision(_)
            | Self::UserInputDecision(_)
            | Self::Aborted => HttpStoredCommandCompletion::Aborted,
        }
    }

    fn from_stored(completion: HttpStoredCommandCompletion) -> Self {
        match completion {
            HttpStoredCommandCompletion::Start(receipt) => Self::Start(Ok(receipt)),
            HttpStoredCommandCompletion::Cancel(receipt) => Self::Cancel(Ok(receipt)),
            HttpStoredCommandCompletion::Pause(receipt) => Self::Pause(Ok(receipt)),
            HttpStoredCommandCompletion::TerminalCancel(receipt) => {
                Self::TerminalCancel(Ok(receipt))
            }
            HttpStoredCommandCompletion::Approval(receipt) => Self::Approval(Ok(receipt)),
            HttpStoredCommandCompletion::Verification(receipt) => {
                Self::Verification(Box::new(Ok(*receipt)))
            }
            HttpStoredCommandCompletion::Integration(receipt) => {
                Self::Integration(Box::new(Ok(*receipt)))
            }
            HttpStoredCommandCompletion::PlanDecision(receipt) => {
                Self::PlanDecision(Box::new(Ok(receipt)))
            }
            HttpStoredCommandCompletion::UserInputDecision(receipt) => {
                Self::UserInputDecision(Box::new(Ok(receipt)))
            }
            HttpStoredCommandCompletion::IntentDrop(receipt) => {
                Self::IntentDrop(Box::new(Ok(*receipt)))
            }
            HttpStoredCommandCompletion::Queue(receipt) => Self::Queue(Box::new(Ok(*receipt))),
            HttpStoredCommandCompletion::Recovery(receipt) => {
                Self::Recovery(Box::new(Ok(*receipt)))
            }
            HttpStoredCommandCompletion::Reserved | HttpStoredCommandCompletion::Aborted => {
                Self::Aborted
            }
        }
    }
}

fn project_stored_run_snapshot(run: &mut HttpRunSnapshot) {
    run.prompt_preview = HTTP_DURABLE_COMMAND_PROMPT_OMISSION.to_owned();
    for approval in &mut run.pending_approvals {
        approval.call_id = safe_persistence_text(&approval.call_id);
        approval.tool_name = safe_persistence_text(&approval.tool_name);
        approval.approval_request_id = safe_persistence_text(&approval.approval_request_id);
        approval.policy_version = safe_persistence_text(&approval.policy_version);
        approval.display.safe_summary_title =
            safe_persistence_text(&approval.display.safe_summary_title);
        approval.display.safe_summary_detail =
            safe_persistence_text(&approval.display.safe_summary_detail);
    }
}

fn application_registry_error(error: sigil_application::ApplicationError) -> HttpRegistryError {
    HttpRegistryError::DriverRejected {
        operation: "application command",
        run_id: "application".to_owned(),
        message: error.to_string(),
    }
}

fn project_stored_conversation_queue(queue: &mut HttpConversationQueueView) {
    for item in &mut queue.items {
        if item.prompt_material == HttpConversationQueuePromptMaterial::AvailableProcessLocal {
            item.prompt_material = HttpConversationQueuePromptMaterial::RequiresReentry;
            item.dispatchable = false;
            item.blocked_reason = Some(HttpConversationQueueBlockedReason::RequiresReentry);
        }
    }
    queue.next_dispatchable_entry_id = queue
        .items
        .iter()
        .find(|item| item.dispatchable)
        .map(|item| item.entry_id.clone());
}

struct HttpCommandReservation {
    request: HttpReservedCommand,
    completion: Mutex<Option<HttpCommandCompletion>>,
    ready: Condvar,
    waiters: AtomicUsize,
    command_store: Option<Arc<HttpDurableCommandStore>>,
    stored_identity: HttpStoredCommandIdentity,
}

struct HttpCancelOperation {
    completion: Mutex<Option<Result<(), HttpRegistryError>>>,
    ready: Condvar,
    waiters: AtomicUsize,
}

enum HttpCancelClaim {
    Execute {
        operation: Arc<HttpCancelOperation>,
        cancel: HttpRunDriverCancel,
    },
    Wait(Arc<HttpCancelOperation>),
}

enum HttpPauseClaim {
    Execute {
        operation: Arc<HttpCancelOperation>,
        pause: HttpRunDriverTaskPause,
    },
    Wait(Arc<HttpCancelOperation>),
}

impl HttpCancelOperation {
    fn new() -> Self {
        Self {
            completion: Mutex::new(None),
            ready: Condvar::new(),
            waiters: AtomicUsize::new(0),
        }
    }

    fn complete(&self, result: Result<(), HttpRegistryError>) {
        let mut slot = self
            .completion
            .lock()
            .expect("http cancel completion lock should not be poisoned");
        if slot.is_none() {
            *slot = Some(result);
            self.ready.notify_all();
        }
    }

    fn wait(&self) -> Result<(), HttpRegistryError> {
        let _waiter = HttpWaiterGuard::new(&self.waiters);
        let mut slot = self
            .completion
            .lock()
            .expect("http cancel completion lock should not be poisoned");
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            slot = self
                .ready
                .wait(slot)
                .expect("http cancel completion lock should not be poisoned");
        }
    }

    fn waiter_count(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }
}

impl HttpCommandReservation {
    fn new(
        request: HttpReservedCommand,
        command_store: Option<Arc<HttpDurableCommandStore>>,
        stored_identity: HttpStoredCommandIdentity,
    ) -> Self {
        Self {
            request,
            completion: Mutex::new(None),
            ready: Condvar::new(),
            waiters: AtomicUsize::new(0),
            command_store,
            stored_identity,
        }
    }

    fn completed(
        request: HttpReservedCommand,
        command_store: Arc<HttpDurableCommandStore>,
        stored_identity: HttpStoredCommandIdentity,
        completion: HttpStoredCommandCompletion,
    ) -> Self {
        Self {
            request,
            completion: Mutex::new(Some(HttpCommandCompletion::from_stored(completion))),
            ready: Condvar::new(),
            waiters: AtomicUsize::new(0),
            command_store: Some(command_store),
            stored_identity,
        }
    }

    fn complete(&self, completion: HttpCommandCompletion) -> Result<(), HttpRegistryError> {
        if let Some(command_store) = &self.command_store
            && let Err(error) =
                command_store.complete(&self.stored_identity, completion.stored_projection())
        {
            let mut slot = self
                .completion
                .lock()
                .expect("http command completion lock should not be poisoned");
            if slot.is_none() {
                *slot = Some(HttpCommandCompletion::Aborted);
                self.ready.notify_all();
            }
            return Err(command_store_registry_error(error));
        }
        let mut slot = self
            .completion
            .lock()
            .expect("http command completion lock should not be poisoned");
        if slot.is_none() {
            *slot = Some(completion);
            self.ready.notify_all();
        }
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.completion
            .lock()
            .expect("http command completion lock should not be poisoned")
            .is_some()
    }

    fn waiter_count(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }

    fn wait(&self) -> HttpCommandCompletion {
        let _waiter = HttpWaiterGuard::new(&self.waiters);
        let mut slot = self
            .completion
            .lock()
            .expect("http command completion lock should not be poisoned");
        loop {
            if let Some(completion) = slot.as_ref() {
                return completion.clone();
            }
            slot = self
                .ready
                .wait(slot)
                .expect("http command completion lock should not be poisoned");
        }
    }

    #[cfg(test)]
    fn wait_for_start(&self) -> Result<HttpRunStartCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Start(result) => {
                result.map(HttpRunStartCommandReceipt::replayed)
            }
            HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    #[cfg(test)]
    fn wait_for_cancel(&self) -> Result<HttpRunCancelCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Cancel(result) => {
                result.map(HttpRunCancelCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_pause(&self) -> Result<HttpTaskPauseCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Pause(result) => {
                result.map(HttpTaskPauseCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_terminal_cancel(
        &self,
    ) -> Result<HttpTerminalTaskCancelCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::TerminalCancel(result) => {
                result.map(HttpTerminalTaskCancelCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    #[cfg(test)]
    fn wait_for_approval(&self) -> Result<HttpApprovalCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Approval(result) => {
                result.map(HttpApprovalCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_verification(
        &self,
    ) -> Result<HttpVerificationRerunCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Verification(result) => {
                (*result).map(HttpVerificationRerunCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_plan_decision(&self) -> Result<HttpPlanDecisionCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::PlanDecision(receipt) => match *receipt {
                Ok(receipt) => Ok(receipt.replayed()),
                Err(error) => Err(error),
            },
            _ => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    #[cfg(test)]
    fn wait_for_user_input_decision(
        &self,
    ) -> Result<HttpUserInputDecisionCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::UserInputDecision(receipt) => match *receipt {
                Ok(receipt) => Ok(receipt.replayed()),
                Err(error) => Err(error),
            },
            _ => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_integration(
        &self,
    ) -> Result<HttpTaskIntegrationAcceptanceCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Integration(result) => {
                (*result).map(HttpTaskIntegrationAcceptanceCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_intent_drop(&self) -> Result<HttpIntentDropCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::IntentDrop(result) => {
                (*result).map(HttpIntentDropCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_queue(&self) -> Result<HttpConversationQueueCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Queue(result) => {
                (*result).map(HttpConversationQueueCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Recovery(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_recovery(
        &self,
    ) -> Result<HttpConversationRecoveryCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Recovery(result) => {
                (*result).map(HttpConversationRecoveryCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Pause(_)
            | HttpCommandCompletion::TerminalCancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Integration(_)
            | HttpCommandCompletion::IntentDrop(_)
            | HttpCommandCompletion::Queue(_)
            | HttpCommandCompletion::PlanDecision(_)
            | HttpCommandCompletion::UserInputDecision(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }
}

struct HttpWaiterGuard<'a> {
    waiters: &'a AtomicUsize,
}

impl<'a> HttpWaiterGuard<'a> {
    fn new(waiters: &'a AtomicUsize) -> Self {
        waiters.fetch_add(1, Ordering::AcqRel);
        Self { waiters }
    }
}

impl Drop for HttpWaiterGuard<'_> {
    fn drop(&mut self) {
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}

enum HttpCommandClaim {
    Execute(Arc<HttpCommandReservation>),
    Wait(Arc<HttpCommandReservation>),
}

struct HttpCommandExecutionGuard {
    reservation: Arc<HttpCommandReservation>,
    completed: bool,
}

impl HttpCommandExecutionGuard {
    fn new(reservation: Arc<HttpCommandReservation>) -> Self {
        Self {
            reservation,
            completed: false,
        }
    }

    fn complete(&mut self, completion: HttpCommandCompletion) -> Result<(), HttpRegistryError> {
        self.completed = true;
        self.reservation.complete(completion)
    }
}

impl Drop for HttpCommandExecutionGuard {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.reservation.complete(HttpCommandCompletion::Aborted);
        }
    }
}

impl HttpRegistryState {
    fn with_id_namespace(id_namespace: String) -> Self {
        Self {
            id_namespace: Some(id_namespace),
            ..Self::default()
        }
    }

    fn ensure_accepting_commands(&self) -> Result<(), HttpRegistryError> {
        if self.accepting_commands {
            Ok(())
        } else {
            Err(HttpRegistryError::ServerShuttingDown)
        }
    }

    fn next_session_id(&mut self) -> String {
        self.next_session_number += 1;
        self.id_namespace.as_ref().map_or_else(
            || format!("http-session-{}", self.next_session_number),
            |namespace| format!("http-session-{namespace}-{}", self.next_session_number),
        )
    }

    fn next_run_id(&mut self) -> String {
        self.next_run_number += 1;
        self.id_namespace.as_ref().map_or_else(
            || format!("http-run-{}", self.next_run_number),
            |namespace| format!("http-run-{namespace}-{}", self.next_run_number),
        )
    }

    fn transition_run_terminal(
        &mut self,
        run_id: &str,
        outcome: HttpRunTerminalOutcome,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let requested = outcome.status();
        let session_id = {
            let run = self
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.status.is_terminal() {
                if run.status == requested {
                    return Ok(run.snapshot());
                }
                return Err(HttpRegistryError::RunTerminalConflict {
                    run_id: run_id.to_owned(),
                    current: run.status,
                    requested: outcome,
                });
            }
            run.status = requested;
            run.previous_status = None;
            run.cancel_operation = None;
            run.pause_operation = None;
            run.pending_approvals.clear();
            run.in_flight_approvals.clear();
            for lifecycle in run.approval_lifecycles.values_mut() {
                lifecycle.state = HttpApprovalLifecycleState::Terminal;
            }
            run.advance_stream_sequence();
            run.session_id.clone()
        };
        let session = self.sessions.get_mut(&session_id).ok_or_else(|| {
            HttpRegistryError::SessionNotFound {
                session_id: session_id.clone(),
            }
        })?;
        if session.foreground_run_id.as_deref() == Some(run_id) {
            session.foreground_run_id = None;
            session.foreground_owner_generation =
                session.foreground_owner_generation.saturating_add(1);
        }
        self.runs
            .get(run_id)
            .map(HttpRunState::snapshot)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })
    }

    fn clear_run_release_barrier(&mut self, run_id: &str) {
        for session in self.sessions.values_mut() {
            if session.release_pending_run_id.as_deref() == Some(run_id) {
                session.release_pending_run_id = None;
            }
        }
    }

    fn rollback_queued_run_registration(&mut self, run_id: &str) {
        let Some(run) = self.runs.remove(run_id) else {
            return;
        };
        if let Some(session) = self.sessions.get_mut(&run.session_id) {
            session.run_ids.retain(|current| current != run_id);
            if session.foreground_run_id.as_deref() == Some(run_id) {
                session.foreground_run_id = None;
                session.foreground_owner_generation =
                    session.foreground_owner_generation.saturating_add(1);
            }
            if session.release_pending_run_id.as_deref() == Some(run_id) {
                session.release_pending_run_id = None;
            }
        }
    }

    fn mark_run_driver_uncertain(
        &mut self,
        run_id: &str,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
        if !run.status.is_terminal() {
            run.status = HttpRunStatus::ExecutionUncertain;
            run.previous_status = None;
            run.cancel_operation = None;
            run.pause_operation = None;
            run.pending_approvals.clear();
            let uncertain = run
                .in_flight_approvals
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for approval in uncertain {
                run.record_approval_lifecycle(
                    approval,
                    HttpApprovalLifecycleState::DeliveryUncertain,
                );
            }
            run.in_flight_approvals.clear();
            run.advance_stream_sequence();
        }
        Ok(run.snapshot())
    }
}

struct HttpSessionState {
    id: String,
    label: Option<String>,
    run_ids: Vec<String>,
    binding: HttpSessionBinding,
    foreground_run_id: Option<String>,
    release_pending_run_id: Option<String>,
    foreground_owner_generation: u64,
    verification_in_progress: bool,
    route_transition: Option<crate::HttpSessionRouteTransitionView>,
}

impl HttpSessionState {
    fn foreground_owner(&self) -> Option<HttpForegroundRunOwner> {
        self.foreground_run_id
            .as_ref()
            .map(|run_id| HttpForegroundRunOwner {
                run_id: run_id.clone(),
                owner_revision: self.foreground_owner_revision(),
            })
    }

    fn foreground_owner_revision(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"sigil-http-foreground-owner-v1\0");
        update_owner_revision_component(&mut hasher, self.id.as_bytes());
        update_owner_revision_component(&mut hasher, self.binding.session_scope_id.as_bytes());
        update_owner_revision_component(
            &mut hasher,
            &self.foreground_owner_generation.to_be_bytes(),
        );
        update_owner_revision_component(
            &mut hasher,
            self.foreground_run_id.as_deref().unwrap_or("").as_bytes(),
        );
        format!("sha256:{:x}", hasher.finalize())
    }

    fn snapshot(&self) -> HttpSessionSnapshot {
        HttpSessionSnapshot {
            id: self.id.clone(),
            label: self.label.clone(),
            run_ids: self.run_ids.clone(),
            durable_session_scope_id: self.binding.session_scope_id.clone(),
            session_log_path: self.binding.session_log_path.clone(),
            foreground_run_id: self.foreground_run_id.clone(),
            route_transition: self.route_transition.clone(),
            route_recovery: self.binding.route_recovery.clone(),
        }
    }
}

fn update_owner_revision_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

struct HttpRunState {
    id: String,
    session_id: String,
    status: HttpRunStatus,
    previous_status: Option<HttpRunStatus>,
    cancel_operation: Option<Arc<HttpCancelOperation>>,
    pause_operation: Option<Arc<HttpCancelOperation>>,
    permission_mode: HttpPermissionMode,
    reasoning_effort: Option<HttpReasoningEffort>,
    prompt_preview: String,
    pending_approvals: BTreeMap<String, HttpPendingApproval>,
    in_flight_approvals: BTreeMap<String, HttpPendingApproval>,
    approval_lifecycles: BTreeMap<String, HttpApprovalLifecycleView>,
    approval_decisions: Vec<HttpApprovalDecisionRecord>,
    terminal_tasks: BTreeMap<String, HttpTerminalLifecycleView>,
    stream_sequence: u64,
}

impl HttpRunState {
    fn new(
        id: String,
        session_id: String,
        permission_mode: HttpPermissionMode,
        reasoning_effort: Option<HttpReasoningEffort>,
        prompt_preview: String,
    ) -> Self {
        Self {
            id,
            session_id,
            status: HttpRunStatus::Starting,
            previous_status: None,
            cancel_operation: None,
            pause_operation: None,
            permission_mode,
            reasoning_effort,
            prompt_preview,
            pending_approvals: BTreeMap::new(),
            in_flight_approvals: BTreeMap::new(),
            approval_lifecycles: BTreeMap::new(),
            approval_decisions: Vec::new(),
            terminal_tasks: BTreeMap::new(),
            stream_sequence: 0,
        }
    }

    fn snapshot(&self) -> HttpRunSnapshot {
        HttpRunSnapshot {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            status: self.status,
            permission_mode: self.permission_mode,
            reasoning_effort: self.reasoning_effort,
            prompt_preview: self.prompt_preview.clone(),
            pending_approvals: self.pending_approvals.values().cloned().collect(),
            approval_lifecycles: self.approval_lifecycles.values().cloned().collect(),
            terminal_tasks: self.terminal_tasks.values().cloned().collect(),
            stream_sequence: self.stream_sequence,
        }
    }

    fn approval_route_error(
        &self,
        run_id: &str,
        allow_starting: bool,
    ) -> Option<HttpRegistryError> {
        let status_accepts_approval = matches!(
            (self.status, allow_starting),
            (HttpRunStatus::Starting, true)
                | (HttpRunStatus::Running, _)
                | (HttpRunStatus::WaitingForApproval, _)
        );
        if !status_accepts_approval {
            return Some(HttpRegistryError::RunNotActive {
                run_id: run_id.to_owned(),
            });
        }
        None
    }

    fn restore_previous_status_if_cancel_requested(&mut self) {
        if self.status == HttpRunStatus::CancelRequested
            && let Some(previous) = self.previous_status.take()
        {
            self.status = previous;
        }
    }

    fn restore_previous_status_if_pause_requested(&mut self) {
        if self.status == HttpRunStatus::PauseRequested
            && let Some(previous) = self.previous_status.take()
        {
            self.status = previous;
        }
    }

    fn restore_in_flight_approval(&mut self, call_id: &str) {
        if self.status.is_terminal() {
            return;
        }
        if let Some(approval) = self.in_flight_approvals.remove(call_id) {
            self.pending_approvals.insert(call_id.to_owned(), approval);
            self.approval_lifecycles.remove(call_id);
            self.advance_stream_sequence();
        }
    }

    fn record_approval_lifecycle(
        &mut self,
        approval: HttpPendingApproval,
        state: HttpApprovalLifecycleState,
    ) {
        let call_id = approval.call_id.clone();
        self.approval_lifecycles
            .insert(call_id, HttpApprovalLifecycleView { approval, state });
        while self.approval_lifecycles.len() > MAX_APPROVAL_LIFECYCLES_PER_RUN {
            let Some(terminal_call_id) =
                self.approval_lifecycles
                    .iter()
                    .find_map(|(call_id, lifecycle)| {
                        (lifecycle.state == HttpApprovalLifecycleState::Terminal)
                            .then(|| call_id.clone())
                    })
            else {
                break;
            };
            self.approval_lifecycles.remove(&terminal_call_id);
        }
    }

    fn update_approval_lifecycle_state(
        &mut self,
        call_id: &str,
        state: HttpApprovalLifecycleState,
    ) -> bool {
        let Some(lifecycle) = self.approval_lifecycles.get_mut(call_id) else {
            return false;
        };
        if lifecycle.state == state {
            return false;
        }
        lifecycle.state = state;
        true
    }

    fn advance_stream_sequence(&mut self) {
        self.stream_sequence = self.stream_sequence.saturating_add(1);
    }
}

struct HttpApprovalRouteOutcome {
    decision: HttpApprovalDecisionRecord,
    route_state: HttpApprovalRouteState,
    registry_revision: u64,
}

fn parse_application_approval_recovery(
    binding: &str,
) -> Result<(HttpApprovalRouteState, u64), HttpRegistryError> {
    let Some(value) = binding.strip_prefix("http-approval:") else {
        return Err(HttpRegistryError::DriverRejected {
            operation: "application approval",
            run_id: "unknown".to_owned(),
            message: "application approval recovery binding is malformed".to_owned(),
        });
    };
    let Some((route, revision)) = value.split_once(':') else {
        return Err(HttpRegistryError::DriverRejected {
            operation: "application approval",
            run_id: "unknown".to_owned(),
            message: "application approval recovery binding is incomplete".to_owned(),
        });
    };
    let route_state = match route {
        "accepted" => HttpApprovalRouteState::DecisionAccepted,
        "uncertain" => HttpApprovalRouteState::DeliveryUncertain,
        "terminal" => HttpApprovalRouteState::Terminal,
        _ => {
            return Err(HttpRegistryError::DriverRejected {
                operation: "application approval",
                run_id: "unknown".to_owned(),
                message: "application approval recovery route is unknown".to_owned(),
            });
        }
    };
    let registry_revision =
        revision
            .parse::<u64>()
            .map_err(|_| HttpRegistryError::DriverRejected {
                operation: "application approval",
                run_id: "unknown".to_owned(),
                message: "application approval recovery revision is invalid".to_owned(),
            })?;
    Ok((route_state, registry_revision))
}

fn parse_application_user_input_recovery(
    binding: &str,
    expected_request_id: &str,
) -> Result<Option<String>, HttpRegistryError> {
    let Some(value) = binding.strip_prefix("http-user-input:") else {
        return Err(HttpRegistryError::DriverRejected {
            operation: "application user input decision",
            run_id: expected_request_id.to_owned(),
            message: "user-input recovery binding is malformed".to_owned(),
        });
    };
    let Some((request_id, continuation)) = value.split_once(':') else {
        return Err(HttpRegistryError::DriverRejected {
            operation: "application user input decision",
            run_id: expected_request_id.to_owned(),
            message: "user-input recovery binding is incomplete".to_owned(),
        });
    };
    let request_id = decode_hex_binding_component(request_id).ok_or_else(|| {
        HttpRegistryError::DriverRejected {
            operation: "application user input decision",
            run_id: expected_request_id.to_owned(),
            message: "user-input recovery request identity is invalid".to_owned(),
        }
    })?;
    if request_id != expected_request_id {
        return Err(HttpRegistryError::DriverRejected {
            operation: "application user input decision",
            run_id: expected_request_id.to_owned(),
            message: "user-input recovery request identity does not match the route".to_owned(),
        });
    }
    let continuation = decode_hex_binding_component(continuation).ok_or_else(|| {
        HttpRegistryError::DriverRejected {
            operation: "application user input decision",
            run_id: expected_request_id.to_owned(),
            message: "user-input continuation identity is invalid".to_owned(),
        }
    })?;
    Ok((!continuation.is_empty()).then_some(continuation))
}

fn decode_hex_binding_component(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

fn prompt_preview(prompt: &str) -> String {
    const MAX_PROMPT_PREVIEW_CHARS: usize = 120;
    let mut preview = prompt
        .chars()
        .take(MAX_PROMPT_PREVIEW_CHARS)
        .collect::<String>();
    if prompt.chars().count() > MAX_PROMPT_PREVIEW_CHARS {
        preview.push_str("...");
    }
    preview
}

fn update_command_fingerprint_part(hasher: &mut Sha256, part: &[u8]) {
    hasher.update((part.len() as u64).to_be_bytes());
    hasher.update(part);
}

/// Encodes the durable idempotency identity without exact prompt bytes.
///
/// Commands whose exact prompts collapse to the same secret-safe projection intentionally share
/// one durable identity. A user correction must therefore use a fresh command id; retaining a raw
/// or reversibly keyed exact-prompt digest in the durable command store would violate the queue's
/// process-local material boundary.
fn secret_safe_queue_command_fingerprint_payload(
    command: &HttpCommandEnvelope<HttpConversationQueueCommandRequest>,
) -> Result<Vec<u8>, HttpRegistryError> {
    let action = match &command.payload.action {
        HttpConversationQueueCommandAction::Enqueue {
            prompt,
            kind,
            reasoning_effort,
        } => {
            let projection = project_conversation_prompt_for_persistence(prompt);
            serde_json::json!({
                "action": "enqueue",
                "safe_prompt": projection.safe_prompt,
                "prompt_hash": projection.prompt_hash,
                "kind": kind,
                "reasoning_effort": reasoning_effort,
            })
        }
        HttpConversationQueueCommandAction::Edit {
            entry_id,
            prompt,
            reasoning_effort,
        } => {
            let projection = project_conversation_prompt_for_persistence(prompt);
            serde_json::json!({
                "action": "edit",
                "entry_id": entry_id,
                "safe_prompt": projection.safe_prompt,
                "prompt_hash": projection.prompt_hash,
                "reasoning_effort": reasoning_effort,
            })
        }
        HttpConversationQueueCommandAction::Remove { entry_id } => serde_json::json!({
            "action": "remove",
            "entry_id": entry_id,
        }),
        HttpConversationQueueCommandAction::Reorder {
            entry_id,
            after_entry_id,
        } => serde_json::json!({
            "action": "reorder",
            "entry_id": entry_id,
            "after_entry_id": after_entry_id,
        }),
        HttpConversationQueueCommandAction::Pause => serde_json::json!({
            "action": "pause",
        }),
        HttpConversationQueueCommandAction::Resume => serde_json::json!({
            "action": "resume",
        }),
        HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } => serde_json::json!({
            "action": "interrupt_and_run_next",
            "foreground_run_id": foreground_run_id,
            "foreground_owner_revision": foreground_owner_revision,
        }),
    };
    serde_json::to_vec(&serde_json::json!({
        "protocol_version": command.protocol_version,
        "command_id": command.command_id,
        "client_id": command.client_id,
        "session_id": command.session_id,
        "expected_stream_sequence": command.expected_stream_sequence,
        "correlation_id": command.correlation_id,
        "payload": {
            "expected_generation": command.payload.expected_generation,
            "action": action,
        },
    }))
    .map_err(|_| HttpRegistryError::CommandIdentityEncodingFailed)
}

fn command_store_registry_error(error: HttpCommandStoreError) -> HttpRegistryError {
    match error {
        HttpCommandStoreError::Saturated => HttpRegistryError::CommandRegistrySaturated,
        error => HttpRegistryError::CommandIdentityPersistenceFailed {
            message: error.to_string(),
        },
    }
}

fn queue_driver_registry_error(error: HttpConversationQueueDriverError) -> HttpRegistryError {
    match error {
        HttpConversationQueueDriverError::StaleGeneration => {
            HttpRegistryError::ConversationQueueGenerationStale
        }
        HttpConversationQueueDriverError::Terminal => {
            HttpRegistryError::ConversationQueueEntryTerminal
        }
        HttpConversationQueueDriverError::OwnerLost => {
            HttpRegistryError::ConversationQueueOwnerLost
        }
        HttpConversationQueueDriverError::Permission => {
            HttpRegistryError::ConversationQueuePermissionRequired
        }
        HttpConversationQueueDriverError::Conflict => HttpRegistryError::ConversationQueueConflict,
        HttpConversationQueueDriverError::RequiresReentry => {
            HttpRegistryError::ConversationQueueRequiresReentry
        }
        HttpConversationQueueDriverError::Unsupported => {
            HttpRegistryError::ConversationQueueUnsupported
        }
        HttpConversationQueueDriverError::Unavailable => {
            HttpRegistryError::ConversationQueueUnavailable
        }
    }
}

fn recovery_driver_registry_error(error: HttpConversationRecoveryDriverError) -> HttpRegistryError {
    match error {
        HttpConversationRecoveryDriverError::StaleBinding => {
            HttpRegistryError::ConversationRecoveryStaleBinding
        }
        HttpConversationRecoveryDriverError::Conflict => {
            HttpRegistryError::ConversationRecoveryConflict
        }
        HttpConversationRecoveryDriverError::Unavailable => {
            HttpRegistryError::ConversationRecoveryUnavailable
        }
    }
}

fn intent_stack_driver_registry_error(error: HttpIntentStackDriverError) -> HttpRegistryError {
    match error {
        HttpIntentStackDriverError::InvalidRequest => HttpRegistryError::IntentStackInvalidRequest,
        HttpIntentStackDriverError::Stale => HttpRegistryError::IntentStackStale,
        HttpIntentStackDriverError::PermissionRequired => {
            HttpRegistryError::IntentStackPermissionRequired
        }
        HttpIntentStackDriverError::Conflict => HttpRegistryError::IntentStackConflict,
        HttpIntentStackDriverError::Unavailable => HttpRegistryError::IntentStackUnavailable,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

fn validate_session_open_request(
    request: &HttpSessionOpenRequest,
) -> Result<SessionRef, HttpRegistryError> {
    let session_ref = request.session_ref.trim();
    let session_id = request.session_id.trim();
    if session_ref.is_empty()
        || session_ref != request.session_ref
        || session_ref.len() > MAX_SESSION_OPEN_REFERENCE_BYTES
        || session_ref.contains(['/', '\\'])
        || session_id.is_empty()
        || session_id != request.session_id
        || session_id.len() > MAX_SESSION_OPEN_ID_BYTES
        || safe_persistence_text(session_id) != session_id
        || request.label.as_ref().is_some_and(|label| {
            label.len() > MAX_SESSION_OPEN_LABEL_BYTES || safe_persistence_text(label) != *label
        })
        || request.recovery_binding.as_ref().is_some_and(|binding| {
            binding.trim().is_empty()
                || binding.len() > 128
                || safe_persistence_text(binding) != *binding
        })
    {
        return Err(HttpRegistryError::InvalidSessionOpenRequest);
    }
    let session_ref = SessionRef::new_relative(session_ref)
        .map_err(|_| HttpRegistryError::InvalidSessionOpenRequest)?;
    if session_ref
        .as_path()
        .extension()
        .and_then(|value| value.to_str())
        != Some("jsonl")
    {
        return Err(HttpRegistryError::InvalidSessionOpenRequest);
    }
    Ok(session_ref)
}

fn validate_conversation_queue_command(
    request: &HttpConversationQueueCommandRequest,
) -> Result<(), HttpRegistryError> {
    if !is_bounded_queue_token(&request.expected_generation.0) {
        return Err(HttpRegistryError::ConversationQueueInvalidCommand);
    }
    let valid_prompt = |prompt: &str| {
        !prompt.trim().is_empty() && prompt.len() <= MAX_CONVERSATION_QUEUE_PROMPT_BYTES
    };
    let valid = match &request.action {
        HttpConversationQueueCommandAction::Enqueue { prompt, .. } => valid_prompt(prompt),
        HttpConversationQueueCommandAction::Edit {
            entry_id, prompt, ..
        } => is_bounded_queue_token(entry_id) && valid_prompt(prompt),
        HttpConversationQueueCommandAction::Remove { entry_id } => is_bounded_queue_token(entry_id),
        HttpConversationQueueCommandAction::Reorder {
            entry_id,
            after_entry_id,
        } => {
            is_bounded_queue_token(entry_id)
                && after_entry_id.as_deref().is_none_or(is_bounded_queue_token)
        }
        HttpConversationQueueCommandAction::Pause | HttpConversationQueueCommandAction::Resume => {
            true
        }
        HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } => {
            is_bounded_queue_token(foreground_run_id)
                && is_bounded_queue_token(foreground_owner_revision)
        }
    };
    valid
        .then_some(())
        .ok_or(HttpRegistryError::ConversationQueueInvalidCommand)
}

fn validate_checkpoint_restore_request(
    request: &HttpCheckpointRestoreRequest,
) -> Result<(), HttpRegistryError> {
    if is_bounded_recovery_token(&request.checkpoint_id)
        && is_bounded_recovery_token(&request.checkpoint_digest)
    {
        Ok(())
    } else {
        Err(HttpRegistryError::ConversationRecoveryInvalidCommand)
    }
}

fn validate_conversation_recovery_command(
    action: &HttpConversationRecoveryCommandAction,
) -> Result<(), HttpRegistryError> {
    let valid = match action {
        HttpConversationRecoveryCommandAction::ApplyCompaction { preview_id } => {
            is_bounded_recovery_token(preview_id)
        }
        HttpConversationRecoveryCommandAction::PrepareCompaction { preview_id }
        | HttpConversationRecoveryCommandAction::ApplyStandaloneToolOutputShrink { preview_id } => {
            is_bounded_recovery_token(preview_id)
        }
        HttpConversationRecoveryCommandAction::RestoreCheckpoint {
            checkpoint_id,
            checkpoint_digest,
        } => {
            is_bounded_recovery_token(checkpoint_id) && is_bounded_recovery_token(checkpoint_digest)
        }
        HttpConversationRecoveryCommandAction::ForkConversation {
            source_turn_digest,
            model_ref,
        } => {
            is_bounded_recovery_token(source_turn_digest)
                && is_bounded_recovery_token(&model_ref.connection_id)
                && is_bounded_recovery_token(&model_ref.model_id)
        }
    };
    valid
        .then_some(())
        .ok_or(HttpRegistryError::ConversationRecoveryInvalidCommand)
}

fn validate_task_integration_review_request(
    request: &HttpTaskIntegrationReviewRequest,
) -> Result<(), HttpRegistryError> {
    let valid = [
        request.request_id.as_str(),
        request.task_id.as_str(),
        request.plan_id.as_str(),
        request.preview_digest.as_str(),
    ]
    .into_iter()
    .all(|value| {
        !value.trim().is_empty()
            && value.len() <= MAX_TASK_INTEGRATION_ID_BYTES
            && safe_persistence_text(value) == value
    }) && request.plan_version > 0;
    valid
        .then_some(())
        .ok_or(HttpRegistryError::InvalidTaskIntegrationReview)
}

fn is_bounded_recovery_token(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_CONVERSATION_RECOVERY_ID_BYTES
        && !value.chars().any(char::is_control)
}

fn is_bounded_queue_token(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_CONVERSATION_QUEUE_ID_BYTES
        && !value.chars().any(char::is_control)
}

fn validate_session_binding(
    session_id: &str,
    binding: &HttpSessionBinding,
) -> Result<(), HttpRegistryError> {
    if binding.session_scope_id.trim().is_empty() {
        return Err(HttpRegistryError::InvalidSessionBinding {
            session_id: session_id.to_owned(),
            message: "durable session scope id must not be empty".to_owned(),
        });
    }
    if binding.session_scope_id.len() > 256 {
        return Err(HttpRegistryError::InvalidSessionBinding {
            session_id: session_id.to_owned(),
            message: "durable session scope id exceeds 256 bytes".to_owned(),
        });
    }
    if binding.session_log_path.trim().is_empty()
        || !Path::new(&binding.session_log_path).is_absolute()
    {
        return Err(HttpRegistryError::InvalidSessionBinding {
            session_id: session_id.to_owned(),
            message: "durable session log path must be absolute".to_owned(),
        });
    }
    Ok(())
}

fn validate_approval_guard(
    run_id: &str,
    call_id: &str,
    pending: &HttpPendingApproval,
    request: &HttpApprovalDecisionRequest,
    now_ms: u64,
) -> Result<(), HttpRegistryError> {
    if pending.approval_request_id != request.approval_request_id {
        return Err(HttpRegistryError::ApprovalRequestChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if pending.tool_call_hash != request.tool_call_hash {
        return Err(HttpRegistryError::ApprovalToolCallChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if pending.policy_version != request.policy_version {
        return Err(HttpRegistryError::ApprovalPolicyChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if pending.expires_at_ms != request.expires_at_ms {
        return Err(HttpRegistryError::ApprovalExpiryChanged {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if request.decision == crate::HttpApprovalDecision::ApproveForSession
        && !pending.session_grant_available
    {
        return Err(HttpRegistryError::ApprovalDecisionUnavailable {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    if now_ms >= pending.expires_at_ms {
        return Err(HttpRegistryError::ApprovalExpired {
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
        });
    }
    Ok(())
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}
