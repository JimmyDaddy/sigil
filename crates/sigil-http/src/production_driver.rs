use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc as std_mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalHandler, ApprovalRequestIdentityV2, CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX,
    ControlEntry, ConversationInputEditedEntry, ConversationInputKind,
    ConversationInputPromotedEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputReorderedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, ConversationInputTerminalCommand,
    ConversationInputTerminalExpectation, ConversationInputTerminalFrontier,
    ConversationQueueDurableProjection, ConversationQueueMutation,
    ConversationQueueMutationCommand, ConversationQueueRevision, ExecutionContainmentRequest,
    JsonlSessionStore, ModelMessage, PermissionDecisionReason, PermissionRisk,
    ProviderPhysicalAttemptOutcome, ProviderPhysicalAttemptProjection, PublicRouteRecoveryAction,
    PublicRouteRecoveryCode, PublicRunEvent, PublicRunEventKind, RootConfig, SecretString,
    SessionLogEntry, SessionRef, ToolAnalysisStatus, ToolApproval, ToolApprovalContext,
    ToolApprovalUserDecision, ToolArtifactAvailability, ToolArtifactDescriptorV1,
    ToolArtifactEncoding, ToolArtifactRefV1, ToolCall, ToolOperation,
    ToolOutputArchivedArtifactBindingV1, ToolPermissionEffect, ToolPermissionSummary, ToolSpec,
    ToolSubject, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
    project_user_message_for_persistence_with_nonce_and_issued_at, safe_persistence_text,
    stable_event_uuid,
};
use sigil_runtime::application_compaction::{
    PendingApplicationCompaction, PendingApplicationCompactionPreview,
    prepare_application_compaction_from_preview_with_attachment,
    preview_application_compaction_with_attachment,
};
use sigil_runtime::application_intent_stack::{
    ApplicationIntentConfirmationSource, ApplicationIntentStackCommandOutputV1,
    ApplicationIntentStackCommandV1, ApplicationIntentStackErrorClass,
    execute_durable_application_intent_stack_command,
};
use sigil_runtime::application_queue::{
    ApplicationQueuedPromptMaterial, ApplicationQueuedRunRequest, prepare_application_queued_run,
};
use sigil_runtime::application_recovery::{
    application_conversation_recovery_view, application_recovery_workspace_root,
    preview_application_checkpoint_restore, restore_application_checkpoint,
};
use sigil_runtime::application_run::{
    ApplicationPostRunMaintenance, ApplicationRunControl, ApplicationRunEventHandler,
    ApplicationRunExecution, ApplicationRunInteraction, ApplicationRunRequest,
    ApplicationRunServices, ApplicationRunTerminalStatus, ApplicationTaskContinuationExecution,
    ApplicationTaskContinuationRequest, ApplicationTerminalTaskControl, ApplicationTranscriptRole,
    ApplicationUserInputDecisionRequest, PreparedApplicationRun,
    PreparedApplicationTaskContinuation, PreparedApplicationUserInputDecision,
    accept_application_task_integration_review_with_attachment, application_agent_activity_view,
    application_recoverable_user_input_decision, application_run_context_view,
    application_session_frontier_view, application_session_has_unresolved_user_input,
    application_session_transcript_page, application_task_integration_review_view,
    application_user_input_request_view_by_key, application_verification_view,
    bind_application_session_with_model_ref_and_attachment_and_managed_writer,
    bind_existing_application_session, bind_existing_application_session_with_attachment,
    prepare_application_run, prepare_application_task_continuation,
    prepare_application_user_input_decision,
    record_application_preparation_cancellation_with_attachment,
    rerun_application_verification_with_attachment,
};
use sigil_runtime::conversation_display::{
    ConversationDisplayProjectionError, conversation_display_page_with_artifact_store,
};
use sigil_runtime::{LocalSessionLifecycleService, LocalSessionReopenError};
use tokio::{runtime::Handle, sync::mpsc};

use crate::{
    HttpAgentActivityItem, HttpAgentActivityStatus, HttpAgentActivityView, HttpAgentHandoffStatus,
    HttpAgentUsageSummary, HttpApplicationAgentCatalogEntry, HttpApplicationCacheUsage,
    HttpApplicationClientAction, HttpApplicationCommandCatalogEntry,
    HttpApplicationExtensionCatalog, HttpApplicationModelOption, HttpApplicationSkillBinding,
    HttpApplicationSkillCatalogEntry, HttpApprovalDecisionRecord, HttpCheckpointRestoreReceipt,
    HttpCheckpointRestoreReview, HttpCompactionReceipt, HttpCompactionReview,
    HttpContextWindowSource, HttpConversationDisplayDriverError, HttpConversationDisplayPage,
    HttpConversationForkReceipt, HttpConversationQueueBlockedReason,
    HttpConversationQueueCommandAction, HttpConversationQueueDriverCommand,
    HttpConversationQueueDriverError, HttpConversationQueueGeneration, HttpConversationQueueItem,
    HttpConversationQueueItemKind, HttpConversationQueueItemStatus,
    HttpConversationQueuePromptMaterial, HttpConversationQueueView,
    HttpConversationRecoveryCommandAction, HttpConversationRecoveryDriverCommand,
    HttpConversationRecoveryDriverError, HttpConversationRecoveryDriverOutput,
    HttpConversationRecoveryView, HttpDurableCommandStore, HttpDurableEgressDisclosureJournal,
    HttpDurableEgressDisclosurePresenter, HttpIntentDropExecution, HttpIntentDropPreview,
    HttpIntentDropRequest, HttpIntentStackDriverError, HttpIntentStackView, HttpLiveEventBus,
    HttpModelSelectionPolicy, HttpPendingApproval, HttpPendingApprovalDisplay,
    HttpPendingApprovalSubject, HttpPermissionMode, HttpPlanDecisionCommandReceipt,
    HttpPlanDecisionRequest, HttpPlanReviewDetail, HttpQueuedRunAdmission,
    HttpQueuedRunDriverStart, HttpRunAdmissionError, HttpRunContextView, HttpRunDriver,
    HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverError, HttpRunDriverStart,
    HttpRunDriverTaskPause, HttpRunDriverTerminalTaskCancel, HttpRunSnapshot, HttpRunStartRequest,
    HttpRunTerminalOutcome, HttpSessionBinding, HttpSessionOpenBindingError,
    HttpSessionRouteRecoveryCode, HttpSessionRunRegistry, HttpSessionSnapshot,
    HttpSessionTranscriptMessage, HttpSessionTranscriptPage, HttpTaskIntegrationAcceptanceView,
    HttpTaskIntegrationReviewRequest, HttpTaskIntegrationReviewView, HttpToolArtifactPage,
    HttpToolArtifactReadDriverError, HttpToolArtifactReadRequest, HttpToolOutputShrinkReceipt,
    HttpTranscriptAssistantKind, HttpTranscriptRole, HttpUserInputDecisionCommandReceipt,
    HttpUserInputDecisionDriverCommand, HttpUserInputDecisionRequest, HttpUserInputRequest,
    HttpVerificationRerunRequest, HttpVerificationView,
};

const DEFAULT_HTTP_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_EXACT_QUEUE_PROMPTS: usize = 128;
const MAX_HTTP_QUEUE_PREVIEW_CHARS: usize = 240;
const MAX_HTTP_PENDING_COMPACTION_PREVIEWS: usize = 32;
const MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES: usize = 256;

/// Runtime inputs and bounded waits owned by the production HTTP driver.
#[derive(Debug, Clone)]
pub struct HttpProductionRunDriverOptions {
    /// Resolved Sigil configuration path.
    pub config_path: PathBuf,
    /// Process launch working directory used for workspace resolution.
    pub launch_cwd: PathBuf,
    /// Maximum time allowed for cooperative cancellation quiescence.
    pub cancellation_timeout: Duration,
    /// Workspace-bound lifecycle truth used to authorize historical session reopen.
    pub session_lifecycle: Option<LocalSessionLifecycleService>,
    /// RFC-0062 14.1: process-scoped scratch lease registry shared by every run tool surface
    /// and session-delete cleanup in this serve process.
    pub scratch_control: Option<sigil_runtime::RuntimeScratchNamespaceControl>,
}

impl HttpProductionRunDriverOptions {
    /// Creates production defaults for one config/workspace pair.
    #[must_use]
    pub fn new(config_path: impl Into<PathBuf>, launch_cwd: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            cancellation_timeout: DEFAULT_HTTP_CANCELLATION_TIMEOUT,
            session_lifecycle: None,
            scratch_control: None,
        }
    }

    /// Attaches workspace-bound lifecycle truth for durable session reopen.
    #[must_use]
    pub fn with_session_lifecycle(
        mut self,
        session_lifecycle: LocalSessionLifecycleService,
    ) -> Self {
        self.session_lifecycle = Some(session_lifecycle);
        self
    }

    /// Shares the process-scoped scratch lease registry with every run tool surface.
    #[must_use]
    pub fn with_scratch_control(
        mut self,
        scratch_control: sigil_runtime::RuntimeScratchNamespaceControl,
    ) -> Self {
        self.scratch_control = Some(scratch_control);
        self
    }
}

#[async_trait]
trait HttpApplicationRunPreparer: Send + Sync {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun>;

