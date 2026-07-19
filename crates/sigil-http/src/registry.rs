use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sigil_kernel::{SessionRef, safe_persistence_text};
use thiserror::Error as ThisError;

use crate::{
    HttpCommandStoreError, HttpDurableCommandStore,
    command_store::{
        HTTP_DURABLE_COMMAND_PROMPT_OMISSION, HttpStoredCommandClaim, HttpStoredCommandCompletion,
        HttpStoredCommandIdentity, HttpStoredCommandKey,
    },
    driver::{
        HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverStart,
        HttpSessionOpenBindingError,
    },
    dto::{
        HttpApprovalCommandReceipt, HttpApprovalDecisionRecord, HttpApprovalDecisionRequest,
        HttpPendingApproval, HttpRunApprovalMode, HttpRunCancelCommandReceipt,
        HttpRunCancelRequest, HttpRunSnapshot, HttpRunStartCommandReceipt, HttpRunStartRequest,
        HttpRunStatus, HttpRunTerminalOutcome, HttpSessionBinding, HttpSessionCreateRequest,
        HttpSessionOpenRequest, HttpSessionSnapshot, HttpVerificationRerunCommandReceipt,
        HttpVerificationRerunRequest, HttpVerificationView,
    },
    protocol::HttpCommandEnvelope,
};

