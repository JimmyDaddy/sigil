use std::time::Duration;

use sigil_kernel::SessionRef;
use thiserror::Error as ThisError;

use crate::dto::{
    HttpAgentActivityView, HttpApplicationAgentBinding, HttpApplicationSkillBinding,
    HttpApprovalDecisionRecord, HttpConversationDisplayPage, HttpConversationQueueCommandRequest,
    HttpConversationQueueGeneration, HttpConversationQueueView, HttpDurableSessionFrontier,
    HttpForegroundRunOwner, HttpPermissionMode, HttpReasoningEffort, HttpRunContextView,
    HttpRunSnapshot, HttpSessionBinding, HttpSessionSnapshot, HttpSessionTranscriptPage,
    HttpVerificationRerunRequest, HttpVerificationView,
};

/// Start context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverStart {
    /// Session snapshot at the moment the run was registered.
    pub session: HttpSessionSnapshot,
    /// Run snapshot in `starting` state.
    pub run: HttpRunSnapshot,
    /// Full prompt body. The preview is carried separately on the run snapshot.
    pub prompt: String,
    /// Optional model selected from the exact run-context capability set.
    pub model_name: Option<String>,
    /// Opaque model-selection binding supplied with an explicit selection.
    pub model_selection_binding: Option<String>,
    /// Opaque exact provider/model effort binding.
    pub reasoning_effort_binding: Option<String>,
    /// Exact inline-skill binding selected from the current run context.
    pub skill_binding: Option<HttpApplicationSkillBinding>,
    /// Exact supervised-agent binding selected from the current run context.
    pub agent_binding: Option<HttpApplicationAgentBinding>,
}

/// Cancel context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverCancel {
    /// Owning session id.
    pub session_id: String,
    /// Run id being canceled.
    pub run_id: String,
    /// Optional user-facing reason persisted by the runtime cancellation control plane.
    pub reason: Option<String>,
}

/// Approval context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverApproval {
    /// Owning session id.
    pub session_id: String,
    /// Run id receiving the decision.
    pub run_id: String,
    /// Tool call id receiving the decision.
    pub call_id: String,
    /// Decision record routed to the driver.
    pub decision: HttpApprovalDecisionRecord,
}

/// Secret-free admission selected by the application owner for one queued foreground run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpQueuedRunAdmission {
    /// Durable queue item selected under `generation`.
    pub entry_id: String,
    /// Opaque queue generation used by the promotion CAS.
    pub generation: HttpConversationQueueGeneration,
    /// Logical run id durably bound by queue promotion.
    pub dispatch_run_id: String,
    /// Safe bounded prompt preview used by process-local run status.
    pub prompt_preview: String,
    /// Effective permission mode resolved by the application owner.
    pub permission_mode: HttpPermissionMode,
    /// Exact queued reasoning effort when present.
    pub reasoning_effort: Option<HttpReasoningEffort>,
}

/// Start context for a queue-owned foreground run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpQueuedRunDriverStart {
    /// Session snapshot after the registry acquired foreground ownership.
    pub session: HttpSessionSnapshot,
    /// Registered process-local run snapshot.
    pub run: HttpRunSnapshot,
    /// Durable queue admission that must be revalidated before promotion.
    pub admission: HttpQueuedRunAdmission,
}

/// Idempotent identity and exact payload for one queue mutation.
///
/// The application owner uses this identity to derive stable durable entry ids. This prevents a
/// retry after a process interruption from appending a second logical queue item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConversationQueueDriverCommand {
    /// Stable command identity within the client/session scope.
    pub command_id: String,
    /// Stable application-client identity owning the command.
    pub client_id: String,
    /// Exact queue generation and requested mutation.
    pub request: HttpConversationQueueCommandRequest,
}

/// Driver interface used by the HTTP registry.
///
/// The registry owns IDs and routing state. The driver owns actual agent execution,
/// cancellation, and approval delivery so this crate does not duplicate the agent loop.
pub trait HttpRunDriver: Send + Sync {
    /// Whether terminal registry state must retain an admission barrier until the driver reports
    /// that its process-local supervisor and runtime session lease have both been released.
    fn requires_run_release_barrier(&self) -> bool {
        false
    }

    /// Creates or resolves the durable session binding for one adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime cannot establish a durable V2 session scope and path.
    fn bind_session(
        &self,
        session_id: &str,
        model_name: Option<&str>,
    ) -> Result<HttpSessionBinding, HttpRunDriverError>;