    async fn prepare_queued(
        &self,
        request: ApplicationQueuedRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun>;

    async fn prepare_task(
        &self,
        _request: ApplicationTaskContinuationRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationTaskContinuation> {
        Err(anyhow!("application Task continuation is unavailable"))
    }

    async fn prepare_user_input(
        &self,
        _request: ApplicationUserInputDecisionRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationUserInputDecision> {
        Err(anyhow!("application user input is unavailable"))
    }
}

struct HttpSharedApplicationRunPreparer;

#[async_trait]
impl HttpApplicationRunPreparer for HttpSharedApplicationRunPreparer {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        prepare_application_run(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }

    async fn prepare_queued(
        &self,
        request: ApplicationQueuedRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        prepare_application_queued_run(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }

    async fn prepare_task(
        &self,
        request: ApplicationTaskContinuationRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationTaskContinuation> {
        prepare_application_task_continuation(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }

    async fn prepare_user_input(
        &self,
        request: ApplicationUserInputDecisionRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationUserInputDecision> {
        prepare_application_user_input_decision(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }
}

enum HttpPreparedApplicationRun {
    Conversation(Box<PreparedApplicationRun>),
    Task(Box<PreparedApplicationTaskContinuation>),
}

impl HttpPreparedApplicationRun {
    fn session_id(&self) -> &str {
        match self {
            Self::Conversation(prepared) => prepared.session_id(),
            Self::Task(prepared) => prepared.session_id(),
        }
    }

    fn session_log_path(&self) -> &Path {
        match self {
            Self::Conversation(prepared) => prepared.session_log_path(),
            Self::Task(prepared) => prepared.session_log_path(),
        }
    }

    fn terminal_control(&self) -> ApplicationTerminalTaskControl {
        match self {
            Self::Conversation(prepared) => prepared.terminal_control(),
            Self::Task(prepared) => prepared.terminal_control(),
        }
    }

    fn into_parts(self) -> (HttpApplicationRunExecution, ApplicationRunControl) {
        match self {
            Self::Conversation(prepared) => {
                let (execution, control) = (*prepared).into_parts();
                (
                    HttpApplicationRunExecution::Conversation(Box::new(execution)),
                    control,
                )
            }
            Self::Task(prepared) => {
                let (execution, control) = (*prepared).into_parts();
                (
                    HttpApplicationRunExecution::Task(Box::new(execution)),
                    control,
                )
            }
        }
    }
}

enum HttpApplicationRunExecution {
    Conversation(Box<ApplicationRunExecution>),
    Task(Box<ApplicationTaskContinuationExecution>),
}

impl HttpApplicationRunExecution {
    async fn execute_on_owned_blocking(
        self,
        event_handler: HttpProductionEventHandler,
        approval_handler: HttpProductionApprovalHandler,
        post_run_maintenance: Arc<Mutex<Option<ApplicationPostRunMaintenance>>>,
    ) -> Result<ApplicationRunTerminalStatus> {
        match self {
            Self::Conversation(execution) => {
                let output = (*execution)
                    .execute_on_owned_blocking(event_handler, approval_handler)
                    .await?;
                *post_run_maintenance
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = output.post_run_maintenance;
                Ok(output.terminal_status)
            }
            Self::Task(execution) => (*execution)
                .execute_on_owned_blocking(event_handler, approval_handler)
                .await
                .map(|output| output.terminal_status),
        }
    }
}

/// Production run driver backed by the shared runtime application service.
pub struct HttpProductionRunDriver {
    options: HttpProductionRunDriverOptions,
    services: ApplicationRunServices,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    runtime: Handle,
    registry: OnceLock<Weak<HttpSessionRunRegistry>>,
    active_runs: Arc<Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: Arc<Condvar>,
    terminal_owners: Arc<Mutex<BTreeMap<String, HttpProductionTerminalOwner>>>,
    exact_queue_prompts: Arc<Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>>,
    pending_compactions: Arc<Mutex<BTreeMap<String, PendingHttpCompaction>>>,
    session_attachments: Mutex<
        BTreeMap<
            String,
            Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        >,
    >,
    session_projection_stores: Mutex<HttpSessionProjectionStoreCache>,
    reconciled_terminal_sessions: Mutex<BTreeSet<String>>,
}

/// Publishes one plan review revision run's public events to the live event bus.
///
/// The revision runs as a supervised owned background run registered in `active_runs`; the
/// driver publishes an explicit terminal event that closes the SSE stream, and the renderer picks
/// up the durable draft or terminal attempt through the canonical display projection.
struct HttpPlanReviewRevisionEventHandler {
    durable_session_scope_id: String,
    run_id: String,
    event_bus: Arc<HttpLiveEventBus>,
}

impl sigil_runtime::application_run::ApplicationRunEventHandler
    for HttpPlanReviewRevisionEventHandler
{
    fn handle_public_event(&mut self, event: sigil_kernel::PublicRunEvent) -> anyhow::Result<()> {
        if event.session_id != self.durable_session_scope_id || event.run_id != self.run_id {
            anyhow::bail!("plan review revision event scope mismatch");
        }
        self.event_bus.publish_next_run_event(event)?;
        Ok(())
    }
}

enum PendingHttpCompaction {
    Local(Box<PendingApplicationCompactionPreview>),
    Ready(Box<PendingApplicationCompaction>),
}

struct HttpRetainedSessionProjectionStore {
    session_log_path: String,
    store: JsonlSessionStore,
    last_used_sequence: u64,
}

#[derive(Default)]
struct HttpSessionProjectionStoreCache {
    entries: BTreeMap<String, HttpRetainedSessionProjectionStore>,
    access_sequence: u64,
}

impl PendingHttpCompaction {
    fn session_scope_id(&self) -> &str {
        match self {
            Self::Local(pending) => pending.session_scope_id(),
            Self::Ready(pending) => pending.session_scope_id(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpExactQueuePromptKey {
    session_scope_id: String,
    queue_id: ConversationInputQueueId,
}

#[derive(Clone)]
struct HttpExactQueuePrompt {
    prompt_hash: String,
    exact_prompt: SecretString,
}

struct HttpQueuedRunPreparation {
    durable_queue: ConversationQueueDurableProjection,
    promotion: ConversationInputPromotedEntry,
    prompt_material: ApplicationQueuedPromptMaterial,
    capability_registrations: Vec<sigil_kernel::UserUrlCapabilityRegistration>,
    exact_prompt_key: HttpExactQueuePromptKey,
}

#[derive(Clone)]
struct HttpQueuedRunTerminalContext {
    queue_id: ConversationInputQueueId,
    dispatch_run_id: String,
    expected_queue_revision: ConversationQueueRevision,
    prompt_hash: String,
    exact_prompt_key: HttpExactQueuePromptKey,
}

#[derive(Clone, Copy)]
enum HttpQueuedUnpromotedTerminal {
    Rejected,
    Cancelled,
}

impl std::fmt::Debug for HttpExactQueuePrompt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpExactQueuePrompt")
            .field("prompt_hash", &self.prompt_hash)
            .field("exact_prompt", &"[redacted]")
            .finish()
    }
}

impl std::fmt::Debug for HttpProductionRunDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpProductionRunDriver")
            .field("options", &self.options)
            .field("services", &self.services)
            .field("preparer", &"configured")
            .field("event_bus", &"configured")
            .finish_non_exhaustive()
    }
}

fn canonical_http_session_path(session_log_path: &Path) -> Result<PathBuf> {
    Ok(JsonlSessionStore::new(session_log_path)?
        .path()
        .to_path_buf())
}

impl HttpProductionRunDriver {
    /// Returns the lifecycle service enriched with the current managed session-log source.
    #[must_use]
    pub fn session_lifecycle(&self) -> Option<&LocalSessionLifecycleService> {
        self.options.session_lifecycle.as_ref()
    }

    /// Returns the authority-owned host-private native-save port when the current boot was
    /// composed with the NewCurrentSchema authority surface.
    #[must_use]
    pub fn borrowed_native_save_service(
        &self,
    ) -> Option<Arc<dyn sigil_resource_authority::native_save::BorrowedNativeSaveServiceV1>> {
        self.services
            .authority_composition()
            .and_then(|composition| composition.services.borrowed_native_save.clone())
    }

    /// Returns the authority-owned host-private borrowed configuration port for the current boot.
    #[must_use]
    pub fn borrowed_configuration_service(
        &self,
    ) -> Option<Arc<dyn sigil_resource_authority::configuration::BorrowedConfigurationServiceV1>>
    {
        self.services
            .authority_composition()
            .and_then(|composition| composition.services.borrowed_configuration.clone())
    }

    /// Executes one prepared plan review revision as an owned, supervised background run so
    /// `Revise` runs a real read-only plan review instead of leaving a dangling `Started` attempt.
    ///
    /// The revision holds the durable session attachment for its entire duration (concurrent
    /// mutations are serialized), is registered in `active_runs` (so `wait_for_idle`, cancel and
    /// shutdown own it), publishes an explicit terminal public event that closes the SSE stream,
    /// and removes itself from `active_runs` on completion.
    fn spawn_plan_review_revision(
        &self,
        session: &crate::HttpSessionSnapshot,
        root_config: &sigil_kernel::RootConfig,
        workspace_root: &Path,
        request: sigil_runtime::PlanReviewRunRequest,
    ) -> Result<(), HttpRunDriverError> {
        let session_log_path = PathBuf::from(&session.session_log_path);
        let durable_session_scope_id = session.durable_session_scope_id.clone();
        let session_id = session.id.clone();
        let run_id = request.child_logical_run_id();
        let attachment = self.acquire_session_attachment(session).map_err(|error| {
            HttpRunDriverError::new(format!("plan review revision attachment failed: {error}"))
        })?;
        let registry = self.attached_registry()?;
        let (cancel_sender, mut cancel_receiver) = mpsc::unbounded_channel();
        {
            let mut runs = self
                .active_runs
                .lock()
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            if runs.contains_key(&run_id) {
                return Err(HttpRunDriverError::new(format!(
                    "production plan review revision already active: {run_id}"
                )));
            }
            runs.insert(
                run_id.clone(),
                Arc::new(HttpProductionActiveRun {
                    session_id,
                    broker: Arc::new(HttpApprovalBroker::default()),
                    cancel_sender,
                }),
            );
        }
        // The foreground slot is the last pre-spawn registration: a bind failure rolls the
        // active-run registration back so a half-registered revision can never leave the session
        // blocked behind a foreground slot no worker owns. The rollback only removes the run-map
        // entry this call inserted — the slot was never claimed, so no unbind is performed.
        if let Err(error) = registry.bind_supervised_session_run(&session.id, &run_id) {
            rollback_revision_run_registration(&self.active_runs, &self.active_runs_ready, &run_id);
            return Err(HttpRunDriverError::new(error.to_string()));
        }
        let event_bus = Arc::clone(&self.event_bus);
        let root_config = root_config.clone();
        let workspace_root = workspace_root.to_path_buf();
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_ready = Arc::clone(&self.active_runs_ready);
        let registry = Arc::downgrade(&registry);
        let release_session_id = session.id.clone();
        let cancellation_owner = sigil_kernel::RunCancellationOwner::new();
        let cancellation_handle = cancellation_owner.handle();
        let cancellation_timeout = self.options.cancellation_timeout;
        let runtime = self.runtime.clone();
        self.runtime.spawn(async move {
            let _held_session_attachment = attachment;
            let terminal_event_bus = event_bus.clone();
            let terminal_session_scope_id = durable_session_scope_id.clone();
            let terminal_run_id = run_id.clone();
            let mut run = Box::pin(async move {
                let mut handler = HttpPlanReviewRevisionEventHandler {
                    durable_session_scope_id,
                    run_id,
                    event_bus,
                };
                sigil_runtime::application_run::execute_plan_review_revision(
                    &root_config,
                    &workspace_root,
                    &session_log_path,
                    &request,
                    &mut handler,
                    Some(cancellation_handle),
                )
                .await
            });
            let outcome = loop {
                tokio::select! {
                    biased;
                    outcome = &mut run => break outcome,
                    control = cancel_receiver.recv() => match control {
                        Some(HttpProductionRunControlCommand::Cancel(cancellation)) => {
                            cancellation_owner.request_cancel();
                            let deadline = cancellation_deadline(cancellation_timeout);
                            let joined = tokio::time::timeout(remaining_until(deadline), &mut run).await;
                            match joined {
                                Ok(Ok(outcome)) => {
                                    let _ = cancellation.acknowledgement.send(Ok(()));
                                    break Ok(outcome);
                                }
                                Ok(Err(error)) => {
                                    let _ = cancellation.acknowledgement.send(Ok(()));
                                    break Err(error);
                                }
                                Err(_) => {
                                    let error = HttpRunDriverError::new(
                                        "plan review revision did not quiesce before the cancellation deadline",
                                    );
                                    let _ = cancellation.acknowledgement.send(Err(error.clone()));
                                    // The run keeps executing detached; ownership cleanup and the
                                    // terminal event happen only when it actually finishes.
                                    let active_runs = Arc::clone(&active_runs);
                                    let active_runs_ready = Arc::clone(&active_runs_ready);
                                    runtime.spawn(async move {
                                        let late_outcome = run.await;
                                        publish_plan_review_revision_terminal(
                                            &terminal_event_bus,
                                            &terminal_session_scope_id,
                                            &terminal_run_id,
                                            &late_outcome,
                                        );
                                        release_owned_revision_run(
                                            &active_runs,
                                            &active_runs_ready,
                                            &registry,
                                            &release_session_id,
                                            &terminal_run_id,
                                        );
                                    });
                                    return;
                                }
                            }
                        }
                        Some(HttpProductionRunControlCommand::Pause(pause)) => {
                            let _ = pause.acknowledgement.send(Err(HttpRunDriverError::new(
                                "plan review revision cannot be paused",
                            )));
                        }
                        None => {
                            break run.await;
                        }
                    }
                }
            };
            publish_plan_review_revision_terminal(
                &terminal_event_bus,
                &terminal_session_scope_id,
                &terminal_run_id,
                &outcome,
            );
            release_owned_revision_run(
                &active_runs,
                &active_runs_ready,
                &registry,
                &release_session_id,
                &terminal_run_id,
            );
        });
        Ok(())
    }
}

/// Publishes the explicit terminal public event for one plan review revision run and closes its
/// SSE stream so clients observe a definitive terminal instead of a dangling live stream.
fn publish_plan_review_revision_terminal(
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    outcome: &std::result::Result<sigil_runtime::PlanReviewRunOutcome, anyhow::Error>,
) {
    let kind = match outcome {
        Ok(sigil_runtime::PlanReviewRunOutcome::DraftReady { draft }) => {
            PublicRunEventKind::RunFinished {
                final_text: format!("Plan ready: {}", draft.summary),
            }
        }
        Ok(sigil_runtime::PlanReviewRunOutcome::CompletedWithoutDraft) => {
            PublicRunEventKind::RunFinished {
                final_text: "Plan review closed without a draft; no task was created.".to_owned(),
            }
        }
        Ok(sigil_runtime::PlanReviewRunOutcome::AwaitingUserInput { request }) => {
            PublicRunEventKind::RunAwaitingUserInput {
                request_id: request.identity.request_id.as_str().to_owned(),
                generation: request.identity.generation,
                request_hash: request.request_hash.clone(),
            }
        }
        Ok(sigil_runtime::PlanReviewRunOutcome::Cancelled) => PublicRunEventKind::RunCancelled,
        Ok(sigil_runtime::PlanReviewRunOutcome::Blocked(reason)) => {
            PublicRunEventKind::RunBlocked {
                reason: reason.clone(),
            }
        }
        Ok(sigil_runtime::PlanReviewRunOutcome::Paused(reason)) => PublicRunEventKind::RunPaused {
            reason: reason.clone(),
        },
        Ok(sigil_runtime::PlanReviewRunOutcome::Interrupted(error))
        | Ok(sigil_runtime::PlanReviewRunOutcome::Failed(error))
        | Ok(sigil_runtime::PlanReviewRunOutcome::SubmitOnlyProtocolViolation(error)) => {
            PublicRunEventKind::RunFailed {
                error: error.clone(),
            }
        }
        Err(error) => PublicRunEventKind::RunFailed {
            error: format!("{error:#}"),
        },
    };
    let _ = event_bus.publish_next_run_event_and_close_stream(PublicRunEvent::new(
        durable_session_scope_id,
        run_id,
        1,
        kind,
    ));
}

/// Rolls back a partially registered plan review revision: removes only the run-map entry this
/// call inserted and wakes idle/shutdown waiters. Unlike [`release_owned_revision_run`], it never
/// unbinds the registry foreground slot, because the slot was not claimed yet.
fn rollback_revision_run_registration(
    active_runs: &Arc<std::sync::Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: &Arc<Condvar>,
    run_id: &str,
) {
    if let Ok(mut runs) = active_runs.lock() {
        runs.remove(run_id);
        active_runs_ready.notify_all();
    }
}

/// Removes one owned plan review revision run from `active_runs`, releases its registry
/// foreground slot, and wakes idle/shutdown waiters.
fn release_owned_revision_run(
    active_runs: &Arc<std::sync::Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: &Arc<Condvar>,
    registry: &Weak<HttpSessionRunRegistry>,
    session_id: &str,
    run_id: &str,
) {
    if let Some(registry) = registry.upgrade() {
        registry.unbind_supervised_session_run(session_id, run_id);
    }
    if let Ok(mut runs) = active_runs.lock() {
        runs.remove(run_id);
        active_runs_ready.notify_all();
    }
}

impl HttpProductionRunDriver {
    fn acquire_exact_session_attachment(
        &self,
        durable_session_scope_id: &str,
        session_log_path: &Path,
    ) -> Result<
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        HttpRunAdmissionError,
    > {
        let canonical_session_path = canonical_http_session_path(session_log_path)
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        let attachments = self
            .session_attachments
            .lock()
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        if let Some(attachment) = attachments.get(durable_session_scope_id) {
            return if attachment.session_path() == canonical_session_path {
                Ok(Arc::clone(attachment))
            } else {
                Err(HttpRunAdmissionError::Unavailable)
            };
        }
        drop(attachments);
        let attachment =
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                &canonical_session_path,
            )
            .map_err(|error| match error {
                sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::Busy { observed_generation } => {
                    HttpRunAdmissionError::SessionAlreadyActive {
                        recovery_binding: stable_http_attachment_recovery_binding(
                            durable_session_scope_id,
                            &observed_generation,
                        ),
                    }
                }
                _ => HttpRunAdmissionError::Unavailable,
            })?;
        Ok(Arc::new(attachment))
    }

    /// Creates a production driver. Call `build_registry` before starting runs.
    ///
    /// # Errors
    ///
    /// Returns an error when the event bus has no durable protocol journal.
    pub fn new(
        options: HttpProductionRunDriverOptions,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
        event_bus: Arc<HttpLiveEventBus>,
        runtime: Handle,
    ) -> Result<Self, HttpRunDriverError> {
        Self::new_with_preparer(
            options,
            disclosure_journal,
            event_bus,
            runtime,
            Arc::new(HttpSharedApplicationRunPreparer),
        )
    }

    fn new_with_preparer(
        options: HttpProductionRunDriverOptions,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
        event_bus: Arc<HttpLiveEventBus>,
        runtime: Handle,
        preparer: Arc<dyn HttpApplicationRunPreparer>,
    ) -> Result<Self, HttpRunDriverError> {
        if !event_bus.has_durable_journal() {
            return Err(HttpRunDriverError::new(
                "production driver requires a durable protocol journal",
            ));
        }
        let services = ApplicationRunServices::new(Arc::new(
            HttpDurableEgressDisclosurePresenter::new(Arc::clone(&disclosure_journal)),
        ))
        .with_task_role_provider_builder(Arc::new(
            sigil_runtime::agent_supervisor::task_role_runtime::RuntimeTaskRoleProviderBuilder,
        ))
        .with_scratch_control(options.scratch_control.clone());
        // RFC-0071 R71.6: the server surface runs the one-call boot attach (epoch + authority
        // composition, shared with CLI/TUI) and fails closed before serving.
        let services = sigil_runtime::r71_authority_composition::attach_boot_authority_to_services(
            services,
            &options.config_path,
            &options.launch_cwd,
        )
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let mut options = options;
        let current_schema = services.cutover().is_some_and(|cutover| {
            cutover.manifest().selected_epoch
                == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
        });
        if current_schema
            && let Some(composition) = services.authority_composition()
            && let Some(lifecycle) = options.session_lifecycle.take()
        {
            let managed_session_log_root = composition
                .storage_writer
                .managed_leaf_path(
                    sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
            let managed_artifact_store_root = composition
                .storage_writer
                .managed_leaf_path(
                    sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore,
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
            let managed_artifact_staging_root = composition
                .storage_writer
                .managed_leaf_path(
                    sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging,
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
            options.session_lifecycle = Some(
                lifecycle
                    .with_managed_session_log_root(managed_session_log_root)
                    .and_then(|lifecycle| {
                        lifecycle.with_managed_artifact_roots(
                            managed_artifact_store_root,
                            managed_artifact_staging_root,
                        )
                    })
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?,
            );
        }
        if current_schema && let Some(composition) = services.authority_composition() {
            event_bus
                .attach_managed_protocol_replay(
                    Arc::clone(&composition.storage_writer),
                    "http-protocol-replay",
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
            disclosure_journal
                .attach_managed_writer(
                    Arc::clone(&composition.storage_writer),
                    "http-egress-disclosure",
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        }
        Ok(Self {
            options,
            services,
            preparer,
            event_bus,
            runtime,
            registry: OnceLock::new(),
            active_runs: Arc::new(Mutex::new(BTreeMap::new())),
            active_runs_ready: Arc::new(Condvar::new()),
            terminal_owners: Arc::new(Mutex::new(BTreeMap::new())),
            exact_queue_prompts: Arc::new(Mutex::new(BTreeMap::new())),
            pending_compactions: Arc::new(Mutex::new(BTreeMap::new())),
            session_attachments: Mutex::new(BTreeMap::new()),
            session_projection_stores: Mutex::new(HttpSessionProjectionStoreCache::default()),
            reconciled_terminal_sessions: Mutex::new(BTreeSet::new()),
        })
    }

    fn reconcile_terminal_session_once(
        &self,
        session_scope_id: &str,
        session_log_path: &Path,
    ) -> Result<(), HttpSessionOpenBindingError> {
        let mut reconciled = self
            .reconciled_terminal_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if reconciled.contains(session_scope_id) {
            return Ok(());
        }
        sigil_runtime::session_control::reconcile_terminal_tasks_after_restart(
            session_log_path,
            session_scope_id,
            sigil_runtime::current_unix_time_ms(),
        )
        .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
        reconciled.insert(session_scope_id.to_owned());
        Ok(())
    }

    /// Builds and attaches the one process-local registry driven by this instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver was already attached to another registry.
    pub fn build_registry(
        self: &Arc<Self>,
        command_store: Arc<HttpDurableCommandStore>,
    ) -> Result<Arc<HttpSessionRunRegistry>, HttpRunDriverError> {
        let current_schema = self.services.cutover().is_some_and(|cutover| {
            cutover.manifest().selected_epoch
                == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
        });
        if current_schema && let Some(composition) = self.services.authority_composition() {
            command_store
                .attach_managed_writer(
                    Arc::clone(&composition.storage_writer),
                    "http-idempotency-ledger",
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        }
        let driver: Arc<dyn HttpRunDriver> = self.clone();
        let registry = Arc::new(
            HttpSessionRunRegistry::with_durable_command_store(driver, command_store)
                .with_config_path(self.options.config_path.clone()),
        );
        self.registry
            .set(Arc::downgrade(&registry))
            .map_err(|_| HttpRunDriverError::new("production driver registry already attached"))?;
        Ok(registry)
    }

    /// Returns the number of owned run supervisors that have not completed cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error when the active-run state is unavailable.
    pub fn active_run_count(&self) -> Result<usize, HttpRunDriverError> {
        self.active_runs
            .lock()
            .map(|runs| runs.len())
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))
    }

    fn attached_registry(&self) -> Result<Arc<HttpSessionRunRegistry>, HttpRunDriverError> {
        self.registry
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| HttpRunDriverError::new("production driver registry is not attached"))
    }

    fn cancel_owned_terminal_tasks(
        &self,
        durable_session_scope_id: Option<&str>,
    ) -> Result<(), HttpRunDriverError> {
        let owners = self
            .terminal_owners
            .lock()
            .map_err(|_| HttpRunDriverError::new("production terminal-owner state unavailable"))?
            .iter()
            .filter(|(_, owner)| {
                durable_session_scope_id
                    .is_none_or(|expected| owner.durable_session_scope_id == expected)
            })
            .map(|(run_id, owner)| (run_id.clone(), owner.clone()))
            .collect::<Vec<_>>();
        if owners.is_empty() {
            return Ok(());
        }
        let registry = self.attached_registry()?;
        for (run_id, owner) in &owners {
            let run = registry.get_run(run_id).map_err(registry_driver_error)?;
            for task in run
                .terminal_tasks
                .iter()
                .filter(|task| !task.status.is_terminal())
            {
                let terminal = self
                    .runtime
                    .block_on(owner.control.cancel(&task.task_id))
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                if !terminal.status.is_terminal() {
                    return Err(HttpRunDriverError::new(
                        "persistent terminal cleanup did not reach a terminal state",
                    ));
                }
            }
        }
        let mut retained = self
            .terminal_owners
            .lock()
            .map_err(|_| HttpRunDriverError::new("production terminal-owner state unavailable"))?;
        for (run_id, _) in owners {
            retained.remove(&run_id);
        }
        Ok(())
    }

    fn reconcile_orphaned_queued_dispatches(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<(), HttpConversationQueueDriverError> {
        self.reconcile_orphaned_queued_dispatches_with(session, |_| Ok(()))
    }

    fn reconcile_orphaned_queued_dispatches_with<F>(
        &self,
        session: &crate::HttpSessionSnapshot,
        mut before_terminal_append: F,
    ) -> Result<(), HttpConversationQueueDriverError>
    where
        F: FnMut(&JsonlSessionStore) -> Result<(), HttpConversationQueueDriverError>,
    {
        for _ in 0..=crate::HTTP_MAX_CONVERSATION_QUEUE_ITEMS {
            let records = JsonlSessionStore::read_event_records(&session.session_log_path)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            if records
                .iter()
                .any(|record| record.session_id() != session.durable_session_scope_id)
            {
                return Err(HttpConversationQueueDriverError::Unavailable);
            }
            let projection = ConversationQueueDurableProjection::from_records(&records)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            let Some(item) = projection
                .queue
                .items
                .iter()
                .find(|item| item.status == ConversationInputStatus::Dispatching)
            else {
                return Ok(());
            };
            let promotion = http_queued_promotion(&records, &item.queued.queue_id)
                .ok_or(HttpConversationQueueDriverError::Conflict)?;
            let (status, reason) =
                http_queued_terminal_from_attempt_evidence(&records, &promotion.dispatch_run_id)
                    .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            let expected_frontier = records
                .last()
                .map(ConversationInputTerminalFrontier::from_record)
                .ok_or(HttpConversationQueueDriverError::Unavailable)?;
            let store = JsonlSessionStore::new(&session.session_log_path)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            before_terminal_append(&store)?;
            let appended = store
                .append_conversation_input_terminal_if_current(ConversationInputTerminalCommand {
                    expectation: ConversationInputTerminalExpectation::Promoted {
                        queue_id: promotion.queue_id.clone(),
                        dispatch_run_id: promotion.dispatch_run_id,
                        expected_frontier,
                    },
                    terminal: ConversationInputStatusEntry {
                        queue_id: promotion.queue_id.clone(),
                        status,
                        reason,
                        updated_at_ms: Some(current_unix_time_ms()),
                    },
                })
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            if appended.is_none() {
                continue;
            }
            self.exact_queue_prompts
                .lock()
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?
                .remove(&exact_queue_prompt_key(session, promotion.queue_id));
        }
        Err(HttpConversationQueueDriverError::Conflict)
    }

    fn queued_supervisor_start(
        &self,
        start: HttpQueuedRunDriverStart,
    ) -> Result<(HttpRunDriverStart, HttpQueuedRunPreparation), HttpRunDriverError> {
        if start.run.id != start.admission.dispatch_run_id
            || start.session.foreground_run_id.as_deref() != Some(start.run.id.as_str())
        {
            return Err(HttpRunDriverError::new(
                "queued run registration does not own the admitted foreground identity",
            ));
        }
        let state = read_http_durable_queue_state(&start.session)
            .map_err(|_| HttpRunDriverError::new("durable queued run state is unavailable"))?;
        let revision = state.projection.current_revision();
        if start.admission.generation != http_queue_generation(revision.clone()) {
            return Err(HttpRunDriverError::new(
                "queued run admission no longer matches the durable generation",
            ));
        }
        let queue_id = ConversationInputQueueId::new(start.admission.entry_id.clone())
            .map_err(|_| HttpRunDriverError::new("queued run entry identity is invalid"))?;
        if state.projection.queue.next_dispatchable.as_ref() != Some(&queue_id) {
            return Err(HttpRunDriverError::new(
                "queued run entry is no longer the durable dispatch frontier",
            ));
        }
        let queued = state
            .projection
            .queue
            .items
            .iter()
            .find(|item| item.queued.queue_id == queue_id)
            .ok_or_else(|| HttpRunDriverError::new("queued run entry is unavailable"))?;
        if queued.status != ConversationInputStatus::Queued
            || queued.queued.target != ConversationInputTarget::MainThread
            || queued.queued.kind != ConversationInputKind::Chat
        {
            return Err(HttpRunDriverError::new(
                "queued run entry is not a dispatchable main-thread chat",
            ));
        }
        let dispatch_run_id = stable_http_queued_dispatch_run_id(
            &start.session.durable_session_scope_id,
            &queue_id,
            &revision,
        );
        if dispatch_run_id != start.admission.dispatch_run_id {
            return Err(HttpRunDriverError::new(
                "queued run dispatch identity no longer matches durable admission",
            ));
        }

        let exact_prompt_key = exact_queue_prompt_key(&start.session, queue_id.clone());
        let (prompt_material, exact_prompt) = if queued
            .queued
            .prompt_hash
            .starts_with(CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX)
        {
            let exact_prompts = self
                .exact_queue_prompts
                .lock()
                .map_err(|_| HttpRunDriverError::new("queued exact prompt state is unavailable"))?;
            let exact = exact_prompts
                .get(&exact_prompt_key)
                .filter(|exact| exact.prompt_hash == queued.queued.prompt_hash)
                .ok_or_else(|| {
                    HttpRunDriverError::new("queued exact prompt requires user reentry")
                })?;
            (
                ApplicationQueuedPromptMaterial::AvailableProcessLocal {
                    queue_id: queue_id.clone(),
                    prompt_hash: exact.prompt_hash.clone(),
                    exact_prompt: exact.exact_prompt.clone(),
                },
                exact.exact_prompt.expose_secret().to_owned(),
            )
        } else {
            (
                ApplicationQueuedPromptMaterial::PersistedSafe,
                queued.queued.prompt.clone(),
            )
        };
        let prompt_projection = project_conversation_prompt_for_persistence(&exact_prompt);
        if prompt_projection.prompt_hash != queued.queued.prompt_hash
            || prompt_projection.safe_prompt != queued.queued.prompt
        {
            return Err(HttpRunDriverError::new(
                "queued exact prompt no longer matches its durable projection",
            ));
        }

        let promotion_seed = stable_http_identity_seed(&[
            &start.session.durable_session_scope_id,
            queue_id.as_str(),
            &revision.stream_sequence.to_string(),
            &revision.event_id,
        ]);
        let durable_message_id = stable_event_uuid(
            "sigil-http-conversation-queue-user-message",
            &promotion_seed,
        );
        let promoted_at_ms = current_unix_time_ms();
        let capability_projection = project_user_message_for_persistence_with_nonce_and_issued_at(
            durable_message_id.clone(),
            exact_prompt,
            Some(&dispatch_run_id),
            promoted_at_ms,
            None,
        )
        .map_err(|_| HttpRunDriverError::new("queued URL capability projection failed"))?;
        let mut capability_registrations = capability_projection.capability_registrations;
        capability_registrations.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        let capability_descriptors = capability_registrations
            .iter()
            .map(|registration| {
                registration.durable_descriptor(&start.session.durable_session_scope_id)
            })
            .collect::<Vec<_>>();
        let capability_digest =
            conversation_promotion_capability_digest(&capability_descriptors)
                .map_err(|_| HttpRunDriverError::new("queued capability digest failed"))?;
        let mut durable_user_message = ModelMessage::user(queued.queued.prompt.clone());
        durable_user_message.id = durable_message_id;
        let promotion = ConversationInputPromotedEntry {
            queue_id,
            expected_queue_revision: revision,
            prompt_hash: queued.queued.prompt_hash.clone(),
            exact_prompt_required: prompt_projection.exact_prompt_required,
            durable_user_message,
            capability_descriptors,
            capability_digest,
            dispatch_run_id,
            promoted_at_ms,
        };
        promotion
            .validate_for_session(&start.session.durable_session_scope_id)
            .map_err(|_| HttpRunDriverError::new("queued promotion candidate is invalid"))?;

        let run_context = application_run_context_view(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&start.session.session_log_path),
            &start.session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("queued run context is unavailable"))?;
        let reasoning_effort_binding = if start.run.reasoning_effort.is_some() {
            Some(run_context.reasoning_effort_binding.ok_or_else(|| {
                HttpRunDriverError::new(
                    "queued reasoning effort is unavailable for the current model",
                )
            })?)
        } else {
            None
        };

        let standard_start = HttpRunDriverStart {
            session: start.session,
            run: start.run,
            prompt: queued.queued.prompt.clone(),
            model_ref: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort_binding,
            skill_binding: None,
            agent_binding: None,
            task_continuation: None,
        };
        Ok((
            standard_start,
            HttpQueuedRunPreparation {
                durable_queue: state.projection,
                promotion,
                prompt_material,
                capability_registrations,
                exact_prompt_key,
            },
        ))
    }

    fn start_supervised_run(
        &self,
        start: HttpRunDriverStart,
        queued: Option<HttpQueuedRunPreparation>,
        preprepared: Option<HttpPreparedApplicationRun>,
    ) -> Result<(), HttpRunDriverError> {
        let session_attachment = self
            .acquire_session_attachment(&start.session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let registry = self.attached_registry()?;
        let broker = Arc::new(HttpApprovalBroker::default());
        let (cancel_sender, cancel_receiver) = mpsc::unbounded_channel();
        let active = Arc::new(HttpProductionActiveRun {
            session_id: start.session.id.clone(),
            broker: Arc::clone(&broker),
            cancel_sender,
        });
        {
            let mut runs = self
                .active_runs
                .lock()
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            if runs.contains_key(&start.run.id) {
                return Err(HttpRunDriverError::new(format!(
                    "production run already active: {}",
                    start.run.id
                )));
            }
            runs.insert(start.run.id.clone(), active);
        }

        let queued_terminal = queued.as_ref().map(|queued| HttpQueuedRunTerminalContext {
            queue_id: queued.promotion.queue_id.clone(),
            dispatch_run_id: queued.promotion.dispatch_run_id.clone(),
            expected_queue_revision: queued.promotion.expected_queue_revision.clone(),
            prompt_hash: queued.promotion.prompt_hash.clone(),
            exact_prompt_key: queued.exact_prompt_key.clone(),
        });
        let queued_session = start.session.clone();
        let terminal_exact_queue_prompts = Arc::clone(&self.exact_queue_prompts);
        let post_run_maintenance = Arc::new(Mutex::new(None));

        let supervisor = HttpRunSupervisor {
            options: self.options.clone(),
            services: self.services.clone(),
            preparer: Arc::clone(&self.preparer),
            event_bus: Arc::clone(&self.event_bus),
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&broker),
            start: start.clone(),
            session_attachment,
            queued,
            exact_queue_prompts: Arc::clone(&self.exact_queue_prompts),
            terminal_owners: Arc::clone(&self.terminal_owners),
            cancel_receiver,
            post_run_maintenance: Arc::clone(&post_run_maintenance),
        };
        let task = self.runtime.spawn(supervisor.run(preprepared));
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_ready = Arc::clone(&self.active_runs_ready);
        let terminal_owners = Arc::clone(&self.terminal_owners);
        let registry = Arc::downgrade(&registry);
        let run_id = start.run.id;
        self.runtime.spawn(async move {
            let mut uncertain = match task.await {
                Ok(Ok(())) => false,
                Ok(Err(_)) | Err(_) => true,
            };
            if let Some(queued_terminal) = queued_terminal {
                let unpromoted_terminal = registry
                    .upgrade()
                    .and_then(|registry| registry.get_run(&run_id).ok())
                    .map_or(HttpQueuedUnpromotedTerminal::Rejected, |run| {
                        match run.status {
                            crate::HttpRunStatus::Cancelled
                            | crate::HttpRunStatus::Paused
                            | crate::HttpRunStatus::Interrupted => {
                                HttpQueuedUnpromotedTerminal::Cancelled
                            }
                            _ => HttpQueuedUnpromotedTerminal::Rejected,
                        }
                    });
                uncertain |= tokio::task::spawn_blocking(move || {
                    finalize_http_queued_terminal(
                        &queued_session,
                        &queued_terminal,
                        unpromoted_terminal,
                    )?;
                    evict_http_promoted_exact_prompt(
                        &queued_session,
                        Some(&queued_terminal),
                        &terminal_exact_queue_prompts,
                    )
                })
                .await
                .map_or(true, |result| result.is_err());
            }
            broker.cancel_all();
            if uncertain && let Some(registry) = registry.upgrade() {
                let _ = registry.record_run_execution_uncertain(&run_id);
            }
            if let Ok(mut runs) = active_runs.lock() {
                runs.remove(&run_id);
                active_runs_ready.notify_all();
            }
            if let Some(registry) = registry.upgrade() {
                let _ = registry.record_run_released(&run_id);
                let has_active_terminal = registry.get_run(&run_id).ok().is_some_and(|run| {
                    run.terminal_tasks
                        .iter()
                        .any(|task| !task.status.is_terminal())
                });
                if !has_active_terminal && let Ok(mut owners) = terminal_owners.lock() {
                    owners.remove(&run_id);
                }
            } else if let Ok(mut owners) = terminal_owners.lock() {
                owners.remove(&run_id);
            }
            let maintenance = post_run_maintenance
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(maintenance) = maintenance
                && let Err(error) = maintenance.execute().await
            {
                tracing::debug!(
                    %error,
                    "post-run semantic session maintenance was not applied"
                );
            }
        });
        Ok(())
    }

    fn application_intent_stack_command(
        &self,
        session: &crate::HttpSessionSnapshot,
        command: &ApplicationIntentStackCommandV1,
    ) -> Result<ApplicationIntentStackCommandOutputV1, HttpIntentStackDriverError> {
        execute_durable_application_intent_stack_command(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            command,
            ApplicationIntentConfirmationSource::Http,
        )
        .map_err(|error| match error.class() {
            ApplicationIntentStackErrorClass::InvalidRequest => {
                HttpIntentStackDriverError::InvalidRequest
            }
            ApplicationIntentStackErrorClass::Stale => HttpIntentStackDriverError::Stale,
            ApplicationIntentStackErrorClass::PermissionRequired => {
                HttpIntentStackDriverError::PermissionRequired
            }
            ApplicationIntentStackErrorClass::Conflict => HttpIntentStackDriverError::Conflict,
            ApplicationIntentStackErrorClass::Unavailable => {
                HttpIntentStackDriverError::Unavailable
            }
        })
    }
}

impl HttpProductionRunDriver {
    fn install_session_attachment(
        &self,
        durable_session_scope_id: &str,
        session_log_path: &Path,
        attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
    ) -> Result<
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        HttpRunAdmissionError,
    > {
        let canonical_session_path = canonical_http_session_path(session_log_path)
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        if attachment.session_path() != canonical_session_path {
            return Err(HttpRunAdmissionError::Unavailable);
        }
        self.reconcile_terminal_session_once(durable_session_scope_id, &canonical_session_path)
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        let mut attachments = self
            .session_attachments
            .lock()
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        attachments.clear();
        attachments.insert(durable_session_scope_id.to_owned(), Arc::clone(&attachment));
        Ok(attachment)
    }

    fn acquire_session_attachment(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        HttpRunAdmissionError,
    > {
        let attachment = self.acquire_exact_session_attachment(
            &session.durable_session_scope_id,
            Path::new(&session.session_log_path),
        )?;
        self.install_session_attachment(
            &session.durable_session_scope_id,
            Path::new(&session.session_log_path),
            attachment,
        )
    }

    fn probe_session_attachment_recovery(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<crate::HttpSessionRouteRecoveryView>, HttpRunDriverError> {
        let canonical_session_path =
            canonical_http_session_path(Path::new(&session.session_log_path))
                .map_err(|_| HttpRunDriverError::new("session attachment state unavailable"))?;
        let owned = self
            .session_attachments
            .lock()
            .map_err(|_| HttpRunDriverError::new("session attachment state unavailable"))?
            .get(&session.durable_session_scope_id)
            .is_some_and(|attachment| attachment.session_path() == canonical_session_path);
        if owned {
            return Ok(None);
        }
        match sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &canonical_session_path,
        ) {
            Ok(attachment) => {
                drop(attachment);
                Ok(None)
            }
            Err(sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::Busy { observed_generation }) => {
                Ok(Some(crate::HttpSessionRouteRecoveryView {
                    code: crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive,
                    allowed_actions: vec![
                        crate::HttpSessionRouteRecoveryAction::RetrySessionAttach,
                        crate::HttpSessionRouteRecoveryAction::StartNewSession,
                        crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding: stable_http_attachment_recovery_binding(
                        &session.durable_session_scope_id,
                        &observed_generation,
                    ),
                    retryable: true,
                }))
            }
            Err(_) => Err(HttpRunDriverError::new(
                "session attachment probe is unavailable",
            )),
        }
    }

    fn retained_session_projection_store(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<JsonlSessionStore, HttpToolArtifactReadDriverError> {
        let mut stores = self
            .session_projection_stores
            .lock()
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        stores.access_sequence = stores.access_sequence.saturating_add(1);
        let access_sequence = stores.access_sequence;
        if let Some(retained) = stores.entries.get_mut(&session.durable_session_scope_id) {
            if retained.session_log_path != session.session_log_path {
                return Err(HttpToolArtifactReadDriverError::Unavailable);
            }
            retained.last_used_sequence = access_sequence;
            return Ok(retained.store.clone());
        }
        let store = JsonlSessionStore::new(Path::new(&session.session_log_path))
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        if stores.entries.len() >= MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES
            && let Some(evicted_scope_id) = stores
                .entries
                .iter()
                .min_by(|(left_scope_id, left), (right_scope_id, right)| {
                    left.last_used_sequence
                        .cmp(&right.last_used_sequence)
                        .then_with(|| left_scope_id.cmp(right_scope_id))
                })
                .map(|(scope_id, _)| scope_id.clone())
        {
            stores.entries.remove(&evicted_scope_id);
        }
        stores.entries.insert(
            session.durable_session_scope_id.clone(),
            HttpRetainedSessionProjectionStore {
                session_log_path: session.session_log_path.clone(),
                store: store.clone(),
                last_used_sequence: access_sequence,
            },
        );
        Ok(store)
    }

    fn projected_tool_artifact_binding(
        &self,
        session: &crate::HttpSessionSnapshot,
        artifact_ref: &ToolArtifactRefV1,
    ) -> Result<ToolOutputArchivedArtifactBindingV1, HttpToolArtifactReadDriverError> {
        let session_store = self.retained_session_projection_store(session)?;
        let active = session_store
            .active_projection_snapshot()
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        if active.frontier().session_id() != session.durable_session_scope_id {
            return Err(HttpToolArtifactReadDriverError::Unavailable);
        }

        let pressure = active.tool_output_pressure();
        let active_matches = pressure
            .items
            .iter()
            .filter(|item| item.artifact_ref.as_ref() == Some(artifact_ref))
            .count();
        let archived_by_key = pressure
            .archived_artifact_bindings
            .get(&artifact_ref.artifact_id);
        if archived_by_key.is_some_and(|binding| &binding.artifact_ref != artifact_ref)
            || active_matches > 1
            || (active_matches == 1 && archived_by_key.is_some())
        {
            return Err(HttpToolArtifactReadDriverError::Corrupt);
        }
        if active_matches == 0 && archived_by_key.is_none() {
            return Err(HttpToolArtifactReadDriverError::Unavailable);
        }

        let binding = pressure
            .artifact_source_binding(artifact_ref)
            .ok_or(HttpToolArtifactReadDriverError::Corrupt)?;
        if binding.source_event_id.trim().is_empty()
            || binding.source_stream_sequence == 0
            || binding.source_message_id.trim().is_empty()
        {
            return Err(HttpToolArtifactReadDriverError::Corrupt);
        }
        match binding.artifact_availability {
            ToolArtifactAvailability::Available => Ok(binding),
            ToolArtifactAvailability::HashMismatch => Err(HttpToolArtifactReadDriverError::Corrupt),
            ToolArtifactAvailability::PolicyRevoked => {
                Err(HttpToolArtifactReadDriverError::PolicyRevoked)
            }
            ToolArtifactAvailability::Expired
            | ToolArtifactAvailability::Missing
            | ToolArtifactAvailability::Unavailable => {
                Err(HttpToolArtifactReadDriverError::Unavailable)
            }
        }
    }
}

fn validate_projected_tool_artifact_descriptor(
    binding: &ToolOutputArchivedArtifactBindingV1,
    descriptor: &ToolArtifactDescriptorV1,
) -> Result<(), HttpToolArtifactReadDriverError> {
    if binding.artifact_ref != descriptor.artifact_ref
        || binding.artifact_sha256 != descriptor.content_sha256
        || binding.persisted_bytes != descriptor.persisted_bytes
        || binding.call_id != descriptor.tool_call_id
        || binding.tool_name != descriptor.tool_name
    {
        return Err(HttpToolArtifactReadDriverError::Corrupt);
    }
    Ok(())
}

fn authority_artifact_store_for_session(
    services: &ApplicationRunServices,
    session: &crate::HttpSessionSnapshot,
) -> Option<AuthorityArtifactStoreLease> {
    let current_schema = services.cutover().is_some_and(|cutover| {
        cutover.manifest().selected_epoch
            == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
    });
    let Some(composition) = services.authority_composition() else {
        return None;
    };
    let staging = sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging;
    let store = sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore;
    if !current_schema
        || !composition.declared_channels.contains(&staging)
        || !composition.declared_channels.contains(&store)
    {
        return None;
    }
    let key = Path::new(&session.session_log_path)
        .file_stem()
        .and_then(|value| value.to_str())?;
    let lease = sigil_runtime::managed_artifact_store::ManagedArtifactStoreLeaseV1::acquire_with_session_path(
        Arc::clone(&composition.storage_writer),
        key,
        &session.durable_session_scope_id,
        Path::new(&session.session_log_path).to_path_buf(),
    )
    .ok()?;
    Some(AuthorityArtifactStoreLease::managed(lease))
}

struct AuthorityArtifactStoreLease {
    managed: sigil_runtime::managed_artifact_store::ManagedArtifactStoreLeaseV1,
}

impl AuthorityArtifactStoreLease {
    fn managed(lease: sigil_runtime::managed_artifact_store::ManagedArtifactStoreLeaseV1) -> Self {
        Self { managed: lease }
    }

    fn store(&self) -> sigil_kernel::ToolArtifactStore {
        self.managed.store()
    }
}

impl HttpRunDriver for HttpProductionRunDriver {
    fn requires_run_release_barrier(&self) -> bool {
        true
    }

    fn bind_session(
        &self,
        session_id: &str,
        model_ref: Option<&crate::HttpProviderModelRef>,
    ) -> Result<HttpSessionBinding, HttpRunDriverError> {
        let connection_id = model_ref
            .map(|model_ref| sigil_kernel::ConnectionId::new(model_ref.connection_id.clone()))
            .transpose()
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let managed_session_log_writer = self
            .services
            .cutover()
            .filter(|cutover| {
                cutover.manifest().selected_epoch
                    == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
            })
            .and_then(|_| {
                self.services
                    .authority_composition()
                    .map(|composition| Arc::clone(&composition.storage_writer))
            });
        let (binding, attachment) =
            bind_application_session_with_model_ref_and_attachment_and_managed_writer(
                &self.options.config_path,
                &self.options.launch_cwd,
                None,
                connection_id.as_ref(),
                model_ref.map(|model_ref| model_ref.model_id.as_str()),
                managed_session_log_writer,
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "failed to bind durable session for {session_id}: {error}"
                ))
            })?;
        self.install_session_attachment(
            &binding.session_scope_id,
            &binding.session_log_path,
            attachment,
        )
        .map_err(|_| HttpRunDriverError::new("failed to retain durable session attachment"))?;
        Ok(HttpSessionBinding {
            session_scope_id: binding.session_scope_id,
            session_log_path: binding.session_log_path.display().to_string(),
            route_transition: Some(http_session_route_transition(binding.route_transition)),
            route_recovery: None,
        })
    }

    fn bind_existing_session(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
        recovery_binding: Option<&str>,
    ) -> Result<HttpSessionBinding, HttpSessionOpenBindingError> {
        let lifecycle = self
            .options
            .session_lifecycle
            .as_ref()
            .ok_or(HttpSessionOpenBindingError::Unavailable)?;
        let candidate = lifecycle
            .resolve_session_for_reopen(session_ref, expected_session_id)
            .map_err(|error| match error {
                LocalSessionReopenError::NotFound => HttpSessionOpenBindingError::NotFound,
                LocalSessionReopenError::NotReady { .. } => HttpSessionOpenBindingError::NotReady,
                LocalSessionReopenError::IdentityChanged => {
                    HttpSessionOpenBindingError::IdentityChanged
                }
                LocalSessionReopenError::CatalogUnavailable { .. } => {
                    HttpSessionOpenBindingError::Unavailable
                }
            })?;
        let read_binding = bind_existing_application_session(
            &self.options.config_path,
            &candidate.session_log_path,
        )
        .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
        if read_binding.session_scope_id != candidate.session_id
            || read_binding.session_scope_id != expected_session_id
            || read_binding.session_log_path != candidate.session_log_path
        {
            return Err(HttpSessionOpenBindingError::IdentityChanged);
        }
        let (attachment, attachment_recovery) = if let Some(recovery_binding) = recovery_binding {
            (Some(Arc::new(
                sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire_for_retry(
                    &candidate.session_log_path,
                    &candidate.session_id,
                    recovery_binding,
                )
                .map_err(|error| match error {
                    sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::Busy { observed_generation } => {
                        HttpSessionOpenBindingError::AlreadyActive {
                            recovery_binding: stable_http_attachment_recovery_binding(
                                &candidate.session_id,
                                &observed_generation,
                            ),
                        }
                    }
                    sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::StaleRecoveryBinding { recovery_binding } => {
                        HttpSessionOpenBindingError::AlreadyActive { recovery_binding }
                    }
                    _ => HttpSessionOpenBindingError::Unavailable,
                })?,
            )), None)
        } else {
            match self.acquire_exact_session_attachment(
                &candidate.session_id,
                &candidate.session_log_path,
            ) {
                Ok(attachment) => (Some(attachment), None),
                Err(HttpRunAdmissionError::SessionAlreadyActive { recovery_binding }) => {
                    (None, Some(http_attachment_route_recovery(recovery_binding)))
                }
                Err(_) => return Err(HttpSessionOpenBindingError::Unavailable),
            }
        };
        let (binding, route_recovery) = if let Some(attachment) = attachment.as_ref() {
            match bind_existing_application_session_with_attachment(
                &self.options.config_path,
                &candidate.session_log_path,
                attachment.as_ref(),
            ) {
                Ok(binding) => (binding, None),
                Err(error) => {
                    let Some(recovery) = http_route_recovery_from_prepare_error(
                        &error,
                        &stable_http_attachment_recovery_binding(
                            &candidate.session_id,
                            attachment.generation(),
                        ),
                    ) else {
                        return Err(HttpSessionOpenBindingError::Unavailable);
                    };
                    (read_binding, Some(recovery))
                }
            }
        } else {
            (read_binding, attachment_recovery)
        };
        if let Some(attachment) = attachment {
            self.install_session_attachment(
                &binding.session_scope_id,
                &binding.session_log_path,
                attachment,
            )
            .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
        }
        Ok(HttpSessionBinding {
            session_scope_id: binding.session_scope_id,
            session_log_path: binding.session_log_path.display().to_string(),
            route_transition: route_recovery
                .is_none()
                .then(|| http_session_route_transition(binding.route_transition)),
            route_recovery,
        })
    }

    fn recoverable_session_attention_command(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<Option<HttpUserInputDecisionDriverCommand>, HttpRunDriverError> {
        let Some(command) = application_recoverable_user_input_decision(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|error| {
            HttpRunDriverError::new(format!("user input recovery projection failed: {error:#}"))
        })?
        else {
            return Ok(None);
        };
        Ok(Some(HttpUserInputDecisionDriverCommand {
            command_id: command.command_id.as_str().to_owned(),
            client_id: "session-recovery".to_owned(),
            request_id: command.identity.request_id.as_str().to_owned(),
            request: HttpUserInputDecisionRequest {
                generation: command.identity.generation,
                expected_request_hash: command.request_hash,
                decision: command.decision,
                permission_mode: None,
            },
        }))
    }

    fn admit_run_start(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpRunStartRequest,
    ) -> Result<(), HttpRunAdmissionError> {
        self.acquire_session_attachment(session)?;
        let context = self
            .run_context_view(session)
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        let Some(recovery) = context.route_recovery else {
            return Ok(());
        };
        let admitted = match recovery.code {
            HttpSessionRouteRecoveryCode::SessionRouteConfirmationRequired => {
                request.route_recovery_binding.as_deref()
                    == Some(recovery.recovery_binding.as_str())
            }
            HttpSessionRouteRecoveryCode::SessionRouteSelectionRequired => {
                request.route_recovery_binding.as_deref()
                    == Some(recovery.recovery_binding.as_str())
                    && request.model_selection_binding.as_deref()
                        == Some(context.model_selection_binding.as_str())
                    && request.model_ref.as_ref().is_some_and(|requested| {
                        context.model_options.iter().any(|option| {
                            option.model_ref == *requested
                                && option.availability != "configured_unavailable"
                        })
                    })
            }
            HttpSessionRouteRecoveryCode::ModelRouteNotConfigured
            | HttpSessionRouteRecoveryCode::ConnectionConfigInvalid
            | HttpSessionRouteRecoveryCode::ProviderUnavailable
            | HttpSessionRouteRecoveryCode::SessionAlreadyActive
            | HttpSessionRouteRecoveryCode::SessionWriterBusy
            | HttpSessionRouteRecoveryCode::SessionStreamInvalid => false,
        };
        if admitted {
            Ok(())
        } else {
            Err(HttpRunAdmissionError::RouteRecovery(recovery))
        }
    }

    fn purge_session_local_state(&self, durable_session_scope_id: &str) {
        let _ = self.cancel_owned_terminal_tasks(Some(durable_session_scope_id));
        let mut exact_prompts = self
            .exact_queue_prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        exact_prompts.retain(|key, _| key.session_scope_id != durable_session_scope_id);
        let mut pending_compactions = self
            .pending_compactions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending_compactions
            .retain(|_, pending| pending.session_scope_id() != durable_session_scope_id);
        drop(exact_prompts);
        drop(pending_compactions);
        let mut session_projection_stores = self
            .session_projection_stores
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        session_projection_stores
            .entries
            .remove(durable_session_scope_id);
        self.reconciled_terminal_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_session_scope_id);
        self.session_attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_session_scope_id);
    }

    fn acquire_durable_session_mutation_attachment(
        &self,
        durable_session_scope_id: &str,
        session_log_path: &Path,
    ) -> Result<crate::HttpDurableSessionAttachmentGuard, HttpRunAdmissionError> {
        self.acquire_exact_session_attachment(durable_session_scope_id, session_log_path)
            .map(crate::HttpDurableSessionAttachmentGuard::attached)
    }

    fn session_frontier(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<crate::HttpDurableSessionFrontier, HttpRunDriverError> {
        let frontier = application_session_frontier_view(
            std::path::Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable session frontier is unavailable"))?;
        Ok(crate::HttpDurableSessionFrontier {
            through_stream_sequence: frontier.through_stream_sequence,
        })
    }

    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError> {
        self.start_supervised_run(start, None, None)
    }

    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&cancel.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", cancel.run_id))
        })?;
        if run.session_id != cancel.session_id {
            return Err(HttpRunDriverError::new(
                "production cancel session mismatch",
            ));
        }
        let (acknowledgement, acknowledged) = std_mpsc::sync_channel(1);
        run.cancel_sender
            .send(HttpProductionRunControlCommand::Cancel(
                HttpProductionCancellationCommand {
                    reason: cancel
                        .reason
                        .unwrap_or_else(|| "HTTP client requested cancellation".to_owned()),
                    acknowledgement,
                },
            ))
            .map_err(|_| HttpRunDriverError::new("production cancellation owner is closed"))?;
        acknowledged.recv().map_err(|_| {
            HttpRunDriverError::new(
                "production cancellation owner stopped before durable acknowledgement",
            )
        })?
    }

    fn pause_task(&self, pause: HttpRunDriverTaskPause) -> Result<(), HttpRunDriverError> {
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&pause.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", pause.run_id))
        })?;
        if run.session_id != pause.session_id {
            return Err(HttpRunDriverError::new(
                "production Task pause session mismatch",
            ));
        }
        let (acknowledgement, acknowledged) = std_mpsc::sync_channel(1);
        run.cancel_sender
            .send(HttpProductionRunControlCommand::Pause(
                HttpProductionTaskPauseCommand {
                    request: pause.request,
                    acknowledgement,
                },
            ))
            .map_err(|_| HttpRunDriverError::new("production Task pause owner is closed"))?;
        acknowledged.recv().map_err(|_| {
            HttpRunDriverError::new(
                "production Task pause owner stopped before durable acknowledgement",
            )
        })?
    }

    fn cancel_terminal_task(
        &self,
        cancel: HttpRunDriverTerminalTaskCancel,
    ) -> Result<crate::HttpTerminalLifecycleView, HttpRunDriverError> {
        let owner = self
            .terminal_owners
            .lock()
            .map_err(|_| HttpRunDriverError::new("production terminal-owner state unavailable"))?
            .get(&cancel.run_id)
            .cloned()
            .ok_or_else(|| {
                HttpRunDriverError::new(format!(
                    "persistent terminal owner is unavailable for run {}",
                    cancel.run_id
                ))
            })?;
        if owner.session_id != cancel.session_id {
            return Err(HttpRunDriverError::new(
                "production terminal cancellation session mismatch",
            ));
        }
        let before = self
            .runtime
            .block_on(owner.control.status(&cancel.task_id))
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        if before.generation != cancel.expected_generation {
            return Err(HttpRunDriverError::new(
                "production terminal cancellation generation changed",
            ));
        }
        let terminal = self
            .runtime
            .block_on(owner.control.cancel(&cancel.task_id))
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        if !terminal.status.is_terminal() {
            return Err(HttpRunDriverError::new(
                "production terminal cancellation did not confirm cleanup",
            ));
        }
        Ok(crate::HttpTerminalLifecycleView::from(&terminal))
    }

    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError> {
        if approval.call_id != approval.decision.call_id
            || approval.run_id != approval.decision.run_id
        {
            return Err(HttpRunDriverError::new(
                "production approval decision identity mismatch",
            ));
        }
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&approval.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", approval.run_id))
        })?;
        if run.session_id != approval.session_id {
            return Err(HttpRunDriverError::new(
                "production approval session mismatch",
            ));
        }
        run.broker.resolve(
            &approval.call_id,
            &approval.approval_request_id,
            approval.decision,
        )
    }

    fn verification_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<HttpVerificationView>, HttpRunDriverError> {
        application_verification_view(Path::new(&session.session_log_path)).map_err(|error| {
            HttpRunDriverError::new(format!("failed to project verification state: {error}"))
        })
    }

    fn intent_stack_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpIntentStackView, HttpIntentStackDriverError> {
        match self
            .application_intent_stack_command(session, &ApplicationIntentStackCommandV1::Inspect)?
        {
            ApplicationIntentStackCommandOutputV1::Projection { state } => Ok(state),
            _ => Err(HttpIntentStackDriverError::Unavailable),
        }
    }

    fn preview_intent_drop(
        &self,
        session: &crate::HttpSessionSnapshot,
        intent_ref: &sigil_kernel::IntentVersionRef,
    ) -> Result<HttpIntentDropPreview, HttpIntentStackDriverError> {
        intent_ref
            .validate()
            .map_err(|_| HttpIntentStackDriverError::InvalidRequest)?;
        match self.application_intent_stack_command(
            session,
            &ApplicationIntentStackCommandV1::PreviewDrop {
                intent_ref: intent_ref.clone(),
            },
        )? {
            ApplicationIntentStackCommandOutputV1::DropPreview { preview } => Ok(preview),
            _ => Err(HttpIntentStackDriverError::Unavailable),
        }
    }

    fn execute_intent_drop(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpIntentDropRequest,
    ) -> Result<HttpIntentDropExecution, HttpIntentStackDriverError> {
        self.acquire_session_attachment(session)
            .map_err(|error| match error {
                HttpRunAdmissionError::SessionAlreadyActive { .. }
                | HttpRunAdmissionError::RouteRecovery(_) => HttpIntentStackDriverError::Conflict,
                HttpRunAdmissionError::Unavailable => HttpIntentStackDriverError::Unavailable,
            })?;
        match self.application_intent_stack_command(
            session,
            &ApplicationIntentStackCommandV1::ExecuteDrop {
                request: request.clone(),
            },
        )? {
            ApplicationIntentStackCommandOutputV1::DropExecution { execution } => Ok(execution),
            _ => Err(HttpIntentStackDriverError::Unavailable),
        }
    }

    fn transcript_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        before: Option<u64>,
        limit: usize,
    ) -> Result<HttpSessionTranscriptPage, HttpRunDriverError> {
        let page = application_session_transcript_page(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            before,
            limit,
        )
        .map_err(|_| HttpRunDriverError::new("durable transcript projection failed"))?;
        Ok(HttpSessionTranscriptPage {
            session_scope_id: page.session_scope_id,
            total_messages: page.total_messages,
            messages: page
                .messages
                .into_iter()
                .map(|message| HttpSessionTranscriptMessage {
                    ordinal: message.ordinal,
                    message_id: message.message_id,
                    role: match message.role {
                        ApplicationTranscriptRole::User => HttpTranscriptRole::User,
                        ApplicationTranscriptRole::Assistant => HttpTranscriptRole::Assistant,
                        ApplicationTranscriptRole::Tool => HttpTranscriptRole::Tool,
                    },
                    content: message.content,
                    assistant_kind: message.assistant_kind.map(|kind| match kind {
                        sigil_kernel::AssistantMessageKind::ToolPreamble => {
                            HttpTranscriptAssistantKind::ToolPreamble
                        }
                        sigil_kernel::AssistantMessageKind::Progress => {
                            HttpTranscriptAssistantKind::Progress
                        }
                        sigil_kernel::AssistantMessageKind::ReasoningTrace => {
                            HttpTranscriptAssistantKind::ReasoningTrace
                        }
                        sigil_kernel::AssistantMessageKind::FinalAnswer => {
                            HttpTranscriptAssistantKind::FinalAnswer
                        }
                    }),
                    tool_name: message.tool_name,
                    image_attachment_count: message.image_attachment_count,
                    truncated: message.truncated,
                    original_content_bytes: message.original_content_bytes,
                })
                .collect(),
            next_before: page.next_before,
        })
    }

    fn conversation_display_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HttpConversationDisplayPage, HttpConversationDisplayDriverError> {
        let current_workspace_snapshot_id =
            sigil_kernel::RootConfig::load(&self.options.config_path)
                .ok()
                .and_then(|config| {
                    let workspace_root = sigil_kernel::resolve_workspace_root(
                        &self.options.config_path,
                        &self.options.launch_cwd,
                        &config.workspace.root,
                    );
                    sigil_runtime::plan_handoff_workspace_snapshot_id(&config, &workspace_root).ok()
                })
                .flatten();
        let artifact_lease = authority_artifact_store_for_session(&self.services, session)
            .ok_or(HttpConversationDisplayDriverError::Unavailable)?;
        let artifact_store = artifact_lease.store();
        let page = conversation_display_page_with_artifact_store(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            cursor,
            limit,
            current_workspace_snapshot_id.as_deref(),
            &artifact_store,
        )
        .map_err(|error| match error {
            ConversationDisplayProjectionError::InvalidCursor { .. } => {
                HttpConversationDisplayDriverError::InvalidCursor
            }
            ConversationDisplayProjectionError::StaleCursor { .. } => {
                HttpConversationDisplayDriverError::StaleCursor
            }
            ConversationDisplayProjectionError::Unavailable { .. } => {
                HttpConversationDisplayDriverError::Unavailable
            }
        })?;
        let mut page = HttpConversationDisplayPage::from_runtime(&session.id, page);
        if let Some(run_id) = session.foreground_run_id.as_deref() {
            let run_sequence = self
                .event_bus
                .latest_run_sequence(&session.durable_session_scope_id, run_id)
                .map_err(|_| HttpConversationDisplayDriverError::Unavailable)?
                .unwrap_or(0);
            page.live_provisional_anchor = Some(crate::HttpConversationLiveProvisionalAnchor {
                durable_frontier: page.through_session_stream_sequence.clone(),
                run_id: run_id.to_owned(),
                run_sequence: run_sequence.to_string(),
            });
        }
        Ok(page)
    }

    fn tool_artifact_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpToolArtifactReadRequest,
    ) -> Result<HttpToolArtifactPage, HttpToolArtifactReadDriverError> {
        let artifact_ref = ToolArtifactRefV1 {
            artifact_id: request.artifact_ref.clone(),
        };
        artifact_ref
            .validate()
            .map_err(|_| HttpToolArtifactReadDriverError::InvalidReference)?;
        request
            .selector
            .validate()
            .map_err(|_| HttpToolArtifactReadDriverError::InvalidSelector)?;

        let binding = self.projected_tool_artifact_binding(session, &artifact_ref)?;
        let artifact_lease = authority_artifact_store_for_session(&self.services, session)
            .ok_or(HttpToolArtifactReadDriverError::Unavailable)?;
        let store = artifact_lease.store();
        let descriptor = store
            .resolve(&artifact_ref)
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        validate_projected_tool_artifact_descriptor(&binding, &descriptor)?;
        if descriptor.encoding != ToolArtifactEncoding::Utf8
            && matches!(
                &request.selector,
                crate::HttpToolArtifactSelector::LinePage { .. }
                    | crate::HttpToolArtifactSelector::SearchLiteral { .. }
            )
        {
            return Err(HttpToolArtifactReadDriverError::InvalidSelector);
        }
        match store.availability(&descriptor) {
            ToolArtifactAvailability::Available => {}
            ToolArtifactAvailability::HashMismatch => {
                return Err(HttpToolArtifactReadDriverError::Corrupt);
            }
            ToolArtifactAvailability::PolicyRevoked => {
                return Err(HttpToolArtifactReadDriverError::PolicyRevoked);
            }
            ToolArtifactAvailability::Expired
            | ToolArtifactAvailability::Missing
            | ToolArtifactAvailability::Unavailable => {
                return Err(HttpToolArtifactReadDriverError::Unavailable);
            }
        }
        let page = store
            .read_page(&artifact_ref, request.selector.clone().into())
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        Ok(HttpToolArtifactPage::from_kernel(&session.id, page))
    }

    fn run_context_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpRunContextView, HttpRunDriverError> {
        let view = application_run_context_view(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable run-context projection failed"))?;
        let attachment_recovery = self.probe_session_attachment_recovery(session)?;
        Ok(HttpRunContextView {
            model_ref: crate::HttpProviderModelRef {
                connection_id: view.model_ref.connection_id.to_string(),
                model_id: view.model_ref.model_id,
            },
            provider_name: view.provider_name,
            model_name: view.model_name,
            model_options: view
                .model_options
                .into_iter()
                .map(|option| HttpApplicationModelOption {
                    model_ref: crate::HttpProviderModelRef {
                        connection_id: option.model_ref.connection_id.to_string(),
                        model_id: option.model_ref.model_id,
                    },
                    display_name: option.display_name,
                    availability: option.availability.as_str().to_owned(),
                    recommendation: option.recommendation.as_str().to_owned(),
                    provenance: option.provenance.as_str().to_owned(),
                    model_name: option.model_name,
                    available_reasoning_efforts: option
                        .available_reasoning_efforts
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    default_reasoning_effort: option.default_reasoning_effort.map(Into::into),
                    reasoning_effort_binding: option.reasoning_effort_binding,
                })
                .collect(),
            model_selection: HttpModelSelectionPolicy::SameSession,
            model_selection_binding: view.model_selection_binding,
            default_permission_mode: view.default_permission_mode.into(),
            available_permission_modes: vec![
                HttpPermissionMode::ReadOnly,
                HttpPermissionMode::Manual,
                HttpPermissionMode::AutoEdit,
                HttpPermissionMode::DangerFullAccess,
            ],
            available_reasoning_efforts: view
                .available_reasoning_efforts
                .into_iter()
                .map(Into::into)
                .collect(),
            default_reasoning_effort: view.default_reasoning_effort.map(Into::into),
            reasoning_effort_binding: view.reasoning_effort_binding,
            context_window_tokens: view.context_window_tokens,
            last_prompt_tokens: view.last_prompt_tokens,
            cache_usage: view.cache_usage.map(|usage| HttpApplicationCacheUsage {
                cache_read_tokens: usage.cache_read_tokens,
                cache_miss_tokens: usage.cache_miss_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                last_layout_mutation: usage
                    .last_layout_mutation
                    .map(|mutation| mutation.as_str().to_owned()),
                provider_miss_without_local_mutation: usage
                    .provider_miss_without_local_mutation,
            }),
            context_window_source: match view.context_window_source {
                sigil_runtime::ContextWindowSource::Connection => {
                    HttpContextWindowSource::Connection
                }
                sigil_runtime::ContextWindowSource::Provider => HttpContextWindowSource::Provider,
                sigil_runtime::ContextWindowSource::Config => HttpContextWindowSource::Config,
                sigil_runtime::ContextWindowSource::None => HttpContextWindowSource::Unavailable,
            },
            extension_catalog: HttpApplicationExtensionCatalog {
                commands: view
                    .extension_catalog
                    .commands
                    .into_iter()
                    .map(|entry| HttpApplicationCommandCatalogEntry {
                        canonical: entry.canonical,
                        aliases: entry.aliases,
                        label: entry.label,
                        description: entry.description,
                        argument_hint: entry.argument_hint,
                        completes_with_space: entry.completes_with_space,
                        client_action: entry.client_action.map(|action| match action {
                            sigil_runtime::ApplicationClientAction::PreviewCompaction => {
                                HttpApplicationClientAction::PreviewCompaction
                            }
                            sigil_runtime::ApplicationClientAction::OpenIntentStack => {
                                HttpApplicationClientAction::OpenIntentStack
                            }
                            sigil_runtime::ApplicationClientAction::NewSession => {
                                HttpApplicationClientAction::NewSession
                            }
                            sigil_runtime::ApplicationClientAction::FocusEffort => {
                                HttpApplicationClientAction::FocusEffort
                            }
                            sigil_runtime::ApplicationClientAction::FocusModel => {
                                HttpApplicationClientAction::FocusModel
                            }
                            sigil_runtime::ApplicationClientAction::OpenSessionPicker => {
                                HttpApplicationClientAction::OpenSessionPicker
                            }
                            sigil_runtime::ApplicationClientAction::OpenAgentWorkbench => {
                                HttpApplicationClientAction::OpenAgentWorkbench
                            }
                            sigil_runtime::ApplicationClientAction::OpenSettings => {
                                HttpApplicationClientAction::OpenSettings
                            }
                            sigil_runtime::ApplicationClientAction::OpenSupport => {
                                HttpApplicationClientAction::OpenSupport
                            }
                        }),
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                    })
                    .collect(),
                skills: view
                    .extension_catalog
                    .skills
                    .into_iter()
                    .map(|entry| HttpApplicationSkillCatalogEntry {
                        id: entry.id,
                        invocation_token: entry.invocation_token,
                        name: entry.name,
                        description: entry.description,
                        source: entry.source,
                        run_mode: entry.run_mode,
                        trust: entry.trust,
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                        binding: entry.binding.map(|binding| HttpApplicationSkillBinding {
                            skill_id: binding.skill_id,
                            skill_sha256: binding.skill_sha256,
                            index_fingerprint: binding.index_fingerprint,
                        }),
                    })
                    .collect(),
                agents: view
                    .extension_catalog
                    .agents
                    .into_iter()
                    .map(|entry| HttpApplicationAgentCatalogEntry {
                        id: entry.id,
                        invocation_token: entry.invocation_token,
                        description: entry.description,
                        source: entry.source,
                        kind: entry.kind,
                        trust: entry.trust,
                        enabled: entry.enabled,
                        user_invocable: entry.user_invocable,
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                        snapshot_id: entry.snapshot_id,
                        binding: entry
                            .binding
                            .map(|binding| crate::HttpApplicationAgentBinding {
                                profile_id: binding.profile_id,
                                snapshot_id: binding.snapshot_id,
                            }),
                    })
                    .collect(),
            },
            route_recovery: attachment_recovery.or_else(|| view.route_recovery.map(|recovery| {
                crate::HttpSessionRouteRecoveryView {
                    code: match recovery.code {
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired => crate::HttpSessionRouteRecoveryCode::SessionRouteConfirmationRequired,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::SessionRouteSelectionRequired => crate::HttpSessionRouteRecoveryCode::SessionRouteSelectionRequired,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::ModelRouteNotConfigured => crate::HttpSessionRouteRecoveryCode::ModelRouteNotConfigured,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::ConnectionConfigInvalid => crate::HttpSessionRouteRecoveryCode::ConnectionConfigInvalid,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::ProviderUnavailable => crate::HttpSessionRouteRecoveryCode::ProviderUnavailable,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::SessionAlreadyActive => crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::SessionWriterBusy => crate::HttpSessionRouteRecoveryCode::SessionWriterBusy,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryCode::SessionStreamInvalid => crate::HttpSessionRouteRecoveryCode::SessionStreamInvalid,
                    },
                    allowed_actions: recovery.allowed_actions.into_iter().map(|action| match action {
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::ConfirmCurrentRoute => crate::HttpSessionRouteRecoveryAction::ConfirmCurrentRoute,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::RepairConnection => crate::HttpSessionRouteRecoveryAction::RepairConnection,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::SelectReplacement => crate::HttpSessionRouteRecoveryAction::SelectReplacement,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::StartNewSession => crate::HttpSessionRouteRecoveryAction::StartNewSession,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::RetryProvider => crate::HttpSessionRouteRecoveryAction::RetryProvider,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::RetrySessionAttach => crate::HttpSessionRouteRecoveryAction::RetrySessionAttach,
                        sigil_runtime::application_run::ApplicationSessionRouteRecoveryAction::BackToSessionLibrary => crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
                    }).collect(),
                    recovery_binding: recovery.recovery_binding,
                    retryable: recovery.retryable,
                }
            })),
        })
    }

    fn agent_activity_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpAgentActivityView, HttpRunDriverError> {
        let view = application_agent_activity_view(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable agent activity projection failed"))?;
        Ok(HttpAgentActivityView {
            total_agents: view.total_agents,
            active_agents: view.active_agents,
            terminal_agents: view.terminal_agents,
            items: view
                .items
                .into_iter()
                .map(|item| HttpAgentActivityItem {
                    thread_id: item.thread_id,
                    profile_id: item.profile_id,
                    display_name: item.display_name,
                    objective: item.objective,
                    status: match item.status {
                        sigil_runtime::ApplicationAgentActivityStatus::Started => {
                            HttpAgentActivityStatus::Started
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Running => {
                            HttpAgentActivityStatus::Running
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Blocked => {
                            HttpAgentActivityStatus::Blocked
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Completed => {
                            HttpAgentActivityStatus::Completed
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Failed => {
                            HttpAgentActivityStatus::Failed
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Cancelled => {
                            HttpAgentActivityStatus::Cancelled
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Interrupted => {
                            HttpAgentActivityStatus::Interrupted
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Unavailable => {
                            HttpAgentActivityStatus::Unavailable
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Unknown => {
                            HttpAgentActivityStatus::Unknown
                        }
                    },
                    reason: item.reason,
                    handoff_status: match item.handoff_status {
                        sigil_runtime::ApplicationAgentHandoffStatus::Pending => {
                            HttpAgentHandoffStatus::Pending
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::ResultReady => {
                            HttpAgentHandoffStatus::ResultReady
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::ResultRead => {
                            HttpAgentHandoffStatus::ResultRead
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::Returned => {
                            HttpAgentHandoffStatus::Returned
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::Unavailable => {
                            HttpAgentHandoffStatus::Unavailable
                        }
                    },
                    result_summary: item.result_summary,
                    result_summary_truncated: item.result_summary_truncated,
                    usage: item.usage.map(|usage| HttpAgentUsageSummary {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        total_tokens: usage.total_tokens,
                        cached_tokens: usage.cached_tokens,
                    }),
                })
                .collect(),
        })
    }

    fn conversation_queue_view(
        &self,
        session: &crate::HttpSessionSnapshot,
        foreground_owner: Option<&crate::HttpForegroundRunOwner>,
    ) -> Result<HttpConversationQueueView, HttpConversationQueueDriverError> {
        let state = read_http_durable_queue_state(session)?;
        let exact_prompts = self
            .exact_queue_prompts
            .lock()
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        Ok(http_conversation_queue_view(
            session,
            foreground_owner,
            &state,
            &exact_prompts,
        ))
    }

    fn conversation_recovery_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpConversationRecoveryView, HttpConversationRecoveryDriverError> {
        application_conversation_recovery_view(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map(Into::into)
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)
    }

    fn conversation_compaction_review(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpCompactionReview, HttpConversationRecoveryDriverError> {
        let session_attachment = self
            .acquire_session_attachment(session)
            .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
        let (review, pending) = preview_application_compaction_with_attachment(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            session_attachment,
        )
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        let mut previews = self
            .pending_compactions
            .lock()
            .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        previews.retain(|_, item| item.session_scope_id() != session.durable_session_scope_id);
        if let Some(pending) = pending {
            if previews.len() >= MAX_HTTP_PENDING_COMPACTION_PREVIEWS {
                let oldest = previews.keys().next().cloned();
                if let Some(oldest) = oldest {
                    previews.remove(&oldest);
                }
            }
            previews.insert(
                pending.preview_id().to_owned(),
                PendingHttpCompaction::Local(Box::new(pending)),
            );
        }
        Ok(review.into())
    }

    fn checkpoint_restore_review(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &crate::HttpCheckpointRestoreRequest,
    ) -> Result<HttpCheckpointRestoreReview, HttpConversationRecoveryDriverError> {
        let recovery = self.conversation_recovery_view(session)?;
        if !recovery.checkpoints.iter().any(|checkpoint| {
            checkpoint.checkpoint_id == request.checkpoint_id
                && checkpoint.checkpoint_digest == request.checkpoint_digest
        }) {
            return Err(HttpConversationRecoveryDriverError::StaleBinding);
        }
        let workspace_root = application_recovery_workspace_root(
            &self.options.config_path,
            &self.options.launch_cwd,
        )
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        preview_application_checkpoint_restore(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &workspace_root,
            &request.into(),
        )
        .map(Into::into)
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)
    }

    fn mutate_conversation_recovery(
        &self,
        session: &crate::HttpSessionSnapshot,
        command: &HttpConversationRecoveryDriverCommand,
    ) -> Result<HttpConversationRecoveryDriverOutput, HttpConversationRecoveryDriverError> {
        let session_attachment = self
            .acquire_session_attachment(session)
            .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
        let mut compaction_receipt = None;
        let mut compaction_review = None;
        let mut tool_output_shrink = None;
        let mut restore_receipt = None;
        let mut fork_receipt = None;
        match &command.action {
            HttpConversationRecoveryCommandAction::PrepareCompaction { preview_id } => {
                let pending = self
                    .pending_compactions
                    .lock()
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?
                    .remove(preview_id)
                    .ok_or(HttpConversationRecoveryDriverError::StaleBinding)?;
                let PendingHttpCompaction::Local(pending) = pending else {
                    return Err(HttpConversationRecoveryDriverError::StaleBinding);
                };
                let (review, ready) = self
                    .runtime
                    .block_on(prepare_application_compaction_from_preview_with_attachment(
                        &self.options.config_path,
                        &self.options.launch_cwd,
                        Path::new(&session.session_log_path),
                        &session.durable_session_scope_id,
                        *pending,
                        Arc::clone(&session_attachment),
                    ))
                    .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
                if let Some(ready) = ready {
                    self.pending_compactions
                        .lock()
                        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?
                        .insert(
                            preview_id.clone(),
                            PendingHttpCompaction::Ready(Box::new(ready)),
                        );
                }
                compaction_review = Some(review.into());
            }
            HttpConversationRecoveryCommandAction::ApplyCompaction { preview_id } => {
                let pending = self
                    .pending_compactions
                    .lock()
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?
                    .remove(preview_id)
                    .ok_or(HttpConversationRecoveryDriverError::StaleBinding)?;
                let PendingHttpCompaction::Ready(pending) = pending else {
                    return Err(HttpConversationRecoveryDriverError::StaleBinding);
                };
                let receipt = self
                    .runtime
                    .block_on((*pending).apply_with_optional_native(
                        Path::new(&session.session_log_path),
                        &session.durable_session_scope_id,
                        preview_id,
                    ))
                    .map_err(|_| HttpConversationRecoveryDriverError::StaleBinding)?;
                compaction_receipt = Some(HttpCompactionReceipt {
                    compaction_id: receipt.compaction_id,
                    attempt_id: receipt.attempt_id,
                    task_memory_id: receipt.task_memory_id,
                    folded_event_count: receipt.folded_event_count,
                    tool_output_projection_recorded: receipt.tool_output_projection_recorded,
                    native_carrier_materialized: receipt.native_carrier_materialized,
                    native_carrier_status: receipt.native_carrier_status,
                });
            }
            HttpConversationRecoveryCommandAction::ApplyStandaloneToolOutputShrink {
                preview_id,
            } => {
                let pending = self
                    .pending_compactions
                    .lock()
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?
                    .remove(preview_id)
                    .ok_or(HttpConversationRecoveryDriverError::StaleBinding)?;
                let PendingHttpCompaction::Local(pending) = pending else {
                    return Err(HttpConversationRecoveryDriverError::StaleBinding);
                };
                let receipt = (*pending)
                    .apply_standalone_tool_output_shrink(
                        Path::new(&session.session_log_path),
                        &session.durable_session_scope_id,
                        preview_id,
                    )
                    .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
                tool_output_shrink = Some(HttpToolOutputShrinkReceipt {
                    context_epoch_id: receipt.context_epoch_id,
                    projected_output_count: receipt.projected_output_count,
                });
            }
            HttpConversationRecoveryCommandAction::RestoreCheckpoint {
                checkpoint_id,
                checkpoint_digest,
            } => {
                let review = self.checkpoint_restore_review(
                    session,
                    &crate::HttpCheckpointRestoreRequest {
                        checkpoint_id: checkpoint_id.clone(),
                        checkpoint_digest: checkpoint_digest.clone(),
                    },
                )?;
                if !review.ready {
                    return Err(HttpConversationRecoveryDriverError::Conflict);
                }
                let workspace_root = application_recovery_workspace_root(
                    &self.options.config_path,
                    &self.options.launch_cwd,
                )
                .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
                let output = restore_application_checkpoint(
                    Path::new(&session.session_log_path),
                    &session.durable_session_scope_id,
                    &workspace_root,
                    &sigil_kernel::ControlledCheckpointRestoreRequest {
                        checkpoint_id: checkpoint_id.clone(),
                        checkpoint_digest: checkpoint_digest.clone(),
                    },
                )
                .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
                restore_receipt = Some(HttpCheckpointRestoreReceipt {
                    checkpoint_id: output.preview.checkpoint_id,
                    batch_id: output.batch_id,
                    restored_file_count: output.restored.len(),
                    verification_stale: true,
                });
            }
            HttpConversationRecoveryCommandAction::ForkConversation {
                source_turn_digest,
                model_ref,
            } => {
                let recovery = self.conversation_recovery_view(session)?;
                if !recovery
                    .fork_points
                    .iter()
                    .any(|point| point.source_turn_digest == *source_turn_digest)
                {
                    return Err(HttpConversationRecoveryDriverError::StaleBinding);
                }
                let lifecycle = self
                    .options
                    .session_lifecycle
                    .as_ref()
                    .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
                let source_ref = SessionRef::new_relative(
                    Path::new(&session.session_log_path)
                        .file_name()
                        .ok_or(HttpConversationRecoveryDriverError::Unavailable)?,
                )
                .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
                let root_config = RootConfig::load(&self.options.config_path)
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
                let target_model_ref = sigil_kernel::ModelRef::new(
                    sigil_kernel::ConnectionId::new(model_ref.connection_id.clone())
                        .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?,
                    model_ref.model_id.clone(),
                )
                .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
                let output = lifecycle
                    .fork_session_at_turn(
                        &source_ref,
                        &session.durable_session_scope_id,
                        source_turn_digest,
                        &format!("{}:{}", command.client_id, command.command_id),
                        &root_config,
                        &target_model_ref,
                    )
                    .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
                fork_receipt = Some(HttpConversationForkReceipt {
                    session_ref: output
                        .destination_session_ref
                        .as_path()
                        .to_string_lossy()
                        .into_owned(),
                    session_id: output.destination_session_id,
                    copied_message_count: output.copied_message_count,
                    copied_external_provenance_count: output.copied_external_provenance_count,
                });
            }
        }
        let recovery = application_conversation_recovery_view(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map(Into::into)
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        Ok(HttpConversationRecoveryDriverOutput {
            compaction: compaction_receipt,
            compaction_review,
            tool_output_shrink,
            restore: restore_receipt,
            fork: fork_receipt,
            recovery,
        })
    }

    fn mutate_conversation_queue(
        &self,
        session: &crate::HttpSessionSnapshot,
        foreground_owner: Option<&crate::HttpForegroundRunOwner>,
        command: &HttpConversationQueueDriverCommand,
    ) -> Result<HttpConversationQueueView, HttpConversationQueueDriverError> {
        self.acquire_session_attachment(session)
            .map_err(|error| match error {
                HttpRunAdmissionError::SessionAlreadyActive { .. }
                | HttpRunAdmissionError::RouteRecovery(_) => {
                    HttpConversationQueueDriverError::Conflict
                }
                HttpRunAdmissionError::Unavailable => HttpConversationQueueDriverError::Unavailable,
            })?;
        let state = read_http_durable_queue_state(session)?;
        let current_generation = http_queue_generation(state.projection.current_revision());
        if command.request.expected_generation != current_generation {
            return Err(HttpConversationQueueDriverError::StaleGeneration);
        }
        if let HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } = &command.request.action
        {
            let owner = foreground_owner.ok_or(HttpConversationQueueDriverError::OwnerLost)?;
            if owner.run_id != *foreground_run_id
                || owner.owner_revision != *foreground_owner_revision
            {
                return Err(HttpConversationQueueDriverError::OwnerLost);
            }
            let exact_prompts = self
                .exact_queue_prompts
                .lock()
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            validate_http_interrupt_candidate(session, &state, &exact_prompts)?;
            return Ok(http_conversation_queue_view(
                session,
                foreground_owner,
                &state,
                &exact_prompts,
            ));
        }

        let now_ms = current_unix_time_ms();
        let expected_queue_revision = state.projection.current_revision();
        let mut cache_update = None;
        let mutation = match &command.request.action {
            HttpConversationQueueCommandAction::Enqueue {
                prompt,
                kind,
                reasoning_effort,
            } => {
                let queue_id = stable_http_queue_id(
                    &session.durable_session_scope_id,
                    &command.client_id,
                    &command.command_id,
                )?;
                let projection = project_conversation_prompt_for_persistence(prompt);
                cache_update = Some(HttpExactQueueCacheUpdate::Replace {
                    key: exact_queue_prompt_key(session, queue_id.clone()),
                    prompt_hash: projection.prompt_hash.clone(),
                    exact_prompt: projection
                        .exact_prompt_required
                        .then(|| SecretString::new(prompt.clone())),
                });
                ConversationQueueMutation::Enqueue {
                    entry: ConversationInputQueuedEntry {
                        queue_id,
                        target: ConversationInputTarget::MainThread,
                        kind: http_queue_kind_to_kernel(*kind),
                        prompt_hash: projection.prompt_hash,
                        prompt: projection.safe_prompt,
                        reasoning_effort: reasoning_effort.map(Into::into),
                        created_at_ms: Some(now_ms),
                    },
                }
            }
            HttpConversationQueueCommandAction::Edit {
                entry_id,
                prompt,
                reasoning_effort,
            } => {
                let queue_id = ConversationInputQueueId::new(entry_id.clone())
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                ensure_http_queue_item_mutable(&state.projection, &queue_id)?;
                let projection = project_conversation_prompt_for_persistence(prompt);
                cache_update = Some(HttpExactQueueCacheUpdate::Replace {
                    key: exact_queue_prompt_key(session, queue_id.clone()),
                    prompt_hash: projection.prompt_hash.clone(),
                    exact_prompt: projection
                        .exact_prompt_required
                        .then(|| SecretString::new(prompt.clone())),
                });
                ConversationQueueMutation::Edit {
                    entry: ConversationInputEditedEntry {
                        queue_id,
                        prompt_hash: projection.prompt_hash,
                        prompt: projection.safe_prompt,
                        reasoning_effort: reasoning_effort.map(Into::into),
                        updated_at_ms: Some(now_ms),
                    },
                }
            }
            HttpConversationQueueCommandAction::Remove { entry_id } => {
                let queue_id = ConversationInputQueueId::new(entry_id.clone())
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                ensure_http_queue_item_mutable(&state.projection, &queue_id)?;
                cache_update = Some(HttpExactQueueCacheUpdate::Remove(exact_queue_prompt_key(
                    session,
                    queue_id.clone(),
                )));
                ConversationQueueMutation::Remove {
                    queue_id,
                    reason: Some("removed by application queue command".to_owned()),
                    updated_at_ms: Some(now_ms),
                }
            }
            HttpConversationQueueCommandAction::Reorder {
                entry_id,
                after_entry_id,
            } => {
                let queue_id = ConversationInputQueueId::new(entry_id.clone())
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                ensure_http_queue_item_mutable(&state.projection, &queue_id)?;
                let after_queue_id = after_entry_id
                    .as_ref()
                    .map(|entry_id| ConversationInputQueueId::new(entry_id.clone()))
                    .transpose()
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                if let Some(after_queue_id) = after_queue_id.as_ref() {
                    ensure_http_queue_item_mutable(&state.projection, after_queue_id)?;
                }
                ConversationQueueMutation::Reorder {
                    entry: ConversationInputReorderedEntry {
                        queue_id,
                        after_queue_id,
                        updated_at_ms: Some(now_ms),
                    },
                }
            }
            HttpConversationQueueCommandAction::Pause => ConversationQueueMutation::Pause {
                reason: Some("paused by application queue command".to_owned()),
                updated_at_ms: Some(now_ms),
            },
            HttpConversationQueueCommandAction::Resume => ConversationQueueMutation::Resume {
                reason: Some("resumed by application queue command".to_owned()),
                updated_at_ms: Some(now_ms),
            },
            HttpConversationQueueCommandAction::InterruptAndRunNext { .. } => {
                unreachable!("interrupt action returned after exact owner validation")
            }
        };

        let mut exact_prompts = self
            .exact_queue_prompts
            .lock()
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        validate_http_exact_queue_cache_capacity(&exact_prompts, cache_update.as_ref())?;
        let store = JsonlSessionStore::new(&session.session_log_path)
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        if store
            .append_conversation_queue_mutation(ConversationQueueMutationCommand {
                expected_queue_revision,
                mutation,
            })
            .is_err()
        {
            let latest = read_http_durable_queue_state(session)?;
            return if http_queue_generation(latest.projection.current_revision())
                != current_generation
            {
                Err(HttpConversationQueueDriverError::StaleGeneration)
            } else {
                Err(HttpConversationQueueDriverError::Conflict)
            };
        }
        apply_http_exact_queue_cache_update(&mut exact_prompts, cache_update);
        let state = read_http_durable_queue_state(session)?;
        Ok(http_conversation_queue_view(
            session,
            foreground_owner,
            &state,
            &exact_prompts,
        ))
    }

    fn next_queued_run_admission(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<HttpQueuedRunAdmission>, HttpConversationQueueDriverError> {
        self.acquire_session_attachment(session)
            .map_err(|error| match error {
                HttpRunAdmissionError::SessionAlreadyActive { .. }
                | HttpRunAdmissionError::RouteRecovery(_) => {
                    HttpConversationQueueDriverError::Conflict
                }
                HttpRunAdmissionError::Unavailable => HttpConversationQueueDriverError::Unavailable,
            })?;
        self.reconcile_orphaned_queued_dispatches(session)?;
        let state = read_http_durable_queue_state(session)?;
        if state
            .projection
            .queue
            .items
            .iter()
            .any(|item| item.status == ConversationInputStatus::Dispatching)
        {
            return Ok(None);
        }
        let exact_prompts = self
            .exact_queue_prompts
            .lock()
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        let view = http_conversation_queue_view(session, None, &state, &exact_prompts);
        let Some(entry_id) = view.next_dispatchable_entry_id.as_deref() else {
            return Ok(None);
        };
        let item = state
            .projection
            .queue
            .items
            .iter()
            .find(|item| item.queued.queue_id.as_str() == entry_id)
            .ok_or(HttpConversationQueueDriverError::Conflict)?;
        if item.status != ConversationInputStatus::Queued
            || item.queued.kind != ConversationInputKind::Chat
            || item.queued.target != ConversationInputTarget::MainThread
        {
            return Ok(None);
        }
        let context = application_run_context_view(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        let dispatch_run_id = stable_http_queued_dispatch_run_id(
            &session.durable_session_scope_id,
            &item.queued.queue_id,
            &state.projection.current_revision(),
        );
        let prompt_preview = view
            .items
            .iter()
            .find(|row| row.entry_id == entry_id)
            .map(|row| row.prompt_preview.clone())
            .ok_or(HttpConversationQueueDriverError::Conflict)?;
        Ok(Some(HttpQueuedRunAdmission {
            entry_id: entry_id.to_owned(),
            generation: view.generation,
            dispatch_run_id,
            prompt_preview,
            permission_mode: context.default_permission_mode.into(),
            reasoning_effort: item.queued.reasoning_effort.clone().map(Into::into),
        }))
    }

    fn start_queued_run(&self, start: HttpQueuedRunDriverStart) -> Result<(), HttpRunDriverError> {
        self.acquire_session_attachment(&start.session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let (start, queued) = self.queued_supervisor_start(start)?;
        self.start_supervised_run(start, Some(queued), None)
    }

    fn wait_for_run_release(
        &self,
        run_id: &str,
        timeout: Duration,
    ) -> Result<(), HttpRunDriverError> {
        let deadline = Instant::now() + timeout;
        let mut runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        while runs.contains_key(run_id) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HttpRunDriverError::new(format!(
                    "production run cleanup timed out: {run_id}"
                )));
            }
            let (next, wait) = self
                .active_runs_ready
                .wait_timeout(runs, remaining)
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            runs = next;
            if wait.timed_out() && runs.contains_key(run_id) {
                return Err(HttpRunDriverError::new(format!(
                    "production run cleanup timed out: {run_id}"
                )));
            }
        }
        Ok(())
    }

    fn rerun_verification(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpVerificationRerunRequest,
    ) -> Result<HttpVerificationView, HttpRunDriverError> {
        let session_attachment = self
            .acquire_session_attachment(session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        self.runtime
            .block_on(rerun_application_verification_with_attachment(
                &self.options.config_path,
                &self.options.launch_cwd,
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &self.services,
                request,
                Some(session_attachment),
            ))
            .map_err(|error| HttpRunDriverError::new(format!("verification rerun failed: {error}")))
    }

    fn task_integration_review(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<HttpTaskIntegrationReviewView>, HttpRunDriverError> {
        application_task_integration_review_view(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map(|review| review.map(Into::into))
        .map_err(|error| {
            HttpRunDriverError::new(format!("Task integration review failed: {error}"))
        })
    }

    fn accept_task_integration(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpTaskIntegrationReviewRequest,
    ) -> Result<HttpTaskIntegrationAcceptanceView, HttpRunDriverError> {
        let session_attachment = self
            .acquire_session_attachment(session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        self.runtime
            .block_on(accept_application_task_integration_review_with_attachment(
                &self.options.config_path,
                &self.options.launch_cwd,
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &self.services,
                request,
                Some(session_attachment),
            ))
            .map(Into::into)
            .map_err(|error| {
                HttpRunDriverError::new(format!("Task integration acceptance failed: {error}"))
            })
    }

    fn plan_decision(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpPlanDecisionRequest,
    ) -> Result<HttpPlanDecisionCommandReceipt, HttpRunDriverError> {
        let session_attachment = self
            .acquire_session_attachment(session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let _attachment = session_attachment;
        let command: sigil_runtime::ApplicationPlanDecisionCommand = request.clone().into();
        let root_config =
            sigil_kernel::RootConfig::load(&self.options.config_path).map_err(|error| {
                HttpRunDriverError::new(format!("plan decision config failed: {error}"))
            })?;
        // Resolve the same workspace root the display projection uses so stale evaluation and
        // Task promotion bind the identical snapshot.
        let workspace_root = sigil_kernel::resolve_workspace_root(
            &self.options.config_path,
            &self.options.launch_cwd,
            &root_config.workspace.root,
        );
        let receipt = sigil_runtime::application_plan_decision(
            &root_config,
            &workspace_root,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &command,
        )
        .map_err(|error| HttpRunDriverError::new(format!("plan decision failed: {error}")))?;
        let revision_request = receipt.revision_request.clone();
        let mut http_receipt: HttpPlanDecisionCommandReceipt = receipt.into();
        http_receipt.session_id = session.durable_session_scope_id.clone();
        if let Some(revision_request) = revision_request
            && let Err(error) = self.spawn_plan_review_revision(
                session,
                &root_config,
                &workspace_root,
                revision_request,
            )
        {
            // `application_plan_decision` already persisted `RevisionRequested`; record a
            // durable recoverable failure so the original plan remains actionable instead of
            // being stuck behind a decision that can never complete.
            if let Err(record_error) = sigil_runtime::application_record_revision_failure(
                &root_config,
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &request.plan_id,
                &request.expected_plan_hash,
                &format!("revision spawn failed: {error}"),
            ) {
                return Err(HttpRunDriverError::new(format!(
                    "plan review revision spawn failed ({error}) and its durable failure record also failed ({record_error:#})"
                )));
            }
            return Err(error);
        }
        Ok(http_receipt)
    }

    fn plan_review_detail(
        &self,
        session: &crate::HttpSessionSnapshot,
        plan_id: &str,
        expected_plan_hash: &str,
    ) -> Result<HttpPlanReviewDetail, HttpRunDriverError> {
        let plan_id = sigil_kernel::PlanId::new(plan_id.to_owned())
            .map_err(|error| HttpRunDriverError::new(format!("invalid plan id: {error}")))?;
        let entries = sigil_kernel::JsonlSessionStore::read_entries(&session.session_log_path)
            .map_err(|error| {
                HttpRunDriverError::new(format!("plan detail read failed: {error}"))
            })?;
        sigil_kernel::plan_review_detail_from_entries(&entries, &plan_id, expected_plan_hash)
            .map_err(|error| {
                HttpRunDriverError::new(format!("plan detail projection failed: {error:#}"))
            })
    }

    fn user_input_request(
        &self,
        session: &crate::HttpSessionSnapshot,
        request_id: &str,
        generation: u32,
        expected_request_hash: &str,
    ) -> Result<HttpUserInputRequest, HttpRunDriverError> {
        application_user_input_request_view_by_key(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            request_id,
            generation,
            expected_request_hash,
        )
        .map(Into::into)
        .map_err(|error| HttpRunDriverError::new(format!("user input detail failed: {error:#}")))
    }

    fn has_unresolved_user_input(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<bool, HttpRunDriverError> {
        application_session_has_unresolved_user_input(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|error| {
            HttpRunDriverError::new(format!("user input admission projection failed: {error:#}"))
        })
    }

    fn user_input_decision(
        &self,
        session: &crate::HttpSessionSnapshot,
        command: &HttpUserInputDecisionDriverCommand,
    ) -> Result<HttpUserInputDecisionCommandReceipt, HttpRunDriverError> {
        let attachment = self
            .acquire_session_attachment(session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let exact = application_user_input_request_view_by_key(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &command.request_id,
            command.request.generation,
            &command.request.expected_request_hash,
        )
        .map_err(|error| {
            HttpRunDriverError::stale_user_input(format!("user input decision is stale: {error:#}"))
        })?;
        let run_id = sigil_kernel::user_input_continuation_logical_run_id(
            &exact.identity,
            &exact.request_hash,
        )
        .map_err(|error| HttpRunDriverError::new(format!("user input run id failed: {error}")))?
        .as_str()
        .to_owned();
        let registry = self.attached_registry()?;
        let services = self
            .services
            .clone()
            .with_terminal_lifecycle_handler(Arc::new(HttpProductionTerminalLifecycleHandler {
                durable_session_scope_id: session.durable_session_scope_id.clone(),
                run_id: run_id.clone(),
                registry: Arc::downgrade(&registry),
                event_bus: Arc::clone(&self.event_bus),
                terminal_owners: Arc::clone(&self.terminal_owners),
            }));
        let prepared = self
            .runtime
            .block_on(self.preparer.prepare_user_input(
                ApplicationUserInputDecisionRequest {
                    config_path: self.options.config_path.clone(),
                    launch_cwd: self.options.launch_cwd.clone(),
                    session_path: PathBuf::from(&session.session_log_path),
                    session_attachment: Some(attachment),
                    expected_session_scope_id: session.durable_session_scope_id.clone(),
                    run_id: run_id.clone(),
                    identity: exact.identity,
                    request_hash: exact.request_hash,
                    command_id:
                        sigil_kernel::UserInputCommandId::new(command.command_id.clone()).map_err(
                            |error| {
                                HttpRunDriverError::new(format!(
                                    "user input command id failed: {error}"
                                ))
                            },
                        )?,
                    decision: command.request.decision.clone(),
                    interaction: ApplicationRunInteraction::ExternallyInteractive,
                    permission_mode: command.request.permission_mode.map(Into::into),
                },
                services,
            ))
            .map_err(|error| {
                HttpRunDriverError::new(format!("user input decision failed: {error:#}"))
            })?;
        let (receipt, continuation, revision_request) = prepared.into_parts();
        let plan_review_research_resume = matches!(
            &receipt.request.source,
            sigil_kernel::UserInputSourceV1::PlanReviewResearch { .. }
        );
        let continuation_run_id = continuation.as_ref().map(|_| run_id.clone()).or_else(|| {
            revision_request.as_ref().map(|request| {
                if plan_review_research_resume {
                    run_id.clone()
                } else {
                    request.child_logical_run_id()
                }
            })
        });
        if let Some(continuation) = continuation {
            let permission_mode = command
                .request
                .permission_mode
                .unwrap_or(HttpPermissionMode::Manual);
            let run = registry
                .register_supervised_session_run(
                    &session.id,
                    &run_id,
                    permission_mode,
                    "Continue after answering a requested question",
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
            let start = HttpRunDriverStart {
                session: session.clone(),
                run,
                prompt: "Continue after answering a requested question".to_owned(),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            };
            if let Err(error) = self.start_supervised_run(
                start,
                None,
                Some(HttpPreparedApplicationRun::Conversation(Box::new(
                    continuation,
                ))),
            ) {
                registry.rollback_supervised_session_run_registration(&session.id, &run_id);
                return Err(error);
            }
        }
        if let Some(revision_request) = revision_request {
            let root_config =
                sigil_kernel::RootConfig::load(&self.options.config_path).map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "plan revision guidance config failed: {error}"
                    ))
                })?;
            let workspace_root = sigil_kernel::resolve_workspace_root(
                &self.options.config_path,
                &self.options.launch_cwd,
                &root_config.workspace.root,
            );
            if let Err(error) = self.spawn_plan_review_revision(
                session,
                &root_config,
                &workspace_root,
                revision_request,
            ) {
                if let sigil_kernel::UserInputSourceV1::PlanRevision {
                    base_plan_id,
                    base_plan_hash,
                } = &receipt.request.source
                    && let Err(recovery_error) = sigil_runtime::application_record_revision_failure(
                        &root_config,
                        Path::new(&session.session_log_path),
                        &session.durable_session_scope_id,
                        base_plan_id.as_str(),
                        base_plan_hash,
                        &format!("revision spawn failed: {error}"),
                    )
                {
                    return Err(HttpRunDriverError::new(format!(
                        "{error}; revision recovery failed: {recovery_error:#}"
                    )));
                }
                return Err(error);
            }
        }
        Ok(HttpUserInputDecisionCommandReceipt {
            command_id: command.command_id.clone(),
            client_id: command.client_id.clone(),
            session_id: session.durable_session_scope_id.clone(),
            request: receipt.request.into(),
            continuation_run_id,
            replayed: receipt.idempotent_replay,
        })
    }

    fn wait_for_idle(&self, timeout: Duration) -> Result<(), HttpRunDriverError> {
        self.cancel_owned_terminal_tasks(None)?;
        let deadline = Instant::now() + timeout;
        let mut runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        while !runs.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HttpRunDriverError::new(format!(
                    "production shutdown timed out with {} owned run supervisor(s)",
                    runs.len()
                )));
            }
            let (next, wait) = self
                .active_runs_ready
                .wait_timeout(runs, remaining)
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            runs = next;
            if wait.timed_out() && !runs.is_empty() {
                return Err(HttpRunDriverError::new(format!(
                    "production shutdown timed out with {} owned run supervisor(s)",
                    runs.len()
                )));
            }
        }
        Ok(())
    }
}

struct HttpDurableQueueState {
    projection: ConversationQueueDurableProjection,
    updated_at_ms: BTreeMap<ConversationInputQueueId, u64>,
}

enum HttpExactQueueCacheUpdate {
    Replace {
        key: HttpExactQueuePromptKey,
        prompt_hash: String,
        exact_prompt: Option<SecretString>,
    },
    Remove(HttpExactQueuePromptKey),
}

fn read_http_durable_queue_state(
    session: &crate::HttpSessionSnapshot,
) -> Result<HttpDurableQueueState, HttpConversationQueueDriverError> {
    let records = JsonlSessionStore::read_event_records(&session.session_log_path)
        .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
    if records
        .iter()
        .any(|record| record.session_id() != session.durable_session_scope_id)
    {
        return Err(HttpConversationQueueDriverError::Unavailable);
    }
    let projection = ConversationQueueDurableProjection::from_records(&records)
        .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
    let mut updated_at_ms = BTreeMap::new();
    for record in records {
        let Some(value) = record
            .stored_event()
            .payload
            .get("session_log_entry")
            .cloned()
        else {
            continue;
        };
        let Ok(SessionLogEntry::Control(control)) = serde_json::from_value(value) else {
            continue;
        };
        let update = match control {
            ControlEntry::ConversationInputQueued(entry) => {
                entry.created_at_ms.map(|time| (entry.queue_id, time))
            }
            ControlEntry::ConversationInputEdited(entry) => {
                entry.updated_at_ms.map(|time| (entry.queue_id, time))
            }
            ControlEntry::ConversationInputReordered(entry) => {
                entry.updated_at_ms.map(|time| (entry.queue_id, time))
            }
            ControlEntry::ConversationInputStatusChanged(entry) => {
                entry.updated_at_ms.map(|time| (entry.queue_id, time))
            }
            _ => None,
        };
        if let Some((queue_id, time)) = update {
            updated_at_ms.insert(queue_id, time);
        }
    }
    Ok(HttpDurableQueueState {
        projection,
        updated_at_ms,
    })
}

fn http_conversation_queue_view(
    session: &crate::HttpSessionSnapshot,
    foreground_owner: Option<&crate::HttpForegroundRunOwner>,
    state: &HttpDurableQueueState,
    exact_prompts: &BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
) -> HttpConversationQueueView {
    let next_dispatchable = state.projection.queue.next_dispatchable.as_ref();
    let has_dispatching_frontier = state
        .projection
        .queue
        .items
        .iter()
        .any(|item| item.status == ConversationInputStatus::Dispatching);
    let total_items = state.projection.queue.items.len();
    let items = state
        .projection
        .queue
        .items
        .iter()
        .take(crate::HTTP_MAX_CONVERSATION_QUEUE_ITEMS)
        .enumerate()
        .map(|(index, item)| {
            let key = exact_queue_prompt_key(session, item.queued.queue_id.clone());
            let prompt_material = if item
                .queued
                .prompt_hash
                .starts_with(CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX)
            {
                if exact_prompts
                    .get(&key)
                    .is_some_and(|material| material.prompt_hash == item.queued.prompt_hash)
                {
                    HttpConversationQueuePromptMaterial::AvailableProcessLocal
                } else {
                    HttpConversationQueuePromptMaterial::RequiresReentry
                }
            } else {
                HttpConversationQueuePromptMaterial::PersistedSafe
            };
            let is_supported = item.queued.target == ConversationInputTarget::MainThread
                && item.queued.kind == ConversationInputKind::Chat;
            let is_next = next_dispatchable == Some(&item.queued.queue_id);
            let dispatchable = item.status == ConversationInputStatus::Queued
                && is_supported
                && is_next
                && !has_dispatching_frontier
                && !state.projection.queue.paused
                && foreground_owner.is_none()
                && prompt_material != HttpConversationQueuePromptMaterial::RequiresReentry;
            let blocked_reason = if item.status == ConversationInputStatus::Stale {
                Some(HttpConversationQueueBlockedReason::Stale)
            } else if item.status.is_terminal() {
                Some(HttpConversationQueueBlockedReason::Terminal)
            } else if item.status == ConversationInputStatus::Dispatching {
                Some(HttpConversationQueueBlockedReason::Conflict)
            } else if !is_supported {
                Some(HttpConversationQueueBlockedReason::UnsupportedTarget)
            } else if state.projection.queue.paused {
                Some(HttpConversationQueueBlockedReason::QueuePaused)
            } else if prompt_material == HttpConversationQueuePromptMaterial::RequiresReentry {
                Some(HttpConversationQueueBlockedReason::RequiresReentry)
            } else if item.status == ConversationInputStatus::Queued
                && (has_dispatching_frontier || !is_next)
            {
                Some(HttpConversationQueueBlockedReason::WaitingForTerminalFrontier)
            } else if foreground_owner.is_some() {
                Some(HttpConversationQueueBlockedReason::ForegroundRunActive)
            } else {
                None
            };
            let (prompt_preview, prompt_preview_truncated) =
                http_queue_prompt_preview(&item.queued.prompt);
            HttpConversationQueueItem {
                entry_id: item.queued.queue_id.as_str().to_owned(),
                order: u32::try_from(index).unwrap_or(u32::MAX),
                kind: kernel_queue_kind_to_http(item.queued.kind),
                status: kernel_queue_status_to_http(item.status),
                prompt_preview,
                prompt_preview_truncated,
                prompt_material,
                dispatchable,
                blocked_reason,
                created_at_ms: item.queued.created_at_ms,
                updated_at_ms: state.updated_at_ms.get(&item.queued.queue_id).copied(),
            }
        })
        .collect::<Vec<_>>();
    let next_dispatchable_entry_id = items
        .iter()
        .find(|item| item.dispatchable)
        .map(|item| item.entry_id.clone());
    HttpConversationQueueView {
        schema_version: crate::HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION,
        session_id: session.id.clone(),
        generation: http_queue_generation(state.projection.current_revision()),
        paused: state.projection.queue.paused,
        total_items: u32::try_from(total_items).unwrap_or(u32::MAX),
        items,
        truncated: total_items > crate::HTTP_MAX_CONVERSATION_QUEUE_ITEMS,
        next_dispatchable_entry_id,
    }
}

fn exact_queue_prompt_key(
    session: &crate::HttpSessionSnapshot,
    queue_id: ConversationInputQueueId,
) -> HttpExactQueuePromptKey {
    HttpExactQueuePromptKey {
        session_scope_id: session.durable_session_scope_id.clone(),
        queue_id,
    }
}

fn stable_http_queue_id(
    session_scope_id: &str,
    client_id: &str,
    command_id: &str,
) -> Result<ConversationInputQueueId, HttpConversationQueueDriverError> {
    ConversationInputQueueId::new(stable_event_uuid(
        "sigil-http-conversation-queue-entry",
        &stable_http_identity_seed(&[session_scope_id, client_id, command_id]),
    ))
    .map_err(|_| HttpConversationQueueDriverError::Conflict)
}

fn stable_http_queued_dispatch_run_id(
    session_scope_id: &str,
    queue_id: &ConversationInputQueueId,
    revision: &ConversationQueueRevision,
) -> String {
    stable_event_uuid(
        "sigil-http-conversation-queue-dispatch",
        &stable_http_identity_seed(&[
            session_scope_id,
            queue_id.as_str(),
            &revision.stream_sequence.to_string(),
            &revision.event_id,
        ]),
    )
}

fn stable_http_identity_seed(parts: &[&str]) -> String {
    use std::fmt::Write as _;

    let mut seed = String::new();
    for part in parts {
        write!(&mut seed, "{}:{part}", part.len())
            .expect("writing a stable identity seed into String cannot fail");
    }
    seed
}

fn http_queue_generation(revision: ConversationQueueRevision) -> HttpConversationQueueGeneration {
    let mut hasher = Sha256::new();
    for part in [
        revision.stream_sequence.to_be_bytes().as_slice(),
        revision.event_id.as_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    HttpConversationQueueGeneration(format!("queue-v1:{:x}", hasher.finalize()))
}

fn http_queue_prompt_preview(prompt: &str) -> (String, bool) {
    let truncated = prompt.chars().count() > MAX_HTTP_QUEUE_PREVIEW_CHARS;
    if !truncated {
        return (prompt.to_owned(), false);
    }
    let preview = prompt
        .chars()
        .take(MAX_HTTP_QUEUE_PREVIEW_CHARS.saturating_sub(3))
        .collect::<String>();
    (format!("{preview}..."), true)
}

fn http_queue_kind_to_kernel(kind: HttpConversationQueueItemKind) -> ConversationInputKind {
    match kind {
        HttpConversationQueueItemKind::Chat => ConversationInputKind::Chat,
        HttpConversationQueueItemKind::PlanPrompt => ConversationInputKind::PlanPrompt,
        HttpConversationQueueItemKind::AgentMention => ConversationInputKind::AgentMention,
        HttpConversationQueueItemKind::AgentMessage => ConversationInputKind::AgentMessage,
        HttpConversationQueueItemKind::Unknown => ConversationInputKind::Unknown,
    }
}

fn kernel_queue_kind_to_http(kind: ConversationInputKind) -> HttpConversationQueueItemKind {
    match kind {
        ConversationInputKind::Chat => HttpConversationQueueItemKind::Chat,
        ConversationInputKind::PlanPrompt => HttpConversationQueueItemKind::PlanPrompt,
        ConversationInputKind::AgentMention => HttpConversationQueueItemKind::AgentMention,
        ConversationInputKind::AgentMessage => HttpConversationQueueItemKind::AgentMessage,
        ConversationInputKind::TaskGuidance => HttpConversationQueueItemKind::Unknown,
        ConversationInputKind::Unknown => HttpConversationQueueItemKind::Unknown,
    }
}

fn kernel_queue_status_to_http(status: ConversationInputStatus) -> HttpConversationQueueItemStatus {
    match status {
        ConversationInputStatus::Queued => HttpConversationQueueItemStatus::Queued,
        ConversationInputStatus::Dispatching => HttpConversationQueueItemStatus::Dispatching,
        ConversationInputStatus::Delivered => HttpConversationQueueItemStatus::Delivered,
        ConversationInputStatus::Rejected => HttpConversationQueueItemStatus::Rejected,
        ConversationInputStatus::Cancelled => HttpConversationQueueItemStatus::Cancelled,
        ConversationInputStatus::Stale => HttpConversationQueueItemStatus::Stale,
        ConversationInputStatus::Unknown => HttpConversationQueueItemStatus::Unknown,
    }
}

fn ensure_http_queue_item_mutable(
    projection: &ConversationQueueDurableProjection,
    queue_id: &ConversationInputQueueId,
) -> Result<(), HttpConversationQueueDriverError> {
    let Some(item) = projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
    else {
        return if projection.is_terminal_queue_id(queue_id) {
            Err(HttpConversationQueueDriverError::Terminal)
        } else {
            Err(HttpConversationQueueDriverError::Conflict)
        };
    };
    if item.status.is_terminal() {
        return Err(HttpConversationQueueDriverError::Terminal);
    }
    if item.status != ConversationInputStatus::Queued {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    Ok(())
}

fn validate_http_interrupt_candidate(
    session: &crate::HttpSessionSnapshot,
    state: &HttpDurableQueueState,
    exact_prompts: &BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
) -> Result<(), HttpConversationQueueDriverError> {
    if state.projection.queue.paused {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    let queue_id = state
        .projection
        .queue
        .next_dispatchable
        .as_ref()
        .ok_or(HttpConversationQueueDriverError::Conflict)?;
    let item = state
        .projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
        .ok_or(HttpConversationQueueDriverError::Conflict)?;
    if item.status != ConversationInputStatus::Queued {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    if item.queued.target != ConversationInputTarget::MainThread
        || item.queued.kind != ConversationInputKind::Chat
    {
        return Err(HttpConversationQueueDriverError::Unsupported);
    }
    if item
        .queued
        .prompt_hash
        .starts_with(CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX)
    {
        let key = exact_queue_prompt_key(session, queue_id.clone());
        if exact_prompts
            .get(&key)
            .is_none_or(|material| material.prompt_hash != item.queued.prompt_hash)
        {
            return Err(HttpConversationQueueDriverError::RequiresReentry);
        }
    }
    Ok(())
}

fn validate_http_exact_queue_cache_capacity(
    cache: &BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
    update: Option<&HttpExactQueueCacheUpdate>,
) -> Result<(), HttpConversationQueueDriverError> {
    let Some(HttpExactQueueCacheUpdate::Replace {
        key,
        exact_prompt: Some(_),
        ..
    }) = update
    else {
        return Ok(());
    };
    if !cache.contains_key(key) && cache.len() >= MAX_HTTP_EXACT_QUEUE_PROMPTS {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    Ok(())
}

fn apply_http_exact_queue_cache_update(
    cache: &mut BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
    update: Option<HttpExactQueueCacheUpdate>,
) {
    match update {
        Some(HttpExactQueueCacheUpdate::Replace {
            key,
            prompt_hash,
            exact_prompt: Some(exact_prompt),
        }) => {
            cache.insert(
                key,
                HttpExactQueuePrompt {
                    prompt_hash,
                    exact_prompt,
                },
            );
        }
        Some(HttpExactQueueCacheUpdate::Replace {
            key,
            exact_prompt: None,
            ..
        })
        | Some(HttpExactQueueCacheUpdate::Remove(key)) => {
            cache.remove(&key);
        }
        None => {}
    }
}

fn evict_http_promoted_exact_prompt(
    session: &crate::HttpSessionSnapshot,
    queued: Option<&HttpQueuedRunTerminalContext>,
    exact_queue_prompts: &Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>,
) -> Result<(), HttpRunDriverError> {
    let Some(queued) = queued else {
        return Ok(());
    };
    let state = read_http_durable_queue_state(session)
        .map_err(|_| HttpRunDriverError::new("durable queued promotion state is unavailable"))?;
    let still_queued = state
        .projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == queued.queue_id)
        .is_some_and(|item| item.status == ConversationInputStatus::Queued);
    if still_queued {
        return Ok(());
    }
    exact_queue_prompts
        .lock()
        .map_err(|_| HttpRunDriverError::new("queued exact prompt state is unavailable"))?
        .remove(&queued.exact_prompt_key);
    Ok(())
}

fn finalize_http_queued_terminal(
    session: &crate::HttpSessionSnapshot,
    queued: &HttpQueuedRunTerminalContext,
    unpromoted_terminal: HttpQueuedUnpromotedTerminal,
) -> Result<(), HttpRunDriverError> {
    let records = JsonlSessionStore::read_event_records(&session.session_log_path)
        .map_err(|_| HttpRunDriverError::new("queued terminal evidence is unavailable"))?;
    if records
        .iter()
        .any(|record| record.session_id() != session.durable_session_scope_id)
    {
        return Err(HttpRunDriverError::new(
            "queued terminal evidence belongs to another durable session",
        ));
    }
    let queue = ConversationQueueDurableProjection::from_records(&records)
        .map_err(|_| HttpRunDriverError::new("durable queued terminal state is invalid"))?;
    let Some(item) = queue
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == queued.queue_id)
    else {
        return Ok(());
    };
    let unpromoted = item.status == ConversationInputStatus::Queued;
    if !unpromoted && item.status != ConversationInputStatus::Dispatching {
        return Ok(());
    }

    let (expectation, status, reason) = if unpromoted {
        let (status, reason) = match unpromoted_terminal {
            HttpQueuedUnpromotedTerminal::Rejected => (
                ConversationInputStatus::Rejected,
                Some("queued run preparation ended before durable promotion".to_owned()),
            ),
            HttpQueuedUnpromotedTerminal::Cancelled => (
                ConversationInputStatus::Cancelled,
                Some("queued run was cancelled before durable promotion".to_owned()),
            ),
        };
        (
            ConversationInputTerminalExpectation::Queued {
                expected_queue_revision: queued.expected_queue_revision.clone(),
                queue_id: queued.queue_id.clone(),
                expected_prompt_hash: queued.prompt_hash.clone(),
            },
            status,
            reason,
        )
    } else {
        let Some(promotion) = http_queued_promotion(&records, &queued.queue_id) else {
            return Ok(());
        };
        if promotion.dispatch_run_id != queued.dispatch_run_id {
            return Ok(());
        }
        let (status, reason) =
            http_queued_terminal_from_attempt_evidence(&records, &queued.dispatch_run_id)?;
        let expected_frontier = records
            .last()
            .map(ConversationInputTerminalFrontier::from_record)
            .ok_or_else(|| HttpRunDriverError::new("queued terminal frontier is unavailable"))?;
        (
            ConversationInputTerminalExpectation::Promoted {
                queue_id: queued.queue_id.clone(),
                dispatch_run_id: queued.dispatch_run_id.clone(),
                expected_frontier,
            },
            status,
            reason,
        )
    };
    let store = JsonlSessionStore::new(&session.session_log_path)
        .map_err(|_| HttpRunDriverError::new("queued terminal store is unavailable"))?;
    store
        .append_conversation_input_terminal_if_current(ConversationInputTerminalCommand {
            expectation,
            terminal: ConversationInputStatusEntry {
                queue_id: queued.queue_id.clone(),
                status,
                reason,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        })
        .map(|_| ())
        .map_err(|_| HttpRunDriverError::new("queued terminal status could not be persisted"))
}

fn http_queued_promotion(
    records: &[sigil_kernel::SessionStreamRecord],
    queue_id: &ConversationInputQueueId,
) -> Option<ConversationInputPromotedEntry> {
    records.iter().rev().find_map(|record| {
        let event = record.stored_event();
        if event.event_kind() != Some(sigil_kernel::DurableEventType::ConversationInputPromoted) {
            return None;
        }
        serde_json::from_value::<ConversationInputPromotedEntry>(event.payload.clone())
            .ok()
            .filter(|promotion| &promotion.queue_id == queue_id)
    })
}

fn http_queued_terminal_from_attempt_evidence(
    records: &[sigil_kernel::SessionStreamRecord],
    dispatch_run_id: &str,
) -> Result<(ConversationInputStatus, Option<String>), HttpRunDriverError> {
    let attempts = ProviderPhysicalAttemptProjection::from_records(records)
        .map_err(|_| HttpRunDriverError::new("queued provider attempt evidence is invalid"))?;
    let attempts = attempts.attempts_for_logical_run_id(dispatch_run_id);
    Ok(match attempts.as_slice() {
        [] => (
            ConversationInputStatus::Rejected,
            Some("queued promotion was not followed by a provider physical attempt".to_owned()),
        ),
        [attempt] => match attempt.terminal.as_ref().map(|entry| entry.outcome) {
            Some(
                ProviderPhysicalAttemptOutcome::Completed
                | ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect
                | ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput,
            ) => (ConversationInputStatus::Delivered, None),
            Some(ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption) => (
                ConversationInputStatus::Rejected,
                Some("queued provider attempt confirmed no model consumption".to_owned()),
            ),
            Some(
                ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain
                | ProviderPhysicalAttemptOutcome::Interrupted,
            ) => (
                ConversationInputStatus::Stale,
                Some(
                    "queued provider outcome is uncertain and will not be replayed automatically"
                        .to_owned(),
                ),
            ),
            None => (
                ConversationInputStatus::Stale,
                Some("queued provider physical attempt has no durable terminal".to_owned()),
            ),
        },
        _ => (
            ConversationInputStatus::Stale,
            Some("queued promotion has multiple provider physical attempts".to_owned()),
        ),
    })
}

struct HttpProductionActiveRun {
    session_id: String,
    broker: Arc<HttpApprovalBroker>,
    cancel_sender: mpsc::UnboundedSender<HttpProductionRunControlCommand>,
}

#[derive(Clone)]
struct HttpProductionTerminalOwner {
    session_id: String,
    durable_session_scope_id: String,
    control: ApplicationTerminalTaskControl,
}

enum HttpProductionRunControlCommand {
    Cancel(HttpProductionCancellationCommand),
    Pause(HttpProductionTaskPauseCommand),
}

fn public_preparation_failure_event(error: &anyhow::Error) -> PublicRunEventKind {
    let typed = error.downcast_ref::<sigil_runtime::application_run::ApplicationRunPrepareError>();
    if let Some(typed) = typed {
        let recovery_binding = typed.recovery_binding().unwrap_or_default().to_owned();
        match typed.class() {
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::SessionRouteConfirmationRequired => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::SessionRouteConfirmationRequired,
                    actions: vec![
                        PublicRouteRecoveryAction::ConfirmCurrentRoute,
                        PublicRouteRecoveryAction::RepairConnection,
                        PublicRouteRecoveryAction::SelectReplacement,
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: true,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::SessionRouteSelectionRequired => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::SessionRouteSelectionRequired,
                    actions: vec![
                        PublicRouteRecoveryAction::RepairConnection,
                        PublicRouteRecoveryAction::SelectReplacement,
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: true,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::ModelRouteNotConfigured => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::ModelRouteNotConfigured,
                    actions: vec![
                        PublicRouteRecoveryAction::RepairConnection,
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: false,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::ConnectionConfigInvalid => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::ConnectionConfigInvalid,
                    actions: vec![
                        PublicRouteRecoveryAction::RepairConnection,
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: false,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::ProviderUnavailable => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::ProviderUnavailable,
                    actions: vec![
                        PublicRouteRecoveryAction::RetryProvider,
                        PublicRouteRecoveryAction::RepairConnection,
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: true,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::SessionAlreadyActive => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::SessionAlreadyActive,
                    actions: vec![
                        PublicRouteRecoveryAction::RetrySessionAttach,
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: true,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::SessionWriterBusy => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::SessionWriterBusy,
                    actions: vec![
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: true,
                };
            }
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::SessionStreamInvalid => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::SessionStreamInvalid,
                    actions: vec![
                        PublicRouteRecoveryAction::StartNewSession,
                        PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    recovery_binding,
                    retryable: false,
                };
            }
            _ => {}
        }
    }
    PublicRunEventKind::RunFailed {
        error: error.to_string(),
    }
}

struct HttpProductionCancellationCommand {
    reason: String,
    acknowledgement: std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
}

struct HttpProductionTaskPauseCommand {
    request: sigil_kernel::TaskPauseRequest,
    acknowledgement: std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
}

struct HttpRunSupervisor {
    options: HttpProductionRunDriverOptions,
    services: ApplicationRunServices,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
    start: HttpRunDriverStart,
    session_attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    queued: Option<HttpQueuedRunPreparation>,
    exact_queue_prompts: Arc<Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>>,
    terminal_owners: Arc<Mutex<BTreeMap<String, HttpProductionTerminalOwner>>>,
    cancel_receiver: mpsc::UnboundedReceiver<HttpProductionRunControlCommand>,
    post_run_maintenance: Arc<Mutex<Option<ApplicationPostRunMaintenance>>>,
}

impl HttpRunSupervisor {
    fn evict_promoted_exact_prompt(
        &self,
        queued: Option<&HttpQueuedRunTerminalContext>,
    ) -> Result<(), HttpRunDriverError> {
        evict_http_promoted_exact_prompt(&self.start.session, queued, &self.exact_queue_prompts)
    }

    async fn run(
        mut self,
        preprepared: Option<HttpPreparedApplicationRun>,
    ) -> Result<(), HttpRunDriverError> {
        let registry = self.registry.upgrade().ok_or_else(|| {
            HttpRunDriverError::new("production registry closed before run preparation")
        })?;
        let run_context = application_run_context_view(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&self.start.session.session_log_path),
            &self.start.session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable run-context projection failed"))?;
        let selected_model = self.start.model_ref.as_ref();
        let model_connection_id = selected_model
            .map(|model_ref| sigil_kernel::ConnectionId::new(model_ref.connection_id.clone()))
            .transpose()
            .map_err(|error| {
                HttpRunDriverError::new(format!("invalid selected connection: {error}"))
            })?
            .or_else(|| Some(run_context.model_ref.connection_id.clone()));
        let model_name = selected_model
            .map(|model_ref| model_ref.model_id.clone())
            .or_else(|| Some(run_context.model_ref.model_id.clone()));
        let request = ApplicationRunRequest {
            config_path: self.options.config_path.clone(),
            launch_cwd: self.options.launch_cwd.clone(),
            prompt: self.start.prompt.clone(),
            run_id: self.start.run.id.clone(),
            session_path: Some(PathBuf::from(&self.start.session.session_log_path)),
            session_attachment: Some(Arc::clone(&self.session_attachment)),
            interaction: ApplicationRunInteraction::ExternallyInteractive,
            permission_mode: Some(self.start.run.permission_mode.into()),
            model_name,
            model_connection_id,
            model_selection_binding: self.start.model_selection_binding.clone(),
            route_recovery_binding: self.start.route_recovery_binding.clone(),
            reasoning_effort: self.start.run.reasoning_effort.map(Into::into),
            reasoning_effort_binding: self.start.reasoning_effort_binding.clone(),
            skill_binding: self.start.skill_binding.clone().map(|binding| {
                sigil_runtime::ApplicationSkillBinding {
                    skill_id: binding.skill_id,
                    skill_sha256: binding.skill_sha256,
                    index_fingerprint: binding.index_fingerprint,
                }
            }),
            agent_binding: self.start.agent_binding.clone().map(|binding| {
                sigil_runtime::ApplicationAgentBinding {
                    profile_id: binding.profile_id,
                    snapshot_id: binding.snapshot_id,
                }
            }),
            constraints: None,
        };
        let services = self
            .services
            .clone()
            .with_terminal_lifecycle_handler(Arc::new(HttpProductionTerminalLifecycleHandler {
                durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
                run_id: self.start.run.id.clone(),
                registry: Arc::downgrade(&registry),
                event_bus: Arc::clone(&self.event_bus),
                terminal_owners: Arc::clone(&self.terminal_owners),
            }));
        let preparer = Arc::clone(&self.preparer);
        let queued_terminal = self
            .queued
            .as_ref()
            .map(|queued| HttpQueuedRunTerminalContext {
                queue_id: queued.promotion.queue_id.clone(),
                dispatch_run_id: queued.promotion.dispatch_run_id.clone(),
                expected_queue_revision: queued.promotion.expected_queue_revision.clone(),
                prompt_hash: queued.promotion.prompt_hash.clone(),
                exact_prompt_key: queued.exact_prompt_key.clone(),
            });
        let task_continuation = self.start.task_continuation.clone();
        let expected_session_scope_id = self.start.session.durable_session_scope_id.clone();
        let queued = self.queued.take();
        let mut preparation = Box::pin(async move {
            if let Some(prepared) = preprepared {
                if queued.is_some() || task_continuation.is_some() {
                    return Err(anyhow!(
                        "a pre-prepared run cannot also be queued or continue a Task"
                    ));
                }
                return Ok(prepared);
            }
            match (queued, task_continuation) {
                (Some(_), Some(_)) => Err(anyhow!("queued runs cannot continue an existing Task")),
                (Some(queued), None) => preparer
                    .prepare_queued(
                        ApplicationQueuedRunRequest {
                            run: request,
                            durable_queue: queued.durable_queue,
                            promotion: queued.promotion,
                            prompt_material: queued.prompt_material,
                            capability_registrations: queued.capability_registrations,
                        },
                        services,
                    )
                    .await
                    .map(Box::new)
                    .map(HttpPreparedApplicationRun::Conversation),
                (None, Some(task)) => {
                    let task_id = sigil_kernel::TaskId::new(task.task_id)?;
                    preparer
                        .prepare_task(
                            ApplicationTaskContinuationRequest {
                                config_path: request.config_path,
                                launch_cwd: request.launch_cwd,
                                session_path: request.session_path.ok_or_else(|| {
                                    anyhow!("Task continuation session path is unavailable")
                                })?,
                                session_attachment: request.session_attachment,
                                expected_session_scope_id,
                                run_id: request.run_id,
                                task_id,
                                guidance: task.guidance,
                                interaction: request.interaction,
                                permission_mode: request.permission_mode,
                            },
                            services,
                        )
                        .await
                        .map(Box::new)
                        .map(HttpPreparedApplicationRun::Task)
                }
                (None, None) => preparer
                    .prepare(request, services)
                    .await
                    .map(Box::new)
                    .map(HttpPreparedApplicationRun::Conversation),
            }
        });
        let preparation_outcome = tokio::select! {
            biased;
            result = &mut preparation => Ok(result),
            cancellation = self.cancel_receiver.recv() => Err(cancellation),
        };
        let preparation_result = match preparation_outcome {
            Ok(result) => {
                drop(preparation);
                result
            }
            Err(Some(HttpProductionRunControlCommand::Pause(pause))) => {
                let _ = pause.acknowledgement.send(Err(HttpRunDriverError::new(
                    "production Task pause is unavailable during run preparation",
                )));
                preparation.await
            }
            Err(Some(HttpProductionRunControlCommand::Cancel(cancellation))) => {
                let deadline = cancellation_deadline(self.options.cancellation_timeout);
                let joined =
                    tokio::time::timeout(remaining_until(deadline), &mut preparation).await;
                let preparation_result = match joined {
                    Ok(result) => result,
                    Err(_) => {
                        let error = HttpRunDriverError::new(
                            "production preparation did not quiesce before the cancellation deadline",
                        );
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &cancellation.acknowledgement,
                            error,
                        );
                        let _ = preparation.await;
                        self.evict_promoted_exact_prompt(queued_terminal.as_ref())?;
                        return Err(error);
                    }
                };
                drop(preparation);
                self.evict_promoted_exact_prompt(queued_terminal.as_ref())?;
                return match preparation_result {
                    Ok(prepared) => {
                        self.cancel_prepared_before_execution(
                            &registry,
                            cancellation,
                            prepared,
                            deadline,
                        )
                        .await
                    }
                    Err(_) => {
                        self.cancel_after_failed_preparation(&registry, cancellation, deadline)
                            .await
                    }
                };
            }
            Err(None) => {
                return Err(HttpRunDriverError::new(
                    "production cancellation owner closed during run preparation",
                ));
            }
        };
        self.evict_promoted_exact_prompt(queued_terminal.as_ref())?;
        let prepared = match preparation_result {
            Ok(prepared) => prepared,
            Err(error) => {
                let event = PublicRunEvent::new(
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    1,
                    public_preparation_failure_event(&error),
                );
                let event_bus = Arc::clone(&self.event_bus);
                tokio::task::spawn_blocking(move || event_bus.publish_next_run_event(event))
                    .await
                    .map_err(|_| {
                        HttpRunDriverError::new(
                            "production preparation terminal publication worker failed",
                        )
                    })?
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                record_run_terminal_and_reconcile_stream(
                    &registry,
                    &self.event_bus,
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    HttpRunTerminalOutcome::Failed,
                )?;
                return Ok(());
            }
        };
        if prepared.session_id() != self.start.session.durable_session_scope_id
            || prepared.session_log_path()
                != PathBuf::from(&self.start.session.session_log_path).as_path()
        {
            return Err(HttpRunDriverError::new(
                "prepared application run does not match its durable HTTP session binding",
            ));
        }
        self.terminal_owners
            .lock()
            .map_err(|_| HttpRunDriverError::new("production terminal-owner state unavailable"))?
            .insert(
                self.start.run.id.clone(),
                HttpProductionTerminalOwner {
                    session_id: self.start.session.id.clone(),
                    durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
                    control: prepared.terminal_control(),
                },
            );
        let (execution, control) = prepared.into_parts();
        let control = Arc::new(control);
        let event_handler = HttpProductionEventHandler {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            run_id: self.start.run.id.clone(),
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&self.broker),
            event_bus: Arc::clone(&self.event_bus),
        };
        let approval_handler = HttpProductionApprovalHandler {
            run_id: self.start.run.id.clone(),
            broker: Arc::clone(&self.broker),
        };
        let mut execution = Box::pin(execution.execute_on_owned_blocking(
            event_handler.clone(),
            approval_handler,
            Arc::clone(&self.post_run_maintenance),
        ));
        'run: loop {
            tokio::select! {
                biased;
                result = &mut execution => {
                let terminal_was_delivered = control
                    .terminal_was_delivered()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                let mut recovery_handler = event_handler.clone();
                let terminal = ensure_application_execution_terminal(
                    terminal_was_delivered,
                    &result,
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    &mut recovery_handler,
                )?;
                record_run_terminal_and_reconcile_stream(
                    &registry,
                    &self.event_bus,
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    terminal,
                )?;
                    break 'run;
                }
                command = self.cancel_receiver.recv() => {
                let Some(command) = command else {
                    return Err(HttpRunDriverError::new(
                        "production cancellation owner closed before run terminal",
                    ));
                };
                let cancellation = match command {
                    HttpProductionRunControlCommand::Cancel(cancellation) => cancellation,
                    HttpProductionRunControlCommand::Pause(pause) => {
                        if self
                            .pause_active_task(
                                &registry,
                                pause,
                                Arc::clone(&control),
                                &mut execution,
                                event_handler.clone(),
                            )
                            .await?
                        {
                            break 'run;
                        }
                        continue 'run;
                    }
                };
                let acknowledgement = cancellation.acknowledgement;
                let deadline = cancellation_deadline(self.options.cancellation_timeout);
                let mut acknowledgement_sent = false;
                let request_control = Arc::clone(&control);
                let request_broker = Arc::clone(&self.broker);
                let request_timeout = remaining_until(deadline);
                let mut request_worker = tokio::task::spawn_blocking(move || {
                    request_control.request_cancellation(
                        cancellation.reason,
                        Some(request_timeout),
                        || request_broker.cancel_all(),
                    )
                });
                let request = match tokio::time::timeout(
                    remaining_until(deadline),
                    &mut request_worker,
                )
                .await
                {
                    Ok(Ok(request)) => request,
                    Ok(Err(_)) => {
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production cancellation activation worker failed",
                            ),
                        );
                        let natural_result = (&mut execution).await;
                        if record_natural_terminal_if_delivered(
                            &control,
                            &registry,
                            &self.event_bus,
                            &self.start.session.durable_session_scope_id,
                            &self.start.run.id,
                            &natural_result,
                        )? {
                            return Ok(());
                        }
                        return Err(error);
                    }
                    Err(_) => {
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production cancellation activation missed its shared deadline",
                            ),
                        );
                        acknowledgement_sent = true;
                        match request_worker.await {
                            Ok(request) => request,
                            Err(_) => {
                                let natural_result = (&mut execution).await;
                                if record_natural_terminal_if_delivered(
                                    &control,
                                    &registry,
                                    &self.event_bus,
                                    &self.start.session.durable_session_scope_id,
                                    &self.start.run.id,
                                    &natural_result,
                                )? {
                                    return Ok(());
                                }
                                return Err(error);
                            }
                        }
                    }
                };
                let ticket = match request {
                    Ok(ticket) => ticket,
                    Err(error) => match error.into_ticket() {
                        Some(ticket) => ticket,
                        None => {
                            let natural_result = match tokio::time::timeout(
                                remaining_until(deadline),
                                &mut execution,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => {
                                    let error = HttpRunDriverError::new(
                                        "natural run terminal did not join before the cancellation deadline",
                                    );
                                    let error = if acknowledgement_sent {
                                        error
                                    } else {
                                        quarantine_cancellation_failure(
                                            &registry,
                                            &self.start.run.id,
                                            &acknowledgement,
                                            error,
                                        )
                                    };
                                    let natural_result = (&mut execution).await;
                                    if record_natural_terminal_if_delivered(
                                        &control,
                                        &registry,
                                        &self.event_bus,
                                        &self.start.session.durable_session_scope_id,
                                        &self.start.run.id,
                                        &natural_result,
                                    )? {
                                        return Ok(());
                                    }
                                    return Err(error);
                                }
                            };
                            let terminal_was_delivered = match control.terminal_was_delivered() {
                                Ok(delivered) => delivered,
                                Err(error) => {
                                    let error = HttpRunDriverError::new(error.to_string());
                                    let error = if acknowledgement_sent {
                                        error
                                    } else {
                                        quarantine_cancellation_failure(
                                            &registry,
                                            &self.start.run.id,
                                            &acknowledgement,
                                            error,
                                        )
                                    };
                                    return Err(error);
                                }
                            };
                            if !terminal_was_delivered {
                                let error = HttpRunDriverError::new(
                                    "natural run completion won cancellation without a durable protocol terminal",
                                );
                                let error = if acknowledgement_sent {
                                    error
                                } else {
                                    quarantine_cancellation_failure(
                                        &registry,
                                        &self.start.run.id,
                                        &acknowledgement,
                                        error,
                                    )
                                };
                                return Err(error);
                            }
                            let terminal = http_terminal_from_application_result(&natural_result);
                            if let Err(error) = record_run_terminal_and_reconcile_stream(
                                &registry,
                                &self.event_bus,
                                &self.start.session.durable_session_scope_id,
                                &self.start.run.id,
                                terminal,
                            ) {
                                let error = if acknowledgement_sent {
                                    error
                                } else {
                                    quarantine_cancellation_failure(
                                        &registry,
                                        &self.start.run.id,
                                        &acknowledgement,
                                        error,
                                    )
                                };
                                return Err(error);
                            }
                            self.broker.cancel_all();
                            if !acknowledgement_sent {
                                let _ = acknowledgement.send(Ok(()));
                            }
                            return Ok(());
                        }
                    },
                };
                let execution_joined = tokio::time::timeout(
                    ticket.remaining_timeout(),
                    &mut execution,
                )
                .await
                .is_ok();
                if !execution_joined && !acknowledgement_sent {
                    let _ = quarantine_cancellation_failure(
                        &registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "production execution did not join before the cancellation deadline",
                        ),
                    );
                    acknowledgement_sent = true;
                }
                let finalize_control = Arc::clone(&control);
                let runtime = tokio::runtime::Handle::current();
                let mut cancellation_events = event_handler;
                let mut finalize_worker = tokio::task::spawn_blocking(move || {
                    runtime.block_on(finalize_control.finalize_cancellation(
                        ticket,
                        execution_joined,
                        &mut cancellation_events,
                    ))
                });
                let finalized = match tokio::time::timeout(
                    remaining_until(deadline),
                    &mut finalize_worker,
                )
                .await
                {
                    Ok(Ok(finalized)) => finalized,
                    Ok(Err(_)) => Err(anyhow!(
                        "production cancellation finalization worker failed"
                    )),
                    Err(_) => {
                        if !acknowledgement_sent {
                            let _ = quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                HttpRunDriverError::new(
                                    "production cancellation finalization missed its shared deadline",
                                ),
                            );
                            acknowledgement_sent = true;
                        }
                        finalize_worker.await.map_err(|_| {
                            HttpRunDriverError::new(
                                "production cancellation finalization worker failed",
                            )
                        })?
                    }
                };
                let terminal = match finalized {
                    Ok(sigil_kernel::RunCancellationTerminalOutcome::Cancelled) => {
                        HttpRunTerminalOutcome::Cancelled
                    }
                    Ok(sigil_kernel::RunCancellationTerminalOutcome::Interrupted) => {
                        HttpRunTerminalOutcome::Interrupted
                    }
                    Err(error) => {
                        let error = HttpRunDriverError::new(format!(
                            "production cancellation terminal could not be durably proven: {error}"
                        ));
                        let error = if acknowledgement_sent {
                            error
                        } else {
                            quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                error,
                            )
                        };
                        if !execution_joined {
                            let _ = (&mut execution).await;
                        }
                        return Err(error);
                    }
                };
                if !execution_joined {
                    let _ = (&mut execution).await;
                }
                let terminal_was_delivered = match control.terminal_was_delivered() {
                    Ok(delivered) => delivered,
                    Err(error) => {
                        let error = HttpRunDriverError::new(error.to_string());
                        let error = if acknowledgement_sent {
                            error
                        } else {
                            quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                error,
                            )
                        };
                        return Err(error);
                    }
                };
                if !terminal_was_delivered {
                    let error = HttpRunDriverError::new(
                        "production cancellation ended without a durable protocol terminal",
                    );
                    let error = if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    };
                    return Err(error);
                }
                if let Err(error) = record_run_terminal_and_reconcile_stream(
                    &registry,
                    &self.event_bus,
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    terminal,
                ) {
                    let error = if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    };
                    return Err(error);
                }
                if !acknowledgement_sent {
                    let _ = acknowledgement.send(Ok(()));
                }
                    break 'run;
                }
            }
        }
        self.broker.cancel_all();
        Ok(())
    }

    async fn pause_active_task<F>(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        pause: HttpProductionTaskPauseCommand,
        control: Arc<ApplicationRunControl>,
        execution: &mut Pin<Box<F>>,
        event_handler: HttpProductionEventHandler,
    ) -> Result<bool, HttpRunDriverError>
    where
        F: Future<Output = Result<ApplicationRunTerminalStatus>>,
    {
        let acknowledgement = pause.acknowledgement;
        let deadline = cancellation_deadline(self.options.cancellation_timeout);
        let request_control = Arc::clone(&control);
        let request_broker = Arc::clone(&self.broker);
        let request_timeout = remaining_until(deadline);
        let mut request_worker = tokio::task::spawn_blocking(move || {
            request_control.request_task_pause(pause.request, Some(request_timeout), || {
                request_broker.cancel_all()
            })
        });
        let request =
            match tokio::time::timeout(remaining_until(deadline), &mut request_worker).await {
                Ok(Ok(request)) => request,
                Ok(Err(_)) => {
                    let error = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new("production Task pause activation worker failed"),
                    );
                    let natural_result = (&mut *execution).await;
                    if record_natural_terminal_if_delivered(
                        &control,
                        registry,
                        &self.event_bus,
                        &self.start.session.durable_session_scope_id,
                        &self.start.run.id,
                        &natural_result,
                    )? {
                        return Ok(true);
                    }
                    return Err(error);
                }
                Err(_) => {
                    let error = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "production Task pause activation missed its shared deadline",
                        ),
                    );
                    let request = request_worker.await.map_err(|_| error.clone())?;
                    match request {
                        Ok(ticket) => Ok(ticket),
                        Err(request_error) => match request_error.into_ticket() {
                            Some(ticket) => Ok(ticket),
                            None => {
                                let natural_result = (&mut *execution).await;
                                if record_natural_terminal_if_delivered(
                                    &control,
                                    registry,
                                    &self.event_bus,
                                    &self.start.session.durable_session_scope_id,
                                    &self.start.run.id,
                                    &natural_result,
                                )? {
                                    return Ok(true);
                                }
                                return Err(error);
                            }
                        },
                    }
                }
            };
        let ticket = match request {
            Ok(ticket) => ticket,
            Err(error) => {
                let message = error.to_string();
                match error.into_ticket() {
                    Some(ticket) => ticket,
                    None => {
                        let _ = acknowledgement.send(Err(HttpRunDriverError::new(message)));
                        return Ok(false);
                    }
                }
            }
        };
        let execution_joined = tokio::time::timeout(ticket.remaining_timeout(), &mut *execution)
            .await
            .is_ok();
        let mut acknowledgement_sent = false;
        if !execution_joined {
            let _ = quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                HttpRunDriverError::new(
                    "production Task execution did not join before the pause deadline",
                ),
            );
            acknowledgement_sent = true;
        }
        let finalize_control = Arc::clone(&control);
        let runtime = tokio::runtime::Handle::current();
        let mut pause_events = event_handler;
        let mut finalize_worker = tokio::task::spawn_blocking(move || {
            runtime.block_on(finalize_control.finalize_task_pause(
                ticket,
                execution_joined,
                &mut pause_events,
            ))
        });
        let finalized =
            match tokio::time::timeout(remaining_until(deadline), &mut finalize_worker).await {
                Ok(Ok(finalized)) => finalized,
                Ok(Err(_)) => Err(anyhow!("production Task pause finalization worker failed")),
                Err(_) => {
                    if !acknowledgement_sent {
                        let _ = quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production Task pause finalization missed its shared deadline",
                            ),
                        );
                        acknowledgement_sent = true;
                    }
                    finalize_worker.await.map_err(|_| {
                        HttpRunDriverError::new("production Task pause finalization worker failed")
                    })?
                }
            };
        let terminal = match finalized {
            Ok(outcome) if outcome.task_status == sigil_kernel::TaskRunStatus::Paused => {
                HttpRunTerminalOutcome::Paused
            }
            Ok(outcome) if outcome.task_status == sigil_kernel::TaskRunStatus::Interrupted => {
                HttpRunTerminalOutcome::Interrupted
            }
            Ok(_) => {
                return Err(HttpRunDriverError::new(
                    "production Task pause reached an invalid durable status",
                ));
            }
            Err(error) => {
                let error = HttpRunDriverError::new(format!(
                    "production Task pause terminal could not be durably proven: {error}"
                ));
                let error = if acknowledgement_sent {
                    error
                } else {
                    quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        error,
                    )
                };
                if !execution_joined {
                    let _ = (&mut *execution).await;
                }
                return Err(error);
            }
        };
        if !execution_joined {
            let _ = (&mut *execution).await;
        }
        let terminal_was_delivered = control
            .terminal_was_delivered()
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        if !terminal_was_delivered {
            let error = HttpRunDriverError::new(
                "production Task pause ended without a durable protocol terminal",
            );
            return Err(if acknowledgement_sent {
                error
            } else {
                quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                )
            });
        }
        if let Err(error) = record_run_terminal_and_reconcile_stream(
            registry,
            &self.event_bus,
            &self.start.session.durable_session_scope_id,
            &self.start.run.id,
            terminal,
        ) {
            return Err(if acknowledgement_sent {
                error
            } else {
                quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                )
            });
        }
        if !acknowledgement_sent {
            let _ = acknowledgement.send(Ok(()));
        }
        Ok(true)
    }

    async fn cancel_prepared_before_execution(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        cancellation: HttpProductionCancellationCommand,
        prepared: HttpPreparedApplicationRun,
        deadline: Instant,
    ) -> Result<(), HttpRunDriverError> {
        let acknowledgement = cancellation.acknowledgement;
        if prepared.session_id() != self.start.session.durable_session_scope_id
            || prepared.session_log_path()
                != PathBuf::from(&self.start.session.session_log_path).as_path()
        {
            let error = HttpRunDriverError::new(
                "prepared cancellation does not match its durable HTTP session binding",
            );
            return Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            ));
        }
        let (execution, control) = prepared.into_parts();
        let control = Arc::new(control);
        let request_control = Arc::clone(&control);
        let request_broker = Arc::clone(&self.broker);
        let request_timeout = remaining_until(deadline);
        let mut request_worker = tokio::task::spawn_blocking(move || {
            request_control.request_cancellation(cancellation.reason, Some(request_timeout), || {
                request_broker.cancel_all()
            })
        });
        let mut acknowledgement_sent = false;
        let request = match tokio::time::timeout(remaining_until(deadline), &mut request_worker)
            .await
        {
            Ok(Ok(request)) => request,
            Ok(Err(_)) => {
                let error =
                    HttpRunDriverError::new("pre-execution cancellation activation worker failed");
                return Err(quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                ));
            }
            Err(_) => {
                let _ = quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    HttpRunDriverError::new(
                        "pre-execution cancellation activation missed its shared deadline",
                    ),
                );
                acknowledgement_sent = true;
                request_worker.await.map_err(|_| {
                    HttpRunDriverError::new("pre-execution cancellation activation worker failed")
                })?
            }
        };
        let ticket = match request {
            Ok(ticket) => ticket,
            Err(error) => match error.into_ticket() {
                Some(ticket) => ticket,
                None => {
                    let error = HttpRunDriverError::new(
                        "pre-execution cancellation could not be durably activated",
                    );
                    return Err(if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    });
                }
            },
        };
        drop(execution);
        let finalize_control = Arc::clone(&control);
        let runtime = tokio::runtime::Handle::current();
        let mut event_handler = HttpProductionEventHandler {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            run_id: self.start.run.id.clone(),
            registry: Arc::downgrade(registry),
            broker: Arc::clone(&self.broker),
            event_bus: Arc::clone(&self.event_bus),
        };
        let mut finalize_worker = tokio::task::spawn_blocking(move || {
            runtime.block_on(finalize_control.finalize_cancellation(
                ticket,
                true,
                &mut event_handler,
            ))
        });
        let finalized = match tokio::time::timeout(remaining_until(deadline), &mut finalize_worker)
            .await
        {
            Ok(Ok(finalized)) => finalized,
            Ok(Err(_)) => Err(anyhow!(
                "pre-execution cancellation finalization worker failed"
            )),
            Err(_) => {
                if !acknowledgement_sent {
                    let _ = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "pre-execution cancellation finalization missed its shared deadline",
                        ),
                    );
                    acknowledgement_sent = true;
                }
                finalize_worker.await.map_err(|_| {
                    HttpRunDriverError::new("pre-execution cancellation finalization worker failed")
                })?
            }
        };
        let result = finalized
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "pre-execution cancellation terminal could not be durably proven: {error}"
                ))
            })
            .and_then(|terminal| {
                if !control
                    .terminal_was_delivered()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?
                {
                    return Err(HttpRunDriverError::new(
                        "pre-execution cancellation ended without a durable protocol terminal",
                    ));
                }
                let terminal = match terminal {
                    sigil_kernel::RunCancellationTerminalOutcome::Cancelled => {
                        HttpRunTerminalOutcome::Cancelled
                    }
                    sigil_kernel::RunCancellationTerminalOutcome::Interrupted => {
                        HttpRunTerminalOutcome::Interrupted
                    }
                };
                record_run_terminal_and_reconcile_stream(
                    registry,
                    &self.event_bus,
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    terminal,
                )
                .map(|_| ())
            });
        match result {
            Ok(()) => {
                if !acknowledgement_sent {
                    let _ = acknowledgement.send(Ok(()));
                }
                Ok(())
            }
            Err(error) if acknowledgement_sent => Err(error),
            Err(error) => Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            )),
        }
    }

    async fn cancel_after_failed_preparation(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        cancellation: HttpProductionCancellationCommand,
        deadline: Instant,
    ) -> Result<(), HttpRunDriverError> {
        let acknowledgement = cancellation.acknowledgement;
        let config_path = self.options.config_path.clone();
        let session_path = PathBuf::from(&self.start.session.session_log_path);
        let run_id = self.start.run.id.clone();
        let reason = cancellation.reason;
        let session_attachment = Arc::clone(&self.session_attachment);
        let mut binding_worker = tokio::task::spawn_blocking(move || {
            record_application_preparation_cancellation_with_attachment(
                &config_path,
                &session_path,
                &run_id,
                &reason,
                session_attachment,
            )
        });
        let mut acknowledgement_sent = false;
        let binding_result =
            match tokio::time::timeout(remaining_until(deadline), &mut binding_worker).await {
                Ok(joined) => match joined {
                    Ok(binding) => {
                        binding.map_err(|error| HttpRunDriverError::new(error.to_string()))
                    }
                    Err(_) => Err(HttpRunDriverError::new(
                        "production preparation cancellation worker failed",
                    )),
                },
                Err(_) => {
                    let error = HttpRunDriverError::new(
                        "preparation cancellation evidence missed its shared deadline",
                    );
                    let _ = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        error,
                    );
                    acknowledgement_sent = true;
                    Ok(binding_worker
                        .await
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?)
                }
            };
        let binding = match binding_result {
            Ok(binding) => binding,
            Err(error) if acknowledgement_sent => return Err(error),
            Err(error) => {
                return Err(quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                ));
            }
        };
        let result = async {
            if binding.session_scope_id != self.start.session.durable_session_scope_id
                || binding.session_log_path != Path::new(&self.start.session.session_log_path)
            {
                return Err(HttpRunDriverError::new(
                    "preparation cancellation does not match its durable HTTP session binding",
                ));
            }
            let event = PublicRunEvent::new(
                &self.start.session.durable_session_scope_id,
                &self.start.run.id,
                1,
                PublicRunEventKind::RunCancelled,
            );
            let event_bus = Arc::clone(&self.event_bus);
            let mut publication_worker =
                tokio::task::spawn_blocking(move || event_bus.publish_next_run_event(event));
            match tokio::time::timeout(remaining_until(deadline), &mut publication_worker).await {
                Ok(joined) => {
                    joined
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation publication worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                }
                Err(_) => {
                    let error = HttpRunDriverError::new(
                        "preparation cancellation publication missed its shared deadline",
                    );
                    if !acknowledgement_sent {
                        let _ = quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        );
                        acknowledgement_sent = true;
                    }
                    publication_worker
                        .await
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation publication worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                }
            };
            record_run_terminal_and_reconcile_stream(
                registry,
                &self.event_bus,
                &self.start.session.durable_session_scope_id,
                &self.start.run.id,
                HttpRunTerminalOutcome::Cancelled,
            )
            .map(|_| ())
        }
        .await;
        if acknowledgement_sent {
            return result;
        }
        match result {
            Ok(()) => {
                let _ = acknowledgement.send(Ok(()));
                Ok(())
            }
            Err(error) => Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            )),
        }
    }
}

