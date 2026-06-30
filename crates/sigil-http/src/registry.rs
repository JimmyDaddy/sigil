use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use thiserror::Error as ThisError;

use crate::{
    driver::{HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverStart},
    dto::{
        HttpApprovalCommandReceipt, HttpApprovalDecisionRecord, HttpApprovalDecisionRequest,
        HttpPendingApproval, HttpRunApprovalMode, HttpRunSnapshot, HttpRunStartCommandReceipt,
        HttpRunStartRequest, HttpRunStatus, HttpSessionCreateRequest, HttpSessionSnapshot,
    },
    protocol::HttpCommandEnvelope,
};

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
}

/// In-memory registry for HTTP adapter sessions, runs, cancellations, and approvals.
pub struct HttpSessionRunRegistry {
    state: Mutex<HttpRegistryState>,
    driver: Arc<dyn HttpRunDriver>,
}

impl HttpSessionRunRegistry {
    /// Creates a registry that delegates execution to `driver`.
    #[must_use]
    pub fn new(driver: Arc<dyn HttpRunDriver>) -> Self {
        Self {
            state: Mutex::new(HttpRegistryState::default()),
            driver,
        }
    }

    /// Creates one HTTP adapter session.
    pub fn create_session(&self, request: HttpSessionCreateRequest) -> HttpSessionSnapshot {
        let mut state = self.lock_state();
        let id = state.next_session_id();
        let session = HttpSessionState {
            id: id.clone(),
            label: request.label,
            run_ids: Vec::new(),
        };
        let snapshot = session.snapshot();
        state.sessions.insert(id, session);
        snapshot
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
            let run_id = state.next_run_id();
            let session = state.sessions.get_mut(session_id).ok_or_else(|| {
                HttpRegistryError::SessionNotFound {
                    session_id: session_id.to_owned(),
                }
            })?;
            let run = HttpRunState::new(
                run_id.clone(),
                session_id.to_owned(),
                approval_mode,
                prompt_preview(&prompt),
            );
            session.run_ids.push(run_id.clone());
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
        if let Err(error) = self.driver.start_run(start) {
            let mut state = self.lock_state();
            if let Some(run) = state.runs.get_mut(&run_id) {
                run.status = HttpRunStatus::Failed;
            }
            return Err(HttpRegistryError::DriverRejected {
                operation: "start",
                run_id,
                message: error.message,
            });
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
                command_session_id: command.session_id,
                path_session_id: session_id.to_owned(),
            });
        }
        let key = HttpCommandKey {
            session_id: command.session_id.clone(),
            client_id: command.client_id.clone(),
            command_id: command.command_id.clone(),
        };
        {
            let state = self.lock_state();
            if let Some(receipt) = state.run_start_command_receipts.get(&key) {
                return Ok(receipt.clone().replayed());
            }
        }

        let run = self.start_run(session_id, command.payload.clone())?;
        let receipt = HttpRunStartCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            correlation_id: command.correlation_id,
            run,
            replayed: false,
        };
        let mut state = self.lock_state();
        state
            .run_start_command_receipts
            .insert(key, receipt.clone());
        Ok(receipt)
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

    /// Requests cancellation for a running HTTP adapter run.
    ///
    /// # Errors
    ///
    /// Returns an error when the run is unknown, terminal, or the driver rejects cancellation.
    pub fn cancel_run(&self, run_id: &str) -> Result<HttpRunSnapshot, HttpRegistryError> {
        let cancel = {
            let mut state = self.lock_state();
            let run = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| HttpRegistryError::RunNotFound {
                    run_id: run_id.to_owned(),
                })?;
            if run.status.is_terminal() {
                return Err(HttpRegistryError::RunNotActive {
                    run_id: run_id.to_owned(),
                });
            }
            if run.status == HttpRunStatus::CancelRequested {
                return Ok(run.snapshot());
            }
            run.previous_status = Some(run.status);
            run.status = HttpRunStatus::CancelRequested;
            HttpRunDriverCancel {
                session_id: run.session_id.clone(),
                run_id: run.id.clone(),
            }
        };

        if let Err(error) = self.driver.cancel_run(cancel) {
            let mut state = self.lock_state();
            if let Some(run) = state.runs.get_mut(run_id) {
                run.restore_previous_status();
            }
            return Err(HttpRegistryError::DriverRejected {
                operation: "cancel",
                run_id: run_id.to_owned(),
                message: error.message,
            });
        }

        self.get_run(run_id)
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
        let key = HttpCommandKey {
            session_id: command.session_id.clone(),
            client_id: command.client_id.clone(),
            command_id: command.command_id.clone(),
        };
        {
            let state = self.lock_state();
            if let Some(receipt) = state.command_receipts.get(&key) {
                return Ok(receipt.clone().replayed());
            }
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

        let record = self.submit_approval_decision(run_id, call_id, command.payload.clone())?;
        let receipt = HttpApprovalCommandReceipt {
            command_id: command.command_id,
            client_id: command.client_id,
            session_id: command.session_id,
            run_id: run_id.to_owned(),
            call_id: call_id.to_owned(),
            expected_stream_sequence: command.expected_stream_sequence,
            correlation_id: command.correlation_id,
            decision: record,
            replayed: false,
        };
        let mut state = self.lock_state();
        state.command_receipts.insert(key, receipt.clone());
        Ok(receipt)
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
        if let Err(error) = self.driver.submit_approval(approval) {
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

        let mut state = self.lock_state();
        let run = state
            .runs
            .get_mut(run_id)
            .ok_or_else(|| HttpRegistryError::RunNotFound {
                run_id: run_id.to_owned(),
            })?;
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

    fn lock_state(&self) -> MutexGuard<'_, HttpRegistryState> {
        self.state
            .lock()
            .expect("http registry state lock should not be poisoned")
    }
}

#[derive(Default)]
struct HttpRegistryState {
    sessions: BTreeMap<String, HttpSessionState>,
    runs: BTreeMap<String, HttpRunState>,
    run_start_command_receipts: BTreeMap<HttpCommandKey, HttpRunStartCommandReceipt>,
    command_receipts: BTreeMap<HttpCommandKey, HttpApprovalCommandReceipt>,
    next_session_number: u64,
    next_run_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpCommandKey {
    session_id: String,
    client_id: String,
    command_id: String,
}

impl HttpRegistryState {
    fn next_session_id(&mut self) -> String {
        self.next_session_number += 1;
        format!("http-session-{}", self.next_session_number)
    }

    fn next_run_id(&mut self) -> String {
        self.next_run_number += 1;
        format!("http-run-{}", self.next_run_number)
    }
}

struct HttpSessionState {
    id: String,
    label: Option<String>,
    run_ids: Vec<String>,
}

impl HttpSessionState {
    fn snapshot(&self) -> HttpSessionSnapshot {
        HttpSessionSnapshot {
            id: self.id.clone(),
            label: self.label.clone(),
            run_ids: self.run_ids.clone(),
        }
    }
}

struct HttpRunState {
    id: String,
    session_id: String,
    status: HttpRunStatus,
    previous_status: Option<HttpRunStatus>,
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

    fn restore_previous_status(&mut self) {
        if let Some(previous) = self.previous_status.take() {
            self.status = previous;
        }
    }

    fn restore_in_flight_approval(&mut self, call_id: &str) {
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