    /// Resolves an existing durable session after the registry validates its wire identity.
    ///
    /// Synthetic drivers that do not model historical sessions reject this operation by default.
    ///
    /// # Errors
    ///
    /// Returns a bounded error direction when current workspace truth cannot authorize the reopen.
    fn bind_existing_session(
        &self,
        _session_ref: &SessionRef,
        _expected_session_id: &str,
    ) -> Result<HttpSessionBinding, HttpSessionOpenBindingError> {
        Err(HttpSessionOpenBindingError::Unavailable)
    }

    /// Purges process-local material owned by one durable session after its source was deleted.
    ///
    /// The durable deletion path calls this only after the catalog mutation succeeds. The default
    /// is a no-op for drivers that retain no session-scoped secrets or caches.
    fn purge_session_local_state(&self, _durable_session_scope_id: &str) {}

    /// Starts execution for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot accept the run.
    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError>;

    /// Requests cancellation for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot route the cancellation.
    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError>;

    /// Routes a user approval decision to a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot route the approval decision.
    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError>;

    /// Projects verification truth for one bound durable session.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable stream cannot be read safely.
    fn verification_view(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<Option<HttpVerificationView>, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "verification projection is unavailable",
        ))
    }

    /// Projects one bounded chronological transcript page for a bound durable session.
    ///
    /// # Errors
    ///
    /// Returns an error when durable scope validation or safe projection fails.
    fn transcript_page(
        &self,
        _session: &HttpSessionSnapshot,
        _before: Option<u64>,
        _limit: usize,
    ) -> Result<HttpSessionTranscriptPage, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "transcript projection is unavailable",
        ))
    }

    /// Projects one canonical durable conversation page for a bound session.
    ///
    /// # Errors
    ///
    /// Returns a typed stale-cursor rejection or a generic unavailable result. The projection must
    /// not expose the raw durable scope or session path.
    fn conversation_display_page(
        &self,
        _session: &HttpSessionSnapshot,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<HttpConversationDisplayPage, HttpConversationDisplayDriverError> {
        Err(HttpConversationDisplayDriverError::Unavailable)
    }

    /// Reads the current scope-checked durable frontier without mutating session truth.
    ///
    /// # Errors
    ///
    /// Returns an error when durable scope validation or projection fails.
    fn session_frontier(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<HttpDurableSessionFrontier, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "session frontier projection is unavailable",
        ))
    }

    /// Projects typed model, permission-mode, and context usage facts for one bound session.
    ///
    /// # Errors
    ///
    /// Returns an error when durable scope validation or projection fails.
    fn run_context_view(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<HttpRunContextView, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "run-context projection is unavailable",
        ))
    }

    /// Projects safe, bounded child-agent lifecycle and result-handoff state.
    ///
    /// # Errors
    ///
    /// Returns an error when durable scope validation or projection fails.
    fn agent_activity_view(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<HttpAgentActivityView, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "agent activity projection is unavailable",
        ))
    }

    /// Projects the current durable follow-up queue with process-local material availability.
    ///
    /// # Errors
    ///
    /// Returns a typed rejection when the durable projection or application owner is unavailable.
    fn conversation_queue_view(
        &self,
        _session: &HttpSessionSnapshot,
        _foreground_owner: Option<&HttpForegroundRunOwner>,
    ) -> Result<HttpConversationQueueView, HttpConversationQueueDriverError> {
        Err(HttpConversationQueueDriverError::Unavailable)
    }

    /// Applies one exact queue CAS mutation and returns the resulting bounded view.
    ///
    /// The implementation owns secret-safe projection and any process-local exact prompt cache.
    /// It must not persist the raw prompt from enqueue or edit actions.
    ///
    /// # Errors
    ///
    /// Returns a typed rejection for stale generations, terminal entries, owner loss, permission,
    /// conflict, unsupported actions, or unavailable durable truth.
    fn mutate_conversation_queue(
        &self,
        _session: &HttpSessionSnapshot,
        _foreground_owner: Option<&HttpForegroundRunOwner>,
        _command: &HttpConversationQueueDriverCommand,
    ) -> Result<HttpConversationQueueView, HttpConversationQueueDriverError> {
        Err(HttpConversationQueueDriverError::Unavailable)
    }

    /// Selects the next exact dispatchable queue item without changing durable state.
    ///
    /// # Errors
    ///
    /// Returns a typed rejection when material or durable queue truth cannot be proven.
    fn next_queued_run_admission(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<Option<HttpQueuedRunAdmission>, HttpConversationQueueDriverError> {
        Ok(None)
    }

    /// Starts one internally registered queue-owned foreground run.
    ///
    /// This is deliberately separate from the public run-start route. The driver must prepare and
    /// freeze the exact request, commit queue promotion by writer-lock CAS, and only then execute.
    ///
    /// # Errors
    ///
    /// Returns an error when preparation, promotion, or owned supervisor startup fails.
    fn start_queued_run(&self, _start: HttpQueuedRunDriverStart) -> Result<(), HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "queued run execution is unavailable",
        ))
    }

    /// Waits for one run supervisor to release its process-local session lease after terminal.
    ///
    /// Synthetic drivers own no asynchronous supervisor by default.
    ///
    /// # Errors
    ///
    /// Returns an error when cleanup does not complete before `timeout`.
    fn wait_for_run_release(
        &self,
        _run_id: &str,
        _timeout: Duration,
    ) -> Result<(), HttpRunDriverError> {
        Ok(())
    }

    /// Executes one exact stale-safe verification rerun.
    ///
    /// # Errors
    ///
    /// Returns an error when the binding drifted, the session is busy, or the check fails to
    /// produce a durable terminal projection.
    fn rerun_verification(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpVerificationRerunRequest,
    ) -> Result<HttpVerificationView, HttpRunDriverError> {
        Err(HttpRunDriverError::new("verification rerun is unavailable"))
    }

    /// Waits until every driver-owned run supervisor has completed cleanup.
    ///
    /// Synthetic drivers own no background execution by default. Production drivers override this
    /// hook so a successful listener shutdown cannot leave an unowned run task behind.
    ///
    /// # Errors
    ///
    /// Returns an error when owned work does not drain before `timeout`.
    fn wait_for_idle(&self, _timeout: Duration) -> Result<(), HttpRunDriverError> {
        Ok(())
    }
}