#[derive(Clone)]
struct HttpProductionEventHandler {
    durable_session_scope_id: String,
    run_id: String,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
    event_bus: Arc<HttpLiveEventBus>,
}

struct HttpProductionTerminalLifecycleHandler {
    durable_session_scope_id: String,
    run_id: String,
    registry: Weak<HttpSessionRunRegistry>,
    event_bus: Arc<HttpLiveEventBus>,
    terminal_owners: Arc<Mutex<BTreeMap<String, HttpProductionTerminalOwner>>>,
}

impl std::fmt::Debug for HttpProductionTerminalLifecycleHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpProductionTerminalLifecycleHandler")
            .field("durable_session_scope_id", &self.durable_session_scope_id)
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl sigil_runtime::ApplicationTerminalLifecycleHandler for HttpProductionTerminalLifecycleHandler {
    fn handle_terminal_lifecycle(
        &self,
        session_id: &str,
        run_id: &str,
        event: &sigil_kernel::TerminalLifecycleEvent,
    ) -> Result<()> {
        if session_id != self.durable_session_scope_id || run_id != self.run_id {
            return Err(anyhow!("terminal lifecycle route identity changed"));
        }
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| anyhow!("terminal lifecycle registry is closed"))?;
        let Some(_sequence) = registry
            .record_terminal_lifecycle_with_publication(
                &self.run_id,
                event,
                |_registry_sequence, close_stream_after_publication| {
                    let public_event = PublicRunEvent::new(
                        &self.durable_session_scope_id,
                        &self.run_id,
                        1,
                        PublicRunEventKind::TerminalLifecycle {
                            event: event.clone(),
                        },
                    );
                    if close_stream_after_publication {
                        self.event_bus
                            .publish_next_run_event_and_close_stream(public_event)
                    } else {
                        self.event_bus.publish_next_run_event(public_event)
                    }
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                },
            )
            .map_err(|error| anyhow!(error))?
        else {
            return Ok(());
        };
        if registry.get_run(&self.run_id).ok().is_some_and(|run| {
            !run.terminal_tasks.is_empty()
                && run
                    .terminal_tasks
                    .iter()
                    .all(|task| task.status.is_terminal())
        }) {
            self.terminal_owners
                .lock()
                .map_err(|_| anyhow!("production terminal-owner state unavailable"))?
                .remove(&self.run_id);
        }
        Ok(())
    }
}

