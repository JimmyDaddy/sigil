use std::{path::Path, sync::Arc, time::Duration};

use sigil_kernel::SessionRef;
use thiserror::Error as ThisError;

use crate::dto::{
    HttpAgentActivityView, HttpApplicationAgentBinding, HttpApplicationSkillBinding,
    HttpApprovalDecisionRecord, HttpCheckpointRestoreReceipt, HttpCheckpointRestoreRequest,
    HttpCheckpointRestoreReview, HttpCompactionReceipt, HttpCompactionReview,
    HttpConversationDisplayPage, HttpConversationForkReceipt, HttpConversationQueueCommandRequest,
    HttpConversationQueueGeneration, HttpConversationQueueView,
    HttpConversationRecoveryCommandAction, HttpConversationRecoveryView,
    HttpDurableSessionFrontier, HttpForegroundRunOwner, HttpIntentDropExecution,
    HttpIntentDropPreview, HttpIntentDropRequest, HttpIntentStackView, HttpPermissionMode,
    HttpPlanDecisionCommandReceipt, HttpPlanDecisionRequest, HttpProviderModelRef,
    HttpReasoningEffort, HttpRunContextView, HttpRunSnapshot, HttpRunStartRequest,
    HttpSessionBinding, HttpSessionRouteRecoveryView, HttpSessionSnapshot,
    HttpSessionTranscriptPage, HttpTaskContinuationRequest, HttpTaskIntegrationAcceptanceView,
    HttpTaskIntegrationReviewRequest, HttpTaskIntegrationReviewView, HttpTaskPauseRequest,
    HttpTerminalLifecycleView, HttpToolArtifactPage, HttpToolArtifactReadRequest,
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
    pub model_ref: Option<HttpProviderModelRef>,
    /// Opaque model-selection binding supplied with an explicit selection.
    pub model_selection_binding: Option<String>,
    /// Exact route-recovery binding explicitly confirmed by the client.
    pub route_recovery_binding: Option<String>,
    /// Opaque exact provider/model effort binding.
    pub reasoning_effort_binding: Option<String>,
    /// Exact inline-skill binding selected from the current run context.
    pub skill_binding: Option<HttpApplicationSkillBinding>,
    /// Exact supervised-agent binding selected from the current run context.
    pub agent_binding: Option<HttpApplicationAgentBinding>,
    /// Exact durable Task continuation replacing a new conversation turn.
    pub task_continuation: Option<HttpTaskContinuationRequest>,
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

/// Exact Task pause context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverTaskPause {
    /// Owning session id.
    pub session_id: String,
    /// Run id owning the exact Task cancellation scope.
    pub run_id: String,
    /// Exact rendered Task, plan, and request binding.
    pub request: HttpTaskPauseRequest,
}

/// Exact persistent-terminal cancellation routed to the original process owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverTerminalTaskCancel {
    pub session_id: String,
    pub run_id: String,
    pub task_id: String,
    pub expected_generation: u64,
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
    /// Exact kernel-owned approval request receiving the decision.
    pub approval_request_id: String,
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

/// Lifetime guard proving that one exact durable session is safe for a catalog mutation.
#[derive(Debug)]
pub struct HttpDurableSessionAttachmentGuard {
    _attachment: Option<
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    >,
}

impl HttpDurableSessionAttachmentGuard {
    #[must_use]
    pub fn unmanaged() -> Self {
        Self { _attachment: None }
    }

    #[must_use]
    pub fn attached(
        attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
    ) -> Self {
        Self {
            _attachment: Some(attachment),
        }
    }
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

/// Idempotent identity and exact payload for one durable conversation recovery command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConversationRecoveryDriverCommand {
    pub command_id: String,
    pub client_id: String,
    pub action: HttpConversationRecoveryCommandAction,
}