const DEFAULT_IN_MEMORY_COMMAND_CAPACITY: usize = 4_096;
const MAX_SESSION_OPEN_REFERENCE_BYTES: usize = 512;
const MAX_SESSION_OPEN_ID_BYTES: usize = 512;
const MAX_SESSION_OPEN_LABEL_BYTES: usize = 160;

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
    /// The run did not include an explicit HTTP approval mode.
    #[error("http run start requires an explicit approval mode")]
    MissingApprovalMode,
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
    /// Another foreground run still owns this adapter session.
    #[error("http session {session_id} already has foreground run {run_id}")]
    SessionForegroundRunActive { session_id: String, run_id: String },
    /// A verification rerun already owns the session's foreground mutation lease.
    #[error("http session {session_id} already has an active verification rerun")]
    SessionVerificationActive { session_id: String },
    /// The run cannot accept this operation in its current state.
    #[error("http run {run_id} is not active")]
    RunNotActive { run_id: String },
    /// The approval call id is not currently pending for the run.
    #[error("http approval not pending for run {run_id} call {call_id}")]
    ApprovalNotPending { run_id: String, call_id: String },
    /// The run's approval mode does not use the approval endpoint.
    #[error("http run {run_id} approval mode {approval_mode} does not use approval endpoint")]
    ApprovalModeDoesNotAsk {
        run_id: String,
        approval_mode: HttpRunApprovalMode,
    },
    /// The underlying run driver rejected the registry operation.
    #[error("http driver rejected {operation} for run {run_id}: {message}")]
    DriverRejected {
        operation: &'static str,
        run_id: String,
        message: String,
    },
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
    driver: Arc<dyn HttpRunDriver>,
    command_store: Option<Arc<HttpDurableCommandStore>>,
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
            driver,
            command_store: None,
            in_memory_command_capacity: DEFAULT_IN_MEMORY_COMMAND_CAPACITY,
        }
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
            driver,
            command_store: Some(command_store),
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
            driver,
            command_store: None,
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
        let binding = self.driver.bind_session(&id).map_err(|error| {
            HttpRegistryError::SessionBindingRejected {
                session_id: id.clone(),
                message: error.message,
            }
        })?;
        validate_session_binding(&id, &binding)?;
        let mut state = self.lock_state();
        let session = HttpSessionState {
            id: id.clone(),
            label: request.label,
            run_ids: Vec::new(),
            binding,
            foreground_run_id: None,
            verification_in_progress: false,
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
            .bind_existing_session(&session_ref, &request.session_id)
            .map_err(|error| match error {
                HttpSessionOpenBindingError::NotFound => HttpRegistryError::DurableSessionNotFound,
                HttpSessionOpenBindingError::NotReady => HttpRegistryError::DurableSessionNotReady,
                HttpSessionOpenBindingError::IdentityChanged => {
                    HttpRegistryError::DurableSessionIdentityChanged
                }
                HttpSessionOpenBindingError::Unavailable => {
                    HttpRegistryError::DurableSessionUnavailable
                }
            })?;
        let mut state = self.lock_state();
        state.ensure_accepting_commands()?;
        if let Some(existing) = state
            .sessions
            .values()
            .find(|session| session.binding.session_scope_id == binding.session_scope_id)
        {
            if existing.binding.session_log_path != binding.session_log_path {
                return Err(HttpRegistryError::InvalidSessionBinding {
                    session_id: existing.id.clone(),
                    message: "durable scope resolved to another canonical path".to_owned(),
                });
            }
            return Ok(existing.snapshot());
        }
        let id = state.next_session_id();
        validate_session_binding(&id, &binding)?;
        let session = HttpSessionState {
            id: id.clone(),
            label: request.label,
            run_ids: Vec::new(),
            binding,
            foreground_run_id: None,
            verification_in_progress: false,
        };
        let snapshot = session.snapshot();
        state.sessions.insert(id, session);
        Ok(snapshot)
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

    /// Starts one run inside an existing HTTP adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is unknown, the prompt is empty, approval mode is missing,
    /// or the driver rejects the run.
    pub fn start_run(
        &self,
        session_id: &str,
        request: HttpRunStartRequest,
    ) -> Result<HttpRunSnapshot, HttpRegistryError> {
        if request.prompt.trim().is_empty() {
            return Err(HttpRegistryError::EmptyPrompt);
        }
        let approval_mode = request
            .approval_mode
            .ok_or(HttpRegistryError::MissingApprovalMode)?;
        let prompt = request.prompt;
        let (run_id, session_snapshot, run_snapshot) = {
            let mut state = self.lock_state();
            state.ensure_accepting_commands()?;
            let run_id = state.next_run_id();
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
            if session.verification_in_progress {
                return Err(HttpRegistryError::SessionVerificationActive {
                    session_id: session_id.to_owned(),
                });
            }
            let run = HttpRunState::new(
                run_id.clone(),
                session_id.to_owned(),
                approval_mode,
                prompt_preview(&prompt),
            );
            session.run_ids.push(run_id.clone());
            session.foreground_run_id = Some(run_id.clone());
            let session_snapshot = session.snapshot();
            let run_snapshot = run.snapshot();
            state.runs.insert(run_id.clone(), run);
            (run_id, session_snapshot, run_snapshot)
        };

        let start = HttpRunDriverStart {
            session: session_snapshot,
            run: run_snapshot,
            prompt,
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
        let request = HttpReservedCommand::start(session_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_start(),
            };
        let mut completion = HttpCommandExecutionGuard::new(Arc::clone(&reservation));
        let result =
            self.start_run(session_id, command.payload)
                .map(|run| HttpRunStartCommandReceipt {
                    command_id: command.command_id,
                    client_id: command.client_id,
                    session_id: command.session_id,
                    correlation_id: command.correlation_id,
                    run,
                    replayed: false,
                });
        completion.complete(HttpCommandCompletion::Start(result.clone()))?;
        result
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

    /// Requests cancellation for a running HTTP adapter run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown, terminal, or the driver rejects cancellation.
    pub fn cancel_run(&self, run_id: &str) -> Result<HttpRunSnapshot, HttpRegistryError> {
        self.cancel_run_with_reason(run_id, None)
    }

    fn cancel_run_with_reason(
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
        run.pending_approvals
            .insert(approval.call_id.clone(), approval);
        run.status = HttpRunStatus::WaitingForApproval;
        run.advance_stream_sequence();
        Ok(run.snapshot())
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
        if run.pending_approvals.remove(call_id).is_some() {
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
        let request = HttpReservedCommand::approval(run_id, call_id, &command)?;
        let reservation =
            match self.reserve_command(HttpCommandKey::from_envelope(&command), request)? {
                HttpCommandClaim::Execute(reservation) => reservation,
                HttpCommandClaim::Wait(reservation) => return reservation.wait_for_approval(),
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
            let record = self.submit_approval_decision(run_id, call_id, command.payload)?;
            Ok(HttpApprovalCommandReceipt {
                command_id: command.command_id,
                client_id: command.client_id,
                session_id: command.session_id,
                run_id: run_id.to_owned(),
                call_id: call_id.to_owned(),
                expected_stream_sequence: command.expected_stream_sequence,
                correlation_id: command.correlation_id,
                decision: record,
                replayed: false,
            })
        })();
        completion.complete(HttpCommandCompletion::Approval(result.clone()))?;
        result
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
            run.in_flight_approvals.insert(call_id.to_owned(), pending);
            let record = HttpApprovalDecisionRecord {
                run_id: run_id.to_owned(),
                call_id: call_id.to_owned(),
                decision: request.decision.to_user_decision(),
                reason: request.reason,
            };
            (run.session_id.clone(), record)
        };

        let approval = HttpRunDriverApproval {
            session_id,
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
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
                self.lock_state().mark_run_driver_uncertain(run_id)?;
                return Err(HttpRegistryError::DriverPanicked {
                    operation: "approval",
                    run_id: run_id.to_owned(),
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
            return Ok(record);
        }
        run.in_flight_approvals.remove(call_id);
        run.approval_decisions.push(record.clone());
        if run.pending_approvals.is_empty()
            && run.in_flight_approvals.is_empty()
            && run.status == HttpRunStatus::WaitingForApproval
        {
            run.status = HttpRunStatus::Running;
        }
        run.advance_stream_sequence();
        Ok(record)
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

    fn lock_state(&self) -> MutexGuard<'_, HttpRegistryState> {
        self.state
            .lock()
            .expect("http registry state lock should not be poisoned")
    }
}

struct HttpRegistryState {
    sessions: BTreeMap<String, HttpSessionState>,
    runs: BTreeMap<String, HttpRunState>,
    command_reservations: BTreeMap<HttpCommandKey, Arc<HttpCommandReservation>>,
    next_session_number: u64,
    next_run_number: u64,
    accepting_commands: bool,
    id_namespace: Option<String>,
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
        }
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
    Start,
    Cancel,
    Approval,
    Verification,
}

impl HttpCommandKind {
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Start => b"start",
            Self::Cancel => b"cancel",
            Self::Approval => b"approval",
            Self::Verification => b"verification",
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Cancel => "cancel",
            Self::Approval => "approval",
            Self::Verification => "verification",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpReservedCommand {
    kind: HttpCommandKind,
    fingerprint: [u8; 32],
}

impl HttpReservedCommand {
    fn start(
        path_session_id: &str,
        command: &HttpCommandEnvelope<HttpRunStartRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Start, &[path_session_id], command)
    }

    fn cancel(
        run_id: &str,
        command: &HttpCommandEnvelope<HttpRunCancelRequest>,
    ) -> Result<Self, HttpRegistryError> {
        Self::new(HttpCommandKind::Cancel, &[run_id], command)
    }

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
        let mut hasher = Sha256::new();
        update_command_fingerprint_part(&mut hasher, kind.label());
        for target in targets {
            update_command_fingerprint_part(&mut hasher, target.as_bytes());
        }
        update_command_fingerprint_part(&mut hasher, &encoded);
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
    Approval(Result<HttpApprovalCommandReceipt, HttpRegistryError>),
    Verification(Box<Result<HttpVerificationRerunCommandReceipt, HttpRegistryError>>),
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
            Self::Start(Err(_))
            | Self::Cancel(Err(_))
            | Self::Approval(Err(_))
            | Self::Verification(_)
            | Self::Aborted => HttpStoredCommandCompletion::Aborted,
        }
    }

    fn from_stored(completion: HttpStoredCommandCompletion) -> Self {
        match completion {
            HttpStoredCommandCompletion::Start(receipt) => Self::Start(Ok(receipt)),
            HttpStoredCommandCompletion::Cancel(receipt) => Self::Cancel(Ok(receipt)),
            HttpStoredCommandCompletion::Approval(receipt) => Self::Approval(Ok(receipt)),
            HttpStoredCommandCompletion::Verification(receipt) => {
                Self::Verification(Box::new(Ok(*receipt)))
            }
            HttpStoredCommandCompletion::Reserved | HttpStoredCommandCompletion::Aborted => {
                Self::Aborted
            }
        }
    }
}