impl ApplicationRunEventHandler for HttpProductionEventHandler {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        if event.run_id != self.run_id {
            return Err(anyhow!(
                "application event belongs to another production run"
            ));
        }
        if event.session_id != self.durable_session_scope_id {
            return Err(anyhow!(
                "application event belongs to another durable production session"
            ));
        }
        let route_transition = match &event.event {
            PublicRunEventKind::RouteTransition { transition } => {
                Some(http_public_route_transition(transition.clone()))
            }
            _ => None,
        };
        let application_terminal = matches!(
            &event.event,
            PublicRunEventKind::RunFinished { .. }
                | PublicRunEventKind::RunFailed { .. }
                | PublicRunEventKind::RunBlocked { .. }
                | PublicRunEventKind::RunPaused { .. }
                | PublicRunEventKind::RunInterrupted { .. }
                | PublicRunEventKind::RouteRecoveryRequired { .. }
                | PublicRunEventKind::RunCancelled
        );
        match &event.event {
            PublicRunEventKind::ApprovalResolved {
                call_id,
                approval_request_id,
                approved,
                ..
            } => {
                self.registry
                    .upgrade()
                    .ok_or_else(|| anyhow!("production approval registry is closed"))?
                    .record_approval_resolution(
                        &self.run_id,
                        call_id,
                        approval_request_id,
                        *approved,
                    )?;
            }
            PublicRunEventKind::Control { control } if control.kind == "tool_execution" => {
                let payload = control
                    .payload
                    .as_ref()
                    .ok_or_else(|| anyhow!("tool execution control payload is unavailable"))?;
                let ControlEntry::ToolExecution(execution) =
                    serde_json::from_value::<ControlEntry>(payload.clone())?
                else {
                    return Err(anyhow!("tool execution control payload changed kind"));
                };
                self.registry
                    .upgrade()
                    .ok_or_else(|| anyhow!("production execution registry is closed"))?
                    .record_tool_execution_lifecycle(&self.run_id, &execution)?;
            }
            _ => {}
        }
        let mut approval_request = None;
        let publication = match &event.event {
            PublicRunEventKind::ApprovalRequested {
                approval_identity,
                effects,
                analysis,
                containment,
                safe_summary,
                decision_reasons,
                session_grant_available,
                session_grant_unavailable_reason,
                call,
                spec,
                subjects,
                operation,
                risk,
                snapshot_required,
                ..
            } => {
                let registry = self
                    .registry
                    .upgrade()
                    .ok_or_else(|| anyhow!("production approval registry is closed"))?;
                let display = pending_approval_display(
                    event.sequence.max(1),
                    effects,
                    analysis,
                    containment,
                    safe_summary,
                    decision_reasons,
                    subjects,
                    *operation,
                    *risk,
                    *snapshot_required,
                    session_grant_available
                        .then(|| sigil_kernel::derive_command_family_allow_pattern_for_call(call))
                        .flatten(),
                );
                let pending = self
                    .broker
                    .register(
                        &self.run_id,
                        call,
                        spec,
                        approval_identity,
                        *session_grant_available,
                        *session_grant_unavailable_reason,
                        display,
                    )
                    .map_err(|error| anyhow!(error))?;
                if let Err(error) =
                    registry.register_approval_request(&self.run_id, pending.clone())
                {
                    self.broker
                        .cancel(&call.id, &approval_identity.approval_request_id);
                    return Err(anyhow!(error));
                }
                approval_request = Some(pending.clone());
                let published = self.event_bus.publish_next_run_event_with_approval(
                    event.clone(),
                    |event_sequence| {
                        let mut published_pending = pending.clone();
                        published_pending.display.event_sequence = event_sequence;
                        Ok(published_pending)
                    },
                );
                if let Ok(protocol) = &published {
                    registry.update_approval_event_sequence(
                        &self.run_id,
                        &pending.call_id,
                        &pending.approval_request_id,
                        protocol.run_event.sequence,
                    );
                    if let Some(approval) = approval_request.as_mut() {
                        approval.display.event_sequence = protocol.run_event.sequence;
                    }
                }
                published
            }
            _ if application_terminal => self
                .event_bus
                .publish_next_run_event_with_stream_continuation(event),
            _ => self.event_bus.publish_next_run_event(event),
        };
        if let Err(error) = publication {
            if let Some(approval) = approval_request {
                let call_id = approval.call_id;
                self.broker.cancel(&call_id, &approval.approval_request_id);
                if let Some(registry) = self.registry.upgrade() {
                    let _ = registry.expire_approval_request_exact(
                        &self.run_id,
                        &call_id,
                        &approval.approval_request_id,
                    );
                }
            }
            return Err(anyhow!(error));
        }
        if let Some(transition) = route_transition {
            self.registry
                .upgrade()
                .ok_or_else(|| anyhow!("production route-transition registry is closed"))?
                .record_session_route_transition(&self.durable_session_scope_id, transition)?;
        }
        Ok(())
    }
}

