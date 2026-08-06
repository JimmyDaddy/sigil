use super::*;

const MAX_TERMINAL_MATCH_VALUE_BYTES: usize = 4 * 1024;
const TERMINAL_MATCH_WINDOW_BYTES: usize = 64 * 1024;
const TERMINAL_REGEX_SIZE_LIMIT_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(super) struct TerminalLifecycleOwner {
    state: Arc<StdMutex<TerminalLifecycleState>>,
    tx: watch::Sender<TerminalLifecycleEvent>,
    task_id: TerminalTaskId,
    /// RFC-0062 14.1: task-scoped scratch lease released when the task reaches a terminal
    /// state, so the session namespace becomes TTL-eligible even when the model never
    /// waits/reads the settled task again.
    scratch_leases: Option<Arc<crate::scratch_namespace::ScratchTaskLeaseRegistry>>,
}

struct TerminalLifecycleState {
    event: TerminalLifecycleEvent,
    readiness_matcher: Option<TerminalOutputMatcher>,
    output_window: Vec<u8>,
}

enum TerminalOutputMatcher {
    Contains(Vec<u8>),
    Regex(Regex),
}

impl TerminalOutputMatcher {
    fn matches(&self, output: &[u8]) -> bool {
        match self {
            Self::Contains(value) => {
                !value.is_empty() && output.windows(value.len()).any(|window| window == value)
            }
            Self::Regex(regex) => regex.is_match(&String::from_utf8_lossy(output)),
        }
    }
}

impl TerminalLifecycleOwner {
    pub(super) fn new(
        task_id: TerminalTaskId,
        execution_backend: Option<TerminalExecutionBackendKind>,
        sandbox_profile: Option<ExecutionSandboxProfile>,
        readiness: &TerminalReadinessCondition,
    ) -> Result<Self> {
        let readiness_matcher = readiness_matcher(readiness)?;
        let readiness_status = match readiness.kind() {
            TerminalReadinessKind::None => TerminalReadinessStatus::None,
            kind => TerminalReadinessStatus::Waiting { kind },
        };
        let event = TerminalLifecycleEvent {
            task_id: task_id.clone(),
            execution_backend,
            sandbox_profile,
            generation: 0,
            status: TerminalTaskStatus::Starting,
            readiness: readiness_status,
            total_output_bytes: 0,
            emitted_at_ms: current_epoch_ms(),
        };
        let (tx, _rx) = watch::channel(event.clone());
        Ok(Self {
            state: Arc::new(StdMutex::new(TerminalLifecycleState {
                event,
                readiness_matcher,
                output_window: Vec::new(),
            })),
            tx,
            task_id,
            scratch_leases: None,
        })
    }

    /// Binds the shared task-scoped scratch lease registry to this task lifecycle.
    pub(super) fn with_scratch_leases(
        mut self,
        scratch_leases: Option<Arc<crate::scratch_namespace::ScratchTaskLeaseRegistry>>,
    ) -> Self {
        self.scratch_leases = scratch_leases;
        self
    }

    fn release_scratch_lease(&self) {
        if let Some(leases) = &self.scratch_leases {
            leases.release(self.task_id.as_str());
        }
    }

    pub(super) fn mark_running(&self) {
        self.update(|state| {
            state.event.status = TerminalTaskStatus::Running;
        });
    }

    pub(super) fn observe_output(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.update(|state| {
            state.event.total_output_bytes = state
                .event
                .total_output_bytes
                .saturating_add(bytes.len() as u64);
            push_bounded_output(&mut state.output_window, bytes);
            if state.event.readiness.is_waiting()
                && state
                    .readiness_matcher
                    .as_ref()
                    .is_some_and(|matcher| matcher.matches(&state.output_window))
            {
                let kind = match state.event.readiness {
                    TerminalReadinessStatus::Waiting { kind } => kind,
                    _ => return,
                };
                state.event.readiness = TerminalReadinessStatus::Ready {
                    kind,
                    ready_at_ms: current_epoch_ms(),
                };
                state.readiness_matcher = None;
            }
        });
    }

    pub(super) fn mark_terminal(&self, status: TerminalTaskStatus, total_output_bytes: u64) {
        self.update(|state| {
            apply_terminal_transition(&mut state.event, status, total_output_bytes);
            if !state.event.readiness.is_waiting() {
                state.readiness_matcher = None;
            }
        });
        self.release_scratch_lease();
    }

    pub(super) fn prepare_terminal(
        &self,
        status: TerminalTaskStatus,
        total_output_bytes: u64,
    ) -> TerminalLifecycleEvent {
        let state = self
            .state
            .lock()
            .expect("terminal lifecycle state lock poisoned");
        let mut event = state.event.clone();
        apply_terminal_transition(&mut event, status, total_output_bytes);
        let changed = event != state.event;
        drop(state);
        self.release_scratch_lease();
        if changed {
            event.generation = event.generation.saturating_add(1);
            event.emitted_at_ms = current_epoch_ms();
        }
        event
    }

    pub(super) fn commit_prepared_terminal(&self, event: TerminalLifecycleEvent) -> bool {
        let event = {
            let mut state = self
                .state
                .lock()
                .expect("terminal lifecycle state lock poisoned");
            if event.task_id != state.event.task_id
                || event.generation != state.event.generation.saturating_add(1)
            {
                return false;
            }
            state.event = event;
            if !state.event.readiness.is_waiting() {
                state.readiness_matcher = None;
            }
            state.event.clone()
        };
        self.tx.send_replace(event);
        true
    }