fn project_stored_run_snapshot(run: &mut HttpRunSnapshot) {
    run.prompt_preview = HTTP_DURABLE_COMMAND_PROMPT_OMISSION.to_owned();
    for call_id in &mut run.pending_approval_call_ids {
        *call_id = safe_persistence_text(call_id);
    }
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

    fn wait_for_start(&self) -> Result<HttpRunStartCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Start(result) => {
                result.map(HttpRunStartCommandReceipt::replayed)
            }
            HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_cancel(&self) -> Result<HttpRunCancelCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Cancel(result) => {
                result.map(HttpRunCancelCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Approval(_)
            | HttpCommandCompletion::Verification(_)
            | HttpCommandCompletion::Aborted => Err(HttpRegistryError::CommandExecutionAborted),
        }
    }

    fn wait_for_approval(&self) -> Result<HttpApprovalCommandReceipt, HttpRegistryError> {
        match self.wait() {
            HttpCommandCompletion::Approval(result) => {
                result.map(HttpApprovalCommandReceipt::replayed)
            }
            HttpCommandCompletion::Start(_)
            | HttpCommandCompletion::Cancel(_)
            | HttpCommandCompletion::Verification(_)
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
            | HttpCommandCompletion::Approval(_)
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
            run.pending_approvals.clear();
            run.in_flight_approvals.clear();
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
        }
        self.runs
            .get(run_id)
            .map(HttpRunState::snapshot)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })
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
            run.pending_approvals.clear();
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
    verification_in_progress: bool,
}