struct HttpProductionApprovalHandler {
    run_id: String,
    broker: Arc<HttpApprovalBroker>,
}

impl ApprovalHandler for HttpProductionApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Err(anyhow!(
            "production HTTP approval requires an exact kernel approval identity"
        ))
    }

    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        _spec: &ToolSpec,
        context: &ToolApprovalContext,
    ) -> Result<ToolApproval> {
        if context.identity.run_id != self.run_id || context.identity.call_id != call.id {
            return Err(anyhow!("production HTTP approval identity changed"));
        }
        let outcome = self
            .broker
            .wait_for_decision(&call.id, &context.identity.approval_request_id)?;
        match outcome.decision {
            Some(HttpApprovalDecisionRecord {
                decision: ToolApprovalUserDecision::Approved,
                ..
            }) => Ok(ToolApproval::Approve),
            Some(HttpApprovalDecisionRecord {
                decision: ToolApprovalUserDecision::Denied,
                reason,
                ..
            }) => Ok(ToolApproval::Deny {
                reason: reason.unwrap_or_else(|| "HTTP user denied tool call".to_owned()),
            }),
            Some(HttpApprovalDecisionRecord {
                decision: ToolApprovalUserDecision::ApprovedForSession,
                ..
            }) => Ok(ToolApproval::ApproveForSession),
            None => Ok(ToolApproval::Cancelled {
                reason: "HTTP approval route ended without a decision".to_owned(),
            }),
        }
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct HttpApprovalBroker {
    pending: Mutex<BTreeMap<String, Arc<HttpApprovalSlot>>>,
}

impl HttpApprovalBroker {
    fn register(
        &self,
        run_id: &str,
        call: &ToolCall,
        spec: &ToolSpec,
        identity: &ApprovalRequestIdentityV2,
        session_grant_available: bool,
        session_grant_unavailable_reason: Option<
            sigil_kernel::ToolApprovalSessionGrantUnavailableReason,
        >,
        display: HttpPendingApprovalDisplay,
    ) -> Result<HttpPendingApproval> {
        if identity.run_id != run_id || identity.call_id != call.id {
            return Err(anyhow!("production approval registration identity changed"));
        }
        if session_grant_available != session_grant_unavailable_reason.is_none() {
            return Err(anyhow!(
                "production approval session-grant availability invariant changed"
            ));
        }
        let tool_call_hash = tool_call_hash(call)?;
        let slot = Arc::new(HttpApprovalSlot {
            call_id: call.id.clone(),
            state: Mutex::new(HttpApprovalSlotState::Waiting),
            changed: Condvar::new(),
        });
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?;
        if pending.values().any(|existing| existing.call_id == call.id)
            || pending
                .insert(identity.approval_request_id.clone(), slot)
                .is_some()
        {
            return Err(anyhow!("duplicate production approval identity"));
        }
        Ok(HttpPendingApproval {
            call_id: call.id.clone(),
            tool_name: spec.name.clone(),
            approval_request_id: identity.approval_request_id.clone(),
            tool_call_hash,
            policy_version: identity.policy_version.clone(),
            expires_at_ms: identity.expires_at_ms,
            session_grant_available,
            session_grant_unavailable_reason,
            display,
        })
    }

    fn resolve(
        &self,
        call_id: &str,
        approval_request_id: &str,
        decision: HttpApprovalDecisionRecord,
    ) -> Result<(), HttpRunDriverError> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| HttpRunDriverError::new("production approval broker is unavailable"))?
            .get(approval_request_id)
            .cloned()
            .ok_or_else(|| {
                HttpRunDriverError::new(format!("production approval is not pending: {call_id}"))
            })?;
        if slot.call_id != call_id {
            return Err(HttpRunDriverError::new(
                "production approval request belongs to another tool call",
            ));
        }
        let mut state = slot
            .state
            .lock()
            .map_err(|_| HttpRunDriverError::new("production approval slot is unavailable"))?;
        if !matches!(*state, HttpApprovalSlotState::Waiting) {
            return Err(HttpRunDriverError::new(format!(
                "production approval is no longer waiting: {call_id}"
            )));
        }
        *state = HttpApprovalSlotState::Resolved(decision);
        slot.changed.notify_all();
        Ok(())
    }

    fn wait_for_decision(
        &self,
        call_id: &str,
        approval_request_id: &str,
    ) -> Result<HttpApprovalWaitOutcome> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?
            .get(approval_request_id)
            .cloned()
            .ok_or_else(|| anyhow!("production approval slot is missing"))?;
        if slot.call_id != call_id {
            return Err(anyhow!(
                "production approval request belongs to another tool call"
            ));
        }
        let mut state = slot
            .state
            .lock()
            .map_err(|_| anyhow!("production approval slot is unavailable"))?;
        loop {
            match &*state {
                HttpApprovalSlotState::Resolved(decision) => {
                    let decision = decision.clone();
                    drop(state);
                    self.remove(approval_request_id, &slot);
                    return Ok(HttpApprovalWaitOutcome {
                        decision: Some(decision),
                    });
                }
                HttpApprovalSlotState::Cancelled => {
                    drop(state);
                    self.remove(approval_request_id, &slot);
                    return Err(anyhow!("production approval wait was cancelled"));
                }
                HttpApprovalSlotState::Waiting => {}
            }
            state = slot
                .changed
                .wait(state)
                .map_err(|_| anyhow!("production approval slot is unavailable"))?;
        }
    }

    fn cancel(&self, call_id: &str, approval_request_id: &str) {
        let slot = self
            .pending
            .lock()
            .ok()
            .and_then(|pending| pending.get(approval_request_id).cloned());
        if let Some(slot) = slot
            && slot.call_id == call_id
            && let Ok(mut state) = slot.state.lock()
        {
            *state = HttpApprovalSlotState::Cancelled;
            slot.changed.notify_all();
        }
    }

    fn cancel_all(&self) {
        let slots = self
            .pending
            .lock()
            .map(|pending| pending.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for slot in slots {
            if let Ok(mut state) = slot.state.lock() {
                *state = HttpApprovalSlotState::Cancelled;
                slot.changed.notify_all();
            }
        }
    }

    fn remove(&self, approval_request_id: &str, expected: &Arc<HttpApprovalSlot>) {
        if let Ok(mut pending) = self.pending.lock()
            && pending
                .get(approval_request_id)
                .is_some_and(|slot| Arc::ptr_eq(slot, expected))
        {
            pending.remove(approval_request_id);
        }
    }
}