    pub(super) fn mark_readiness_timed_out(&self) {
        self.update(|state| {
            if let TerminalReadinessStatus::Waiting { kind } = state.event.readiness {
                state.event.readiness = TerminalReadinessStatus::TimedOut { kind };
                state.readiness_matcher = None;
            }
        });
    }

    pub(super) fn snapshot(&self) -> TerminalLifecycleEvent {
        self.state
            .lock()
            .expect("terminal lifecycle state lock poisoned")
            .event
            .clone()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<TerminalLifecycleEvent> {
        self.tx.subscribe()
    }

    pub(super) async fn wait(
        &self,
        after_generation: u64,
        condition: &TerminalWaitCondition,
        max_wait: Duration,
    ) -> Result<TerminalWaitOutcome> {
        let matcher = wait_matcher(condition)?;
        let mut rx = self.subscribe();
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            if self.condition_matches(after_generation, condition, matcher.as_ref())? {
                return Ok(TerminalWaitOutcome::ConditionMet);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(TerminalWaitOutcome::Timeout);
            }
            match timeout(remaining, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Ok(TerminalWaitOutcome::OwnerShutdown),
                Err(_) => return Ok(TerminalWaitOutcome::Timeout),
            }
        }
    }

    fn condition_matches(
        &self,
        after_generation: u64,
        condition: &TerminalWaitCondition,
        matcher: Option<&TerminalOutputMatcher>,
    ) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("terminal lifecycle state lock poisoned"))?;
        Ok(match condition {
            TerminalWaitCondition::StatusChange => state.event.generation > after_generation,
            TerminalWaitCondition::Exit => state.event.status.is_terminal(),
            TerminalWaitCondition::OutputContains(_) | TerminalWaitCondition::OutputRegex(_) => {
                matcher.is_some_and(|matcher| matcher.matches(&state.output_window))
            }
            TerminalWaitCondition::Readiness => {
                !matches!(
                    state.event.readiness,
                    TerminalReadinessStatus::Waiting { .. }
                ) || state.event.status.is_terminal()
            }
        })
    }

    fn update(&self, update: impl FnOnce(&mut TerminalLifecycleState)) {
        let event = {
            let mut state = self
                .state
                .lock()
                .expect("terminal lifecycle state lock poisoned");
            let previous = state.event.clone();
            update(&mut state);
            if state.event == previous {
                return;
            }
            state.event.generation = state.event.generation.saturating_add(1);
            state.event.emitted_at_ms = current_epoch_ms();
            state.event.clone()
        };
        self.tx.send_replace(event);
    }
}

fn apply_terminal_transition(
    event: &mut TerminalLifecycleEvent,
    status: TerminalTaskStatus,
    total_output_bytes: u64,
) {
    event.status = status;
    event.total_output_bytes = event.total_output_bytes.max(total_output_bytes);
    if let TerminalReadinessStatus::Waiting { kind } = event.readiness {
        event.readiness = TerminalReadinessStatus::Failed {
            kind,
            reason: "terminal task exited before readiness was observed".to_owned(),
        };
    }
}

fn readiness_matcher(
    readiness: &TerminalReadinessCondition,
) -> Result<Option<TerminalOutputMatcher>> {
    match readiness {
        TerminalReadinessCondition::None => Ok(None),
        TerminalReadinessCondition::OutputContains { value, .. } => {
            validate_match_value("terminal readiness output_contains", value)?;
            Ok(Some(TerminalOutputMatcher::Contains(
                value.as_bytes().to_vec(),
            )))
        }
        TerminalReadinessCondition::OutputRegex { value, .. } => {
            validate_match_value("terminal readiness output_regex", value)?;
            Ok(Some(TerminalOutputMatcher::Regex(compile_regex(value)?)))
        }
    }
}

fn wait_matcher(condition: &TerminalWaitCondition) -> Result<Option<TerminalOutputMatcher>> {
    match condition {
        TerminalWaitCondition::OutputContains(value) => {
            validate_match_value("terminal_wait output_contains", value)?;
            Ok(Some(TerminalOutputMatcher::Contains(
                value.as_bytes().to_vec(),
            )))
        }
        TerminalWaitCondition::OutputRegex(value) => {
            validate_match_value("terminal_wait output_regex", value)?;
            Ok(Some(TerminalOutputMatcher::Regex(compile_regex(value)?)))
        }
        TerminalWaitCondition::StatusChange
        | TerminalWaitCondition::Exit
        | TerminalWaitCondition::Readiness => Ok(None),
    }
}

fn validate_match_value(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} value cannot be empty");
    }
    if value.len() > MAX_TERMINAL_MATCH_VALUE_BYTES {
        bail!("{label} value exceeds {MAX_TERMINAL_MATCH_VALUE_BYTES} bytes");
    }
    Ok(())
}

fn compile_regex(value: &str) -> Result<Regex> {
    RegexBuilder::new(value)
        .size_limit(TERMINAL_REGEX_SIZE_LIMIT_BYTES)
        .dfa_size_limit(TERMINAL_REGEX_SIZE_LIMIT_BYTES)
        .build()
        .map_err(|error| anyhow!("invalid bounded terminal output regex: {error}"))
}

fn push_bounded_output(output: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= TERMINAL_MATCH_WINDOW_BYTES {
        output.clear();
        output.extend_from_slice(&bytes[bytes.len() - TERMINAL_MATCH_WINDOW_BYTES..]);
        return;
    }
    let overflow = output
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(TERMINAL_MATCH_WINDOW_BYTES);
    if overflow > 0 {
        output.drain(..overflow);
    }
    output.extend_from_slice(bytes);
}