/// Bounded, path-free failure direction returned while reopening an existing durable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpSessionOpenBindingError {
    /// The requested direct-child source is absent from current workspace truth.
    #[error("durable session was not found")]
    NotFound,
    /// The source exists but is not a ready, supported V2 stream.
    #[error("durable session is not ready")]
    NotReady,
    /// The source identity no longer matches the catalog candidate selected by the client.
    #[error("durable session identity changed")]
    IdentityChanged,
    /// Current bounded lifecycle or durable stream validation could not complete.
    #[error("durable session is unavailable")]
    Unavailable,
}

/// Error returned by an HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
#[error("{message}")]
pub struct HttpRunDriverError {
    /// Driver-provided error message.
    pub message: String,
}

/// Typed rejection surface for the canonical display query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpConversationDisplayDriverError {
    /// The opaque cursor is malformed or belongs to another request scope.
    #[error("conversation display cursor is invalid")]
    InvalidCursor,
    /// The opaque cursor no longer binds the fixed durable history frontier.
    #[error("conversation display cursor is stale")]
    StaleCursor,
    /// Durable projection could not be proven safely.
    #[error("conversation display projection is unavailable")]
    Unavailable,
}

/// Typed application-owner rejection for queue projection and mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpConversationQueueDriverError {
    /// The client generation no longer matches durable queue truth.
    #[error("conversation queue generation is stale")]
    StaleGeneration,
    /// The addressed queue entry already reached a terminal state.
    #[error("conversation queue entry is terminal")]
    Terminal,
    /// The foreground owner binding changed or disappeared.
    #[error("conversation queue foreground owner changed")]
    OwnerLost,
    /// Current policy cannot authorize this queue operation.
    #[error("conversation queue operation requires permission")]
    Permission,
    /// The queue mutation conflicts with current durable state.
    #[error("conversation queue operation conflicts with durable state")]
    Conflict,
    /// Exact prompt material was intentionally lost and must be entered again.
    #[error("conversation queue prompt requires reentry")]
    RequiresReentry,
    /// The requested queue action is not supported by this application owner.
    #[error("conversation queue operation is unsupported")]
    Unsupported,
    /// Durable queue truth or application ownership could not be proven.
    #[error("conversation queue is unavailable")]
    Unavailable,
}

impl HttpRunDriverError {
    /// Creates a driver error with context.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