impl HttpSessionState {
    fn snapshot(&self) -> HttpSessionSnapshot {
        HttpSessionSnapshot {
            id: self.id.clone(),
            label: self.label.clone(),
            run_ids: self.run_ids.clone(),
            durable_session_scope_id: self.binding.session_scope_id.clone(),
            session_log_path: self.binding.session_log_path.clone(),
            foreground_run_id: self.foreground_run_id.clone(),
        }
    }
}

struct HttpRunState {
    id: String,
    session_id: String,
    status: HttpRunStatus,
    previous_status: Option<HttpRunStatus>,
    cancel_operation: Option<Arc<HttpCancelOperation>>,
    approval_mode: HttpRunApprovalMode,
    prompt_preview: String,
    pending_approvals: BTreeMap<String, HttpPendingApproval>,
    in_flight_approvals: BTreeMap<String, HttpPendingApproval>,
    approval_decisions: Vec<HttpApprovalDecisionRecord>,
    stream_sequence: u64,
}

impl HttpRunState {
    fn new(
        id: String,
        session_id: String,
        approval_mode: HttpRunApprovalMode,
        prompt_preview: String,
    ) -> Self {
        Self {
            id,
            session_id,
            status: HttpRunStatus::Starting,
            previous_status: None,
            cancel_operation: None,
            approval_mode,
            prompt_preview,
            pending_approvals: BTreeMap::new(),
            in_flight_approvals: BTreeMap::new(),
            approval_decisions: Vec::new(),
            stream_sequence: 0,
        }
    }

    fn snapshot(&self) -> HttpRunSnapshot {
        HttpRunSnapshot {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            status: self.status,
            approval_mode: self.approval_mode,
            prompt_preview: self.prompt_preview.clone(),
            pending_approval_call_ids: self.pending_approvals.keys().cloned().collect(),
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
        if self.approval_mode != HttpRunApprovalMode::Ask {
            return Some(HttpRegistryError::ApprovalModeDoesNotAsk {
                run_id: run_id.to_owned(),
                approval_mode: self.approval_mode,
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

    fn restore_in_flight_approval(&mut self, call_id: &str) {
        if self.status.is_terminal() {
            return;
        }
        if let Some(approval) = self.in_flight_approvals.remove(call_id) {
            self.pending_approvals.insert(call_id.to_owned(), approval);
        }
    }

    fn advance_stream_sequence(&mut self) {
        self.stream_sequence = self.stream_sequence.saturating_add(1);
    }
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

fn command_store_registry_error(error: HttpCommandStoreError) -> HttpRegistryError {
    match error {
        HttpCommandStoreError::Saturated => HttpRegistryError::CommandRegistrySaturated,
        error => HttpRegistryError::CommandIdentityPersistenceFailed {
            message: error.to_string(),
        },
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