struct HttpApprovalSlot {
    call_id: String,
    state: Mutex<HttpApprovalSlotState>,
    changed: Condvar,
}

enum HttpApprovalSlotState {
    Waiting,
    Resolved(HttpApprovalDecisionRecord),
    Cancelled,
}

struct HttpApprovalWaitOutcome {
    decision: Option<HttpApprovalDecisionRecord>,
}

#[allow(clippy::too_many_arguments)]
fn pending_approval_display(
    event_sequence: u64,
    effects: &BTreeSet<ToolPermissionEffect>,
    analysis: &ToolAnalysisStatus,
    containment: &ExecutionContainmentRequest,
    safe_summary: &ToolPermissionSummary,
    decision_reasons: &[PermissionDecisionReason],
    subjects: &[ToolSubject],
    operation: Option<ToolOperation>,
    risk: Option<PermissionRisk>,
    snapshot_required: bool,
    command_family_allow_pattern: Option<String>,
) -> HttpPendingApprovalDisplay {
    let (analysis_status, analysis_reason_facts) = match analysis {
        ToolAnalysisStatus::Complete => ("complete", Vec::new()),
        ToolAnalysisStatus::Conservative { reasons } => {
            ("conservative", reasons.iter().take(8).collect())
        }
        ToolAnalysisStatus::Unsupported { reason } => ("unsupported", vec![reason]),
        ToolAnalysisStatus::Invalid { reason } => ("invalid", vec![reason]),
    };
    let analysis_reason_codes = analysis_reason_facts
        .iter()
        .map(|reason| stable_enum_label(&reason.code))
        .collect();
    let analysis_reasons = analysis_reason_facts
        .iter()
        .map(|reason| {
            reason
                .detail
                .as_deref()
                .map_or_else(|| stable_enum_label(&reason.code), bounded_approval_text)
        })
        .collect();
    HttpPendingApprovalDisplay {
        event_sequence,
        effects: effects.iter().take(16).map(stable_enum_label).collect(),
        subjects: subjects
            .iter()
            .take(16)
            .map(|subject| HttpPendingApprovalSubject {
                kind: subject.kind.as_str().to_owned(),
                scope: subject.scope.as_str().to_owned(),
                workspace_label: safe_workspace_subject_label(subject),
            })
            .collect(),
        analysis_status: analysis_status.to_owned(),
        analysis_reason_codes,
        analysis_reasons,
        containment: vec![
            format!("filesystem={}", stable_enum_label(&containment.filesystem)),
            format!("network={}", stable_enum_label(&containment.network)),
            format!("process={}", stable_enum_label(&containment.process)),
            format!(
                "environment={}",
                stable_enum_label(&containment.environment)
            ),
            format!("persistent_process={}", containment.persistent_process),
        ],
        decision_reasons: decision_reasons
            .iter()
            .take(8)
            .map(|reason| {
                if reason.detail.trim().is_empty() {
                    bounded_approval_text(&reason.code)
                } else {
                    bounded_approval_text(&reason.detail)
                }
            })
            .collect(),
        safe_summary_title: bounded_approval_text(&safe_summary.title),
        safe_summary_detail: bounded_approval_text(&safe_summary.detail),
        operation: operation.map(|value| value.as_str().to_owned()),
        risk: risk.map(|value| stable_enum_label(&value)),
        snapshot_required,
        command_family_allow_pattern,
    }
}