/// Driver-owned durable outcome of one recovery mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConversationRecoveryDriverOutput {
    pub compaction: Option<HttpCompactionReceipt>,
    pub compaction_review: Option<HttpCompactionReview>,
    pub tool_output_shrink: Option<crate::HttpToolOutputShrinkReceipt>,
    pub restore: Option<HttpCheckpointRestoreReceipt>,
    pub fork: Option<HttpConversationForkReceipt>,
    pub recovery: HttpConversationRecoveryView,
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
        model_ref: Option<&crate::HttpProviderModelRef>,
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
        _recovery_binding: Option<&str>,
    ) -> Result<HttpSessionBinding, HttpSessionOpenBindingError> {
        Err(HttpSessionOpenBindingError::Unavailable)
    }

    /// Purges process-local material owned by one durable session after its source was deleted.
    ///
    /// The durable deletion path calls this only after the catalog mutation succeeds. The default
    /// is a no-op for drivers that retain no session-scoped secrets or caches.
    fn purge_session_local_state(&self, _durable_session_scope_id: &str) {}

    /// Acquires exact cross-process ownership for one ready durable catalog mutation.
    fn acquire_durable_session_mutation_attachment(
        &self,
        _durable_session_scope_id: &str,
        _session_log_path: &Path,
    ) -> Result<HttpDurableSessionAttachmentGuard, HttpRunAdmissionError> {
        Ok(HttpDurableSessionAttachmentGuard::unmanaged())
    }

    /// Acquires write ownership and validates route recovery before registry run allocation.
    fn admit_run_start(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpRunStartRequest,
    ) -> Result<(), HttpRunAdmissionError> {
        Ok(())
    }

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

    /// Requests an exact durable Task pause for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot prove the rendered task/plan/scope
    /// binding or route the pause to its cancellation owner.
    fn pause_task(&self, _pause: HttpRunDriverTaskPause) -> Result<(), HttpRunDriverError> {
        Err(HttpRunDriverError::new("Task pause is unavailable"))
    }

    /// Cancels one persistent terminal task without cancelling its completed foreground turn.
    fn cancel_terminal_task(
        &self,
        _cancel: HttpRunDriverTerminalTaskCancel,
    ) -> Result<HttpTerminalLifecycleView, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "persistent terminal cancellation is unavailable",
        ))
    }

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

    /// Projects the adapter-neutral durable Intent Stack for one bound session.
    fn intent_stack_view(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<HttpIntentStackView, HttpIntentStackDriverError> {
        Err(HttpIntentStackDriverError::Unavailable)
    }

    /// Builds one fresh exact Intent Drop preview without mutation authority.
    fn preview_intent_drop(
        &self,
        _session: &HttpSessionSnapshot,
        _intent_ref: &sigil_kernel::IntentVersionRef,
    ) -> Result<HttpIntentDropPreview, HttpIntentStackDriverError> {
        Err(HttpIntentStackDriverError::Unavailable)
    }

    /// Executes one digest-bound Drop request under host-owned permission and trust authority.
    fn execute_intent_drop(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpIntentDropRequest,
    ) -> Result<HttpIntentDropExecution, HttpIntentStackDriverError> {
        Err(HttpIntentStackDriverError::Unavailable)
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

    /// Reads one session-scoped, integrity-checked, bounded tool artifact page.
    ///
    /// # Errors
    ///
    /// Returns a typed fail-closed rejection. Implementations must not include physical paths,
    /// artifact bodies, or backend error strings in an error returned to clients.
    fn tool_artifact_page(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpToolArtifactReadRequest,
    ) -> Result<HttpToolArtifactPage, HttpToolArtifactReadDriverError> {
        Err(HttpToolArtifactReadDriverError::Unavailable)
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

    /// Projects exact checkpoint and finalized-turn recovery bindings.
    fn conversation_recovery_view(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<HttpConversationRecoveryView, HttpConversationRecoveryDriverError> {
        Err(HttpConversationRecoveryDriverError::Unavailable)
    }

    /// Builds one local-only portable compaction review and retains its exact process-local plan.
    ///
    /// Provider consumption requires the separate `prepare_compaction` recovery action; this
    /// query never activates compaction or sends a semantic-summary request.
    fn conversation_compaction_review(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<HttpCompactionReview, HttpConversationRecoveryDriverError> {
        Err(HttpConversationRecoveryDriverError::Unavailable)
    }

    /// Revalidates one exact checkpoint binding and returns its reverse-diff review.
    fn checkpoint_restore_review(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpCheckpointRestoreRequest,
    ) -> Result<HttpCheckpointRestoreReview, HttpConversationRecoveryDriverError> {
        Err(HttpConversationRecoveryDriverError::Unavailable)
    }

    /// Applies one exact restore or conversation-fork mutation.
    fn mutate_conversation_recovery(
        &self,
        _session: &HttpSessionSnapshot,
        _command: &HttpConversationRecoveryDriverCommand,
    ) -> Result<HttpConversationRecoveryDriverOutput, HttpConversationRecoveryDriverError> {
        Err(HttpConversationRecoveryDriverError::Unavailable)
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

    /// Projects the current exact Task integration review without exposing private lane state.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable binding or immutable aggregate artifact cannot be
    /// verified.
    fn task_integration_review(
        &self,
        _session: &HttpSessionSnapshot,
    ) -> Result<Option<HttpTaskIntegrationReviewView>, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "Task integration review is unavailable",
        ))
    }

    /// Accepts one exact current Task integration review under the shared foreground lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is stale, another operation owns the session, promotion
    /// fails, or authoritative parent verification cannot be recorded.
    fn accept_task_integration(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpTaskIntegrationReviewRequest,
    ) -> Result<HttpTaskIntegrationAcceptanceView, HttpRunDriverError> {
        Err(HttpRunDriverError::new(
            "Task integration acceptance is unavailable",
        ))
    }

    /// Applies one exact typed plan decision under the shared session lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan is stale, the decision conflicts with durable facts, or the
    /// session cannot be opened.
    fn plan_decision(
        &self,
        _session: &HttpSessionSnapshot,
        _request: &HttpPlanDecisionRequest,
    ) -> Result<HttpPlanDecisionCommandReceipt, HttpRunDriverError> {
        Err(HttpRunDriverError::new("Plan decision is unavailable"))
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

/// Typed, secret-free run admission failures returned before a run id is allocated.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpRunAdmissionError {
    #[error("session route recovery is required")]
    RouteRecovery(HttpSessionRouteRecoveryView),
    #[error("session is already active")]
    SessionAlreadyActive { recovery_binding: String },
    #[error("session write admission is unavailable")]
    Unavailable,
}

/// Bounded failure classes for the typed Intent Stack application adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpIntentStackDriverError {
    #[error("Intent Stack request is invalid")]
    InvalidRequest,
    #[error("Intent Stack request is stale")]
    Stale,
    #[error("Intent Stack operation requires permission")]
    PermissionRequired,
    #[error("Intent Stack operation conflicts with current durable state")]
    Conflict,
    #[error("Intent Stack is unavailable")]
    Unavailable,
}

/// Bounded, path-free failure direction returned while reopening an existing durable session.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
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
    /// Another controller owns the exact cross-process session attachment.
    #[error("durable session is already active")]
    AlreadyActive { recovery_binding: String },
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

/// Typed fail-closed errors for display-surface artifact retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpToolArtifactReadDriverError {
    /// The opaque reference does not conform to the supported schema.
    #[error("tool artifact reference is invalid")]
    InvalidReference,
    /// The selector exceeds a byte, line, match, context, or literal bound.
    #[error("tool artifact selector is invalid")]
    InvalidSelector,
    /// The reference is not resolvable in this logical session scope or has expired.
    #[error("tool artifact is unavailable")]
    Unavailable,
    /// The immutable artifact bytes no longer match their durable descriptor.
    #[error("tool artifact failed integrity validation")]
    Corrupt,
    /// Current persistence or retrieval policy does not authorize display access.
    #[error("tool artifact retrieval is not authorized")]
    PolicyRevoked,
}

/// Typed rejection surface for checkpoint and conversation-fork recovery operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpConversationRecoveryDriverError {
    /// The exact checkpoint digest or finalized-turn digest is stale or unavailable.
    #[error("conversation recovery binding is stale")]
    StaleBinding,
    /// Fresh workspace or durable truth conflicts with the requested mutation.
    #[error("conversation recovery conflicts with current state")]
    Conflict,
    /// Current durable projection or recovery owner is unavailable.
    #[error("conversation recovery is unavailable")]
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
