//! Owned blocking boundary for durable HTTP terminal observation and delivery.

use super::*;

#[derive(Clone)]
pub(super) struct HttpRunTerminalIo {
    registry: Arc<HttpSessionRunRegistry>,
    event_bus: Arc<HttpLiveEventBus>,
    session_scope_id: String,
    run_id: String,
    session_log_path: PathBuf,
}

impl HttpRunTerminalIo {
    pub(super) fn new(
        registry: &Arc<HttpSessionRunRegistry>,
        event_bus: &Arc<HttpLiveEventBus>,
        session: &crate::HttpSessionSnapshot,
        run_id: &str,
    ) -> Self {
        Self {
            registry: Arc::clone(registry),
            event_bus: Arc::clone(event_bus),
            session_scope_id: session.durable_session_scope_id.clone(),
            run_id: run_id.to_owned(),
            session_log_path: PathBuf::from(&session.session_log_path),
        }
    }

    // Callers await every worker before releasing run ownership or acknowledging completion.
    // Moving to the blocking pool does not turn a timeout into proof of a durable terminal.
    pub(super) async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, HttpRunDriverError> + Send + 'static,
    ) -> Result<T, HttpRunDriverError> {
        let context = self.clone();
        let started = Instant::now();
        let result = tokio::task::spawn_blocking(move || operation(&context))
            .await
            .map_err(|_| HttpRunDriverError::new("durable terminal worker failed"))
            .and_then(|result| result);
        tracing::debug!(target: "sigil_run_latency", phase = "terminal_io",
            session_scope_id = %self.session_scope_id, run_id = %self.run_id,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            succeeded = result.is_ok(), "durable terminal worker joined");
        result
    }

    pub(super) async fn observe_execution(
        &self,
        control: &Arc<ApplicationRunControl>,
        result: Result<ApplicationRunTerminalStatus>,
    ) -> Result<Option<HttpRunTerminalOutcome>, HttpRunDriverError> {
        let control = Arc::clone(control);
        self.run(move |_| durable_application_execution_terminal(&control, &result))
            .await
    }

    pub(super) async fn observe(
        &self,
        control: &Arc<ApplicationRunControl>,
    ) -> Result<Option<HttpRunTerminalOutcome>, HttpRunDriverError> {
        let control = Arc::clone(control);
        self.run(move |_| durable_application_terminal(&control))
            .await
    }

    pub(super) async fn replay(&self) -> Result<usize, HttpRunDriverError> {
        self.run(|context| {
            replay_pending_http_public_outbox(
                &context.session_log_path,
                &context.session_scope_id,
                &context.run_id,
                &context.event_bus,
                &context.registry,
            )
        })
        .await
    }

    pub(super) async fn record(
        &self,
        outcome: HttpRunTerminalOutcome,
    ) -> Result<HttpRunSnapshot, HttpRunDriverError> {
        self.run(move |context| {
            record_run_terminal_and_reconcile_stream(
                &context.registry,
                &context.event_bus,
                &context.session_scope_id,
                &context.run_id,
                outcome,
            )
        })
        .await
    }

    pub(super) async fn record_natural_if_committed(
        &self,
        control: &Arc<ApplicationRunControl>,
        result: Result<ApplicationRunTerminalStatus>,
    ) -> Result<bool, HttpRunDriverError> {
        let control = Arc::clone(control);
        self.run(move |context| {
            record_natural_terminal_if_committed(
                &control,
                &context.registry,
                &context.event_bus,
                &context.session_scope_id,
                &context.run_id,
                &context.session_log_path,
                &result,
            )
        })
        .await
    }
}