fn safe_workspace_subject_label(subject: &ToolSubject) -> Option<String> {
    if subject.kind != sigil_kernel::ToolSubjectKind::Path
        || subject.scope != sigil_kernel::ToolSubjectScope::Workspace
    {
        return None;
    }
    let normalized = subject.normalized.trim().trim_start_matches("./");
    let path = Path::new(normalized);
    if normalized.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(bounded_approval_text(normalized))
}

fn stable_enum_label<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn bounded_approval_text(value: &str) -> String {
    safe_persistence_text(value).chars().take(512).collect()
}

fn tool_call_hash(call: &ToolCall) -> Result<String> {
    let bytes = serde_json::to_vec(call)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn cancellation_deadline(timeout: Duration) -> Instant {
    Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now)
}

fn remaining_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn quarantine_cancellation_failure(
    registry: &HttpSessionRunRegistry,
    run_id: &str,
    acknowledgement: &std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
    error: HttpRunDriverError,
) -> HttpRunDriverError {
    let error = match registry.record_run_execution_uncertain(run_id) {
        Ok(_) => error,
        Err(quarantine_error) => HttpRunDriverError::new(format!(
            "{error}; production run quarantine failed: {quarantine_error}"
        )),
    };
    let _ = acknowledgement.send(Err(error.clone()));
    error
}

pub(crate) fn record_run_terminal_and_reconcile_stream(
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    outcome: HttpRunTerminalOutcome,
) -> Result<HttpRunSnapshot, HttpRunDriverError> {
    registry
        .record_run_terminal_with_reconciliation(run_id, outcome, || {
            let mut last_error = None;
            for _ in 0..3 {
                match event_bus.close_run_stream(durable_session_scope_id, run_id) {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        last_error = Some(error);
                        std::thread::yield_now();
                    }
                }
            }
            Err(format!(
                "terminal run stream could not be reconciled before foreground completion: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "unknown durable close failure".to_owned())
            ))
        })
        .map_err(registry_driver_error)
}

fn record_natural_terminal_if_delivered(
    control: &ApplicationRunControl,
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    result: &Result<ApplicationRunTerminalStatus>,
) -> Result<bool, HttpRunDriverError> {
    if !control
        .terminal_was_delivered()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?
    {
        return Ok(false);
    }
    record_run_terminal_and_reconcile_stream(
        registry,
        event_bus,
        durable_session_scope_id,
        run_id,
        http_terminal_from_application_result(result),
    )?;
    Ok(true)
}

fn http_terminal_from_application_result(
    result: &Result<ApplicationRunTerminalStatus>,
) -> HttpRunTerminalOutcome {
    match result {
        Ok(terminal_status) => match terminal_status {
            ApplicationRunTerminalStatus::Succeeded => HttpRunTerminalOutcome::Finished,
            ApplicationRunTerminalStatus::Interrupted => HttpRunTerminalOutcome::Interrupted,
            ApplicationRunTerminalStatus::Blocked => HttpRunTerminalOutcome::Blocked,
            ApplicationRunTerminalStatus::AwaitingUserInput => HttpRunTerminalOutcome::Paused,
        },
        Err(_) => HttpRunTerminalOutcome::Failed,
    }
}

fn ensure_application_execution_terminal<H>(
    terminal_was_delivered: bool,
    result: &Result<ApplicationRunTerminalStatus>,
    durable_session_scope_id: &str,
    run_id: &str,
    handler: &mut H,
) -> Result<HttpRunTerminalOutcome, HttpRunDriverError>
where
    H: ApplicationRunEventHandler,
{
    if terminal_was_delivered {
        return Ok(http_terminal_from_application_result(result));
    }

    let message = if result.is_ok() {
        "run execution completed before its terminal status was published"
    } else {
        "run execution failed before its terminal status was published"
    };
    handler
        .handle_public_event(PublicRunEvent::new(
            durable_session_scope_id,
            run_id,
            1,
            PublicRunEventKind::RunFailed {
                error: message.to_owned(),
            },
        ))
        .map_err(|_| {
            HttpRunDriverError::new("production execution fallback terminal publication failed")
        })?;
    Ok(HttpRunTerminalOutcome::Failed)
}

fn registry_driver_error(error: crate::HttpRegistryError) -> HttpRunDriverError {
    HttpRunDriverError::new(format!(
        "production registry terminal update failed: {error}"
    ))
}

fn stable_http_attachment_recovery_binding(
    session_scope_id: &str,
    attachment_generation: &str,
) -> String {
    sigil_runtime::interactive_session_attachment::session_attachment_recovery_binding(
        session_scope_id,
        attachment_generation,
    )
}

fn http_session_route_transition(
    transition: sigil_runtime::provider_connections::SessionRouteTransitionView,
) -> crate::HttpSessionRouteTransitionView {
    crate::HttpSessionRouteTransitionView {
        kind: match transition.kind {
            sigil_runtime::provider_connections::SessionRouteTransitionKind::Exact => {
                crate::HttpSessionRouteTransitionKind::Exact
            }
            sigil_runtime::provider_connections::SessionRouteTransitionKind::Rebound => {
                crate::HttpSessionRouteTransitionKind::Rebound
            }
            sigil_runtime::provider_connections::SessionRouteTransitionKind::ExplicitlyConfirmed => {
                crate::HttpSessionRouteTransitionKind::ExplicitlyConfirmed
            }
        },
        connection_id: transition.connection_id,
        model_id: transition.model_id,
        remote_context_reset: transition.remote_context_reset,
    }
}

fn http_public_route_transition(
    transition: sigil_kernel::PublicSessionRouteTransitionView,
) -> crate::HttpSessionRouteTransitionView {
    crate::HttpSessionRouteTransitionView {
        kind: match transition.kind {
            sigil_kernel::PublicSessionRouteTransitionKind::Exact => {
                crate::HttpSessionRouteTransitionKind::Exact
            }
            sigil_kernel::PublicSessionRouteTransitionKind::Rebound => {
                crate::HttpSessionRouteTransitionKind::Rebound
            }
            sigil_kernel::PublicSessionRouteTransitionKind::ExplicitlyConfirmed => {
                crate::HttpSessionRouteTransitionKind::ExplicitlyConfirmed
            }
        },
        connection_id: transition.connection_id,
        model_id: transition.model_id,
        remote_context_reset: transition.remote_context_reset,
    }
}

fn http_attachment_route_recovery(recovery_binding: String) -> crate::HttpSessionRouteRecoveryView {
    crate::HttpSessionRouteRecoveryView {
        code: crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive,
        allowed_actions: vec![
            crate::HttpSessionRouteRecoveryAction::RetrySessionAttach,
            crate::HttpSessionRouteRecoveryAction::StartNewSession,
            crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
        ],
        recovery_binding,
        retryable: true,
    }
}

fn http_route_recovery_from_prepare_error(
    error: &sigil_runtime::application_run::ApplicationRunPrepareError,
    fallback_recovery_binding: &str,
) -> Option<crate::HttpSessionRouteRecoveryView> {
    use sigil_runtime::application_run::ApplicationRunPrepareErrorClass as Class;

    let recovery_binding = error
        .recovery_binding()
        .unwrap_or(fallback_recovery_binding)
        .to_owned();
    let (code, allowed_actions, retryable) = match error.class() {
        Class::SessionRouteConfirmationRequired => (
            crate::HttpSessionRouteRecoveryCode::SessionRouteConfirmationRequired,
            vec![
                crate::HttpSessionRouteRecoveryAction::ConfirmCurrentRoute,
                crate::HttpSessionRouteRecoveryAction::RepairConnection,
                crate::HttpSessionRouteRecoveryAction::SelectReplacement,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            true,
        ),
        Class::SessionRouteSelectionRequired => (
            crate::HttpSessionRouteRecoveryCode::SessionRouteSelectionRequired,
            vec![
                crate::HttpSessionRouteRecoveryAction::RepairConnection,
                crate::HttpSessionRouteRecoveryAction::SelectReplacement,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            true,
        ),
        Class::ModelRouteNotConfigured => (
            crate::HttpSessionRouteRecoveryCode::ModelRouteNotConfigured,
            vec![
                crate::HttpSessionRouteRecoveryAction::RepairConnection,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            false,
        ),
        Class::ConnectionConfigInvalid | Class::Configuration => (
            crate::HttpSessionRouteRecoveryCode::ConnectionConfigInvalid,
            vec![
                crate::HttpSessionRouteRecoveryAction::RepairConnection,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            false,
        ),
        Class::ProviderUnavailable => (
            crate::HttpSessionRouteRecoveryCode::ProviderUnavailable,
            vec![
                crate::HttpSessionRouteRecoveryAction::RetryProvider,
                crate::HttpSessionRouteRecoveryAction::RepairConnection,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            true,
        ),
        Class::SessionAlreadyActive => (
            crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive,
            vec![
                crate::HttpSessionRouteRecoveryAction::RetrySessionAttach,
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            true,
        ),
        Class::SessionWriterBusy => (
            crate::HttpSessionRouteRecoveryCode::SessionWriterBusy,
            vec![
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            true,
        ),
        Class::SessionStreamInvalid => (
            crate::HttpSessionRouteRecoveryCode::SessionStreamInvalid,
            vec![
                crate::HttpSessionRouteRecoveryAction::StartNewSession,
                crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
            ],
            false,
        ),
        Class::InvalidInvocation | Class::Execution | Class::Internal => return None,
    };
    Some(crate::HttpSessionRouteRecoveryView {
        code,
        allowed_actions,
        recovery_binding,
        retryable,
    })
}

#[cfg(test)]
#[path = "tests/production_driver_tests.rs"]
mod tests;
