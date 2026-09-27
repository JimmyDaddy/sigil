use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc as std_mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use anyhow::Context;
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
    JsonlSessionStore, ModelMessage, PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION, PermissionDecisionReason,
    PermissionRisk, PlanReviewAttemptStatus, PlanReviewProjection, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptProjection, PublicEventDeliveryReceiptV1, PublicEventOutboxProjectionV1,
    PublicEventOutboxRecorder, PublicRouteRecoveryAction, PublicRouteRecoveryCode, PublicRunEvent,
    PublicRunEventKind, RootConfig, SecretString, SessionLogEntry, SessionRef, ToolAnalysisStatus,
    ToolApproval, ToolApprovalContext, ToolApprovalUserDecision, ToolArtifactAvailability,
    ToolArtifactDescriptorV1, ToolArtifactEncoding, ToolArtifactRefV1, ToolCall, ToolOperation,
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
    application_run_start_view, application_session_frontier_view,
    application_session_has_unresolved_user_input, application_task_integration_review_view,
    application_user_input_request_view_by_key, application_verification_view,
    bind_application_session_with_model_ref_and_projection_owner,
    bind_existing_application_session,
    bind_existing_application_session_with_attachment_and_projection_owner,
    prepare_application_run, prepare_application_task_continuation,
    prepare_application_user_input_decision,
    record_application_preparation_cancellation_with_attachment,
    rerun_application_verification_with_attachment,
};
use sigil_runtime::conversation_display::ConversationDisplayProjectionError;
use sigil_runtime::{LocalSessionLifecycleService, LocalSessionReopenError};
use tokio::{runtime::Handle, sync::mpsc};

#[path = "production_run_start.rs"]
mod run_start;
use run_start::{bind_run_start_route, http_route_recovery};

#[path = "production_active_runs.rs"]
mod active_runs;
use active_runs::HttpActiveRunsReady;

#[path = "production_terminal_io.rs"]
mod terminal_io;
use terminal_io::HttpRunTerminalIo;

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
    HttpQueuedRunDriverStart, HttpRegistryError, HttpRunAdmissionError, HttpRunContextView,
    HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel, HttpRunDriverError,
    HttpRunDriverStart, HttpRunDriverTaskPause, HttpRunDriverTerminalTaskCancel, HttpRunSnapshot,
    HttpRunStartRequest, HttpRunTerminalOutcome, HttpSessionBinding, HttpSessionOpenBindingError,
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
    fn session_projection_owner(&self) -> sigil_runtime::RuntimeSessionProjectionOwner {
        match self {
            Self::Conversation(prepared) => prepared.session_projection_owner(),
            Self::Task(prepared) => prepared.session_projection_owner(),
        }
    }

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

    fn terminal_control(&self) -> Option<ApplicationTerminalTaskControl> {
        match self {
            Self::Conversation(prepared) => prepared.terminal_control(),
            Self::Task(prepared) => prepared.terminal_control(),
        }
    }

    fn tool_artifact_store(&self) -> Option<sigil_kernel::ToolArtifactStore> {
        match self {
            Self::Conversation(prepared) => prepared.tool_artifact_store(),
            Self::Task(_) => None,
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
    authority_ready: bool,
    authority_recovery_code: HttpSessionRouteRecoveryCode,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    runtime: Handle,
    registry: OnceLock<Weak<HttpSessionRunRegistry>>,
    application_reservations: OnceLock<Arc<sigil_runtime::ManagedApplicationReservationStore>>,
    application_delivery_acks:
        Mutex<BTreeMap<String, Arc<sigil_runtime::RuntimeApplicationDeliveryAckStore>>>,
    active_runs: Arc<Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: Arc<HttpActiveRunsReady>,
    active_artifact_stores: Arc<Mutex<BTreeMap<String, sigil_kernel::ToolArtifactStore>>>,
    artifact_access: Arc<ArtifactAccessCoordinator>,
    terminal_owners: Arc<Mutex<BTreeMap<String, HttpProductionTerminalOwner>>>,
    exact_queue_prompts: Arc<Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>>,
    pending_compactions: Arc<Mutex<BTreeMap<String, PendingHttpCompaction>>>,
    session_attachments: Mutex<BTreeMap<String, HttpAttachedSession>>,
    session_projection_stores: Mutex<HttpSessionProjectionStoreCache>,
    readonly_query_owners: Mutex<HttpSessionQueryOwnerCache>,
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
    fn bind_live_preview_source(
        &mut self,
        source: sigil_runtime::RuntimeLivePreviewSource,
    ) -> Result<()> {
        if source.session_id() != self.durable_session_scope_id || source.run_id() != self.run_id {
            anyhow::bail!("plan review revision live source scope mismatch");
        }
        self.event_bus
            .bind_live_preview_source(source)
            .map_err(anyhow::Error::new)
    }

    fn handle_public_event(&mut self, event: sigil_kernel::PublicRunEvent) -> anyhow::Result<()> {
        if event.session_id != self.durable_session_scope_id || event.run_id != self.run_id {
            anyhow::bail!("plan review revision event scope mismatch");
        }
        // The runtime bridge is the only sequence owner for this revision. Re-numbering here
        // would make its later durable terminal outbox disagree with the already emitted stream.
        // The shared exact helper also accepts journal bytes committed by a prior process before
        // its outbox delivery receipt was persisted.
        publish_exact_http_outbox_event(
            &self.event_bus,
            &self.durable_session_scope_id,
            &self.run_id,
            event,
        )
        .map(|_| ())
    }

    fn public_event_adapter_id(&self) -> &'static str {
        "http"
    }
}

fn revision_attempt_is_exact_waiting_input(
    session_log_path: &Path,
    request: &sigil_runtime::PlanReviewRunRequest,
) -> Result<bool> {
    let session = sigil_kernel::Session::load_from_store(
        "http-plan-review-admission",
        "unknown",
        JsonlSessionStore::new(session_log_path)?,
    )?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    if projection.has_conflicts() {
        anyhow::bail!("plan review projection is conflicted before supervised revision admission");
    }
    let Some(attempt) = projection.latest_attempt(&request.plan_review_id) else {
        return Ok(false);
    };
    if attempt.attempt_id != request.attempt_id {
        return Ok(false);
    }
    let exact = attempt.plan_id == request.plan_id
        && attempt.source == request.source
        && attempt.source_turn == request.source_turn
        && attempt.route_decision_id == request.route_decision_id
        && attempt.child_session_ref == request.child_session_ref
        && attempt.revision_request_id == request.revision_request_id
        && attempt.attempt_ordinal == request.attempt_ordinal
        && attempt.base_plan_id == request.base_plan_id
        && attempt.base_plan_hash == request.base_plan_hash
        && attempt.workspace_snapshot_id == request.workspace_snapshot_id;
    if !exact {
        anyhow::bail!("plan review attempt binding does not match the supervised revision request");
    }
    Ok(attempt.status == PlanReviewAttemptStatus::WaitingForInput)
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

struct HttpAttachedSession {
    attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    projection_owner: Option<HttpBoundProjectionOwner>,
    background_agent_monitor: Option<HttpBackgroundAgentMonitor>,
}

struct HttpBackgroundAgentMonitor {
    sender: tokio::sync::mpsc::UnboundedSender<HttpBackgroundAgentSignal>,
    _worker: tokio::task::JoinHandle<()>,
}

enum HttpBackgroundAgentSignal {
    Completion(sigil_kernel::AgentThreadId),
    Rescan,
}

struct HttpBackgroundAgentEventSink {
    sender: tokio::sync::mpsc::UnboundedSender<HttpBackgroundAgentSignal>,
}

impl sigil_runtime::AgentToolBackgroundEventSink for HttpBackgroundAgentEventSink {
    fn handle_agent_event(
        &self,
        _thread_id: &sigil_kernel::AgentThreadId,
        _event: sigil_kernel::RunEvent,
    ) {
    }

    fn handle_agent_status(
        &self,
        _thread_id: &sigil_kernel::AgentThreadId,
        _status: sigil_kernel::AgentThreadStatus,
        _reason: Option<String>,
    ) {
    }

    fn handle_agent_completion_ready(&self, thread_id: &sigil_kernel::AgentThreadId) {
        let _ = self
            .sender
            .send(HttpBackgroundAgentSignal::Completion(thread_id.clone()));
    }
}

struct HttpBackgroundAgentEventHandler;

impl sigil_kernel::EventHandler for HttpBackgroundAgentEventHandler {
    fn handle(&mut self, _event: sigil_kernel::RunEvent) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct HttpBoundProjectionOwner {
    durable_session_scope_id: String,
    session_log_path: PathBuf,
    owner: sigil_runtime::RuntimeSessionProjectionOwner,
}

impl HttpBoundProjectionOwner {
    fn for_session(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<sigil_runtime::RuntimeSessionProjectionOwner, HttpRunDriverError> {
        if self.durable_session_scope_id != session.durable_session_scope_id
            || self.session_log_path != Path::new(&session.session_log_path)
        {
            return Err(HttpRunDriverError::new(
                "application projection owner does not match its durable session binding",
            ));
        }
        Ok(self.owner.clone())
    }
}

async fn run_http_background_agent_monitor(
    session: crate::HttpSessionSnapshot,
    attachment: Weak<
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
    >,
    registry: Weak<HttpSessionRunRegistry>,
    active_runs: Arc<Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: Arc<HttpActiveRunsReady>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<HttpBackgroundAgentSignal>,
) {
    while let Some(signal) = receiver.recv().await {
        let _completion_thread_id = match signal {
            HttpBackgroundAgentSignal::Completion(thread_id) => Some(thread_id),
            HttpBackgroundAgentSignal::Rescan => None,
        };
        let Some(attachment) = attachment.upgrade() else {
            return;
        };
        let idle = active_runs_ready
            .wait_for_session_idle(&active_runs, &session.id)
            .await;
        if let Err(error) = idle {
            tracing::warn!(session_id = %session.durable_session_scope_id, %error, "background agent monitor could not wait for session idle");
            continue;
        }

        let background_runs = match attachment.agent_tool_background_runs() {
            Ok(owner) => owner,
            Err(error) => {
                tracing::warn!(session_id = %session.durable_session_scope_id, %error, "background agent monitor could not read its owner");
                continue;
            }
        };
        let live_threads = match background_runs.thread_ids() {
            Ok(threads) => threads,
            Err(error) => {
                tracing::warn!(session_id = %session.durable_session_scope_id, %error, "background agent monitor could not enumerate owned threads");
                continue;
            }
        };
        let store = match JsonlSessionStore::new(Path::new(&session.session_log_path)) {
            Ok(store) => store.with_live_background_agent_threads(live_threads),
            Err(error) => {
                tracing::warn!(session_id = %session.durable_session_scope_id, %error, "background agent monitor could not open the session store");
                continue;
            }
        };
        let mut durable_session = match sigil_kernel::Session::load_from_store(
            "http-background-agent-monitor",
            "unknown",
            store,
        ) {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(session_id = %session.durable_session_scope_id, %error, "background agent monitor could not restore the session");
                continue;
            }
        };
        let mut event_handler = HttpBackgroundAgentEventHandler;
        if let Err(error) = background_runs
            .collect_finished_background_runs(&mut durable_session, &mut event_handler)
            .await
        {
            tracing::warn!(session_id = %session.durable_session_scope_id, %error, "background agent monitor could not durably collect child results");
            continue;
        }
        let Some(task_id) =
            sigil_runtime::application_run::ready_direct_task_background_continuations(
                &durable_session,
            )
            .into_iter()
            .next()
        else {
            continue;
        };
        let Some(registry) = registry.upgrade() else {
            return;
        };
        let request = crate::HttpRunStartRequest {
            permission_mode: Some(crate::HttpPermissionMode::Manual),
            task_continuation: Some(crate::HttpTaskContinuationRequest {
                task_id: task_id.as_str().to_owned(),
                guidance: None,
            }),
            ..crate::HttpRunStartRequest::default()
        };
        if let Err(error) = registry.start_run(&session.id, request) {
            tracing::warn!(
                session_id = %session.durable_session_scope_id,
                task_id = %task_id.as_str(),
                %error,
                "ready Direct Task background result could not auto-continue"
            );
        }
    }
}

#[derive(Default)]
struct HttpSessionProjectionStoreCache {
    entries: BTreeMap<String, HttpRetainedSessionProjectionStore>,
    access_sequence: u64,
}

#[derive(Default)]
struct HttpSessionQueryOwnerCache {
    entries: BTreeMap<String, (HttpBoundProjectionOwner, u64)>,
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

#[derive(Default)]
struct ArtifactAccessState {
    readers: usize,
    preparing: bool,
}

/// Serializes the short authority leases used by HTTP readers with each other and with run
/// preparation. Even read-only artifact operations acquire an exclusive namespace reservation;
/// different sessions may proceed independently. The authority remains the final owner.
struct ArtifactAccessCoordinator {
    states: Mutex<BTreeMap<String, ArtifactAccessState>>,
    ready: Condvar,
}

impl ArtifactAccessCoordinator {
    fn begin_read(
        self: &Arc<Self>,
        key: &str,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<ArtifactReadPermit> {
        budget.check()?;
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            budget.check()?;
            let state = states.entry(key.to_owned()).or_default();
            if !state.preparing && state.readers == 0 {
                state.readers = state.readers.saturating_add(1);
                return Ok(ArtifactReadPermit {
                    coordinator: Arc::clone(self),
                    key: key.to_owned(),
                });
            }
            states = self
                .ready
                .wait_timeout(states, Duration::from_millis(20))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    fn begin_preparation(self: &Arc<Self>, key: &str) -> ArtifactPreparationPermit {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let state = states.entry(key.to_owned()).or_default();
            if !state.preparing && state.readers == 0 {
                state.preparing = true;
                return ArtifactPreparationPermit {
                    coordinator: Arc::clone(self),
                    key: key.to_owned(),
                    completed: false,
                };
            }
            states = self
                .ready
                .wait(states)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn release_read(&self, key: &str) {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = states.get_mut(key) {
            state.readers = state.readers.saturating_sub(1);
            if state.readers == 0 && !state.preparing {
                states.remove(key);
            }
        }
        self.ready.notify_all();
    }

    fn release_preparation(&self, key: &str) {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = states.get_mut(key) {
            state.preparing = false;
            if state.readers == 0 {
                states.remove(key);
            }
        }
        self.ready.notify_all();
    }
}

struct ArtifactReadPermit {
    coordinator: Arc<ArtifactAccessCoordinator>,
    key: String,
}

impl Drop for ArtifactReadPermit {
    fn drop(&mut self) {
        self.coordinator.release_read(&self.key);
    }
}

struct ArtifactPreparationPermit {
    coordinator: Arc<ArtifactAccessCoordinator>,
    key: String,
    completed: bool,
}

impl ArtifactPreparationPermit {
    fn complete(mut self) {
        self.completed = true;
        self.coordinator.release_preparation(&self.key);
    }
}

impl Drop for ArtifactPreparationPermit {
    fn drop(&mut self) {
        if !self.completed {
            self.coordinator.release_preparation(&self.key);
        }
    }
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

fn application_publication_driver_error(
    error: anyhow::Error,
) -> HttpConversationRecoveryDriverError {
    if error.is::<sigil_runtime::application_operation_owner::ApplicationPublicationError>() {
        HttpConversationRecoveryDriverError::Unavailable
    } else {
        HttpConversationRecoveryDriverError::StaleBinding
    }
}

fn canonical_http_session_path(session_log_path: &Path) -> Result<PathBuf> {
    Ok(JsonlSessionStore::new(session_log_path)?
        .path()
        .to_path_buf())
}

impl HttpProductionRunDriver {
    fn catalog_reference_for_session(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<SessionRef, HttpConversationRecoveryDriverError> {
        let lifecycle = self
            .options
            .session_lifecycle
            .as_ref()
            .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
        let expected_path = std::fs::canonicalize(&session.session_log_path)
            .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        lifecycle
            .catalog()
            .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?
            .entries
            .into_iter()
            .find(|entry| {
                entry.session_id.as_deref() == Some(&session.durable_session_scope_id)
                    && std::fs::canonicalize(&entry.path).is_ok_and(|path| path == expected_path)
            })
            .map(|entry| entry.session_ref)
            .ok_or(HttpConversationRecoveryDriverError::StaleBinding)
    }

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

    /// Dispatches accepted revision guidance against the current configuration. A confirmed
    /// failure before any run owner exists restores the base Plan through the runtime authority.
    fn spawn_plan_review_revision_from_current_config(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: sigil_runtime::PlanReviewRunRequest,
    ) -> Result<(), HttpRunDriverError> {
        let dispatch = sigil_kernel::RootConfig::load(&self.options.config_path)
            .and_then(|config| config.with_effective_composition())
            .map_err(|error| HttpRunDriverError::new(format!("plan review config failed: {error}")))
            .and_then(|root_config| {
                let workspace_root = sigil_kernel::resolve_workspace_root(
                    &self.options.config_path,
                    &self.options.launch_cwd,
                    &root_config.workspace.root,
                );
                self.spawn_plan_review_revision(
                    session,
                    &root_config,
                    &workspace_root,
                    request.clone(),
                )
            });
        let Err(error) = dispatch else {
            return Ok(());
        };
        let Ok(registry) = self.attached_registry() else {
            return Err(error);
        };
        let Ok(runs) = self.active_runs.lock() else {
            return Err(error);
        };
        let run_id = request.child_logical_run_id();
        if runs.contains_key(&run_id)
            || !matches!(
                registry.get_run(&run_id),
                Err(HttpRegistryError::RunNotFound { .. })
            )
        {
            // A duplicate or queued owner can still start this exact revision. Keep its accepted
            // guidance intact, including when the current caller failed to load configuration.
            return Err(error);
        }
        // Hold the active-run lock through the durable append so another dispatcher cannot
        // register the same run between the zero-owner check and its unstarted failure.
        sigil_runtime::application_run::record_application_plan_revision_dispatch_failure(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &request,
            &error.message,
        )
        .map_err(|recovery_error| {
            HttpRunDriverError::new(format!(
                "{}; revision dispatch recovery failed: {recovery_error:#}",
                error.message
            ))
        })?;
        Err(error)
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
        let waiting_attempt = revision_attempt_is_exact_waiting_input(&session_log_path, &request)
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "plan review revision durable admission failed: {error:#}"
                ))
            })?;
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
                    projection_owner: Arc::new(Mutex::new(None)),
                }),
            );
        }
        // The registry keeps an adapter checkpoint for this exact child run. A durable waiting
        // attempt may reopen only its own Paused checkpoint; all other duplicate/stale bindings
        // fail closed before the worker starts.
        if let Err(error) = registry.register_or_resume_supervised_revision_run(
            &session.id,
            &run_id,
            HttpPermissionMode::ReadOnly,
            if waiting_attempt {
                "Resume plan review after answering research input"
            } else {
                "Run plan review revision"
            },
            waiting_attempt,
        ) {
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
        let managed_command_execution = self
            .services
            .authority_composition()
            .map(|composition| Arc::clone(&composition.command_execution));
        let managed_tool_authority = self
            .services
            .authority_composition()
            .map(|composition| Arc::new(composition.tool_authority.clone()));
        let child_resource_provisioner = self
            .services
            .authority_composition()
            .map(|composition| composition.plan_review_child_resource_provisioner());
        let runtime = self.runtime.clone();
        self.runtime.spawn(async move {
            let mut held_session_attachment = Some(attachment);
            let terminal_event_bus = event_bus.clone();
            let terminal_session_log_path = session_log_path.clone();
            let terminal_session_scope_id = durable_session_scope_id.clone();
            let terminal_run_id = run_id.clone();
            let mut run: PlanReviewRevisionExecutionFuture = Box::pin(async move {
                let mut handler = HttpPlanReviewRevisionEventHandler {
                    durable_session_scope_id,
                    run_id,
                    event_bus,
                };
                sigil_runtime::application_run::execute_plan_review_revision_with_managed_execution(
                    &root_config,
                    &workspace_root,
                    &session_log_path,
                    &request,
                    &mut handler,
                    Some(cancellation_handle),
                    managed_command_execution,
                    managed_tool_authority,
                    child_resource_provisioner,
                )
                .await
            });
            let outcome = loop {
                tokio::select! {
                    biased;
                    outcome = &mut run => break outcome,
                    control = cancel_receiver.recv() => match control {
                        Some(HttpProductionRunControlCommand::Cancel(cancellation)) => {
                            let attachment = held_session_attachment.take().expect(
                                "active plan-review revision must retain its session attachment",
                            );
                            let cancellation_registry = registry.upgrade();
                            match await_plan_review_revision_cancellation(
                                &cancellation_owner,
                                cancellation,
                                cancellation_timeout,
                                run,
                                attachment,
                                cancellation_registry.as_deref(),
                                &terminal_run_id,
                            ).await {
                                PlanReviewRevisionCancellationWait::Joined {
                                    outcome,
                                    attachment,
                                } => {
                                    held_session_attachment = Some(attachment);
                                    break *outcome;
                                }
                                PlanReviewRevisionCancellationWait::Deadline(detached) => {
                                    // The run keeps executing detached; ownership cleanup and the
                                    // terminal event happen only when it actually finishes.
                                    let active_runs = Arc::clone(&active_runs);
                                    let active_runs_ready = Arc::clone(&active_runs_ready);
                                    let late_session_log_path = terminal_session_log_path.clone();
                                    let late_registry = registry.clone();
                                    let (late_run, late_session_attachment) = detached.into_parts();
                                    runtime.spawn(async move {
                                        let _held_session_attachment = late_session_attachment;
                                        let late_outcome = late_run.await;
                                        publish_plan_review_revision_terminal(
                                            &terminal_event_bus,
                                            &late_session_log_path,
                                            &terminal_session_scope_id,
                                            &terminal_run_id,
                                            &late_outcome,
                                        );
                                        reconcile_plan_review_revision_http_registry(
                                            &late_registry,
                                            &terminal_event_bus,
                                            &terminal_session_scope_id,
                                            &terminal_run_id,
                                            &late_outcome,
                                        );
                                        release_owned_revision_run(
                                            &active_runs,
                                            &active_runs_ready,
                                            &late_registry,
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
                &terminal_session_log_path,
                &terminal_session_scope_id,
                &terminal_run_id,
                &outcome,
            );
            reconcile_plan_review_revision_http_registry(
                &registry,
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
            drop(held_session_attachment);
        });
        Ok(())
    }
}

/// Publishes the explicit terminal public event for one plan review revision run and closes its
/// SSE stream so clients observe a definitive terminal instead of a dangling live stream.
fn publish_plan_review_revision_terminal(
    event_bus: &HttpLiveEventBus,
    session_log_path: &Path,
    durable_session_scope_id: &str,
    run_id: &str,
    execution: &std::result::Result<
        sigil_runtime::application_run::PlanReviewRevisionExecution,
        anyhow::Error,
    >,
) {
    let Ok(execution) = execution else {
        // An execution error before a revision terminal bundle exists is not a domain terminal.
        // The durable coordinator/recovery path owns any later typed conclusion.
        return;
    };
    let Some(outbox) = execution.terminal_outbox.as_ref() else {
        // WaitingForInput is a resumable attempt transition, not the one revision finalizer. Its
        // broader nonterminal delivery treatment is intentionally outside A1.
        return;
    };
    let _ = publish_exact_plan_review_revision_terminal_outbox(
        event_bus,
        session_log_path,
        durable_session_scope_id,
        run_id,
        outbox,
    );
}

fn publish_exact_plan_review_revision_terminal_outbox(
    event_bus: &HttpLiveEventBus,
    session_log_path: &Path,
    durable_session_scope_id: &str,
    run_id: &str,
    outbox: &sigil_kernel::PublicEventOutboxEntryV1,
) -> Result<()> {
    if outbox.event.session_id != durable_session_scope_id
        || outbox.run_id != run_id
        || outbox.event.run_id != run_id
    {
        return Err(anyhow!(
            "plan-review revision terminal outbox belongs to another HTTP run"
        ));
    }
    publish_exact_http_outbox_event(
        event_bus,
        durable_session_scope_id,
        run_id,
        outbox.event.clone(),
    )?;
    record_plan_review_revision_terminal_delivery_receipt(session_log_path, outbox)?;
    let close_result = event_bus.close_run_stream(durable_session_scope_id, run_id);
    close_result.map_err(anyhow::Error::new)
}

/// Delivers the exact durable revision terminal and always attempts the registry/stream
/// projection afterwards. A protocol/receipt error leaves the original outbox pending for attach
/// replay, but must not strand the registered revision in `Paused` or `ExecutionUncertain` when
/// its domain terminal is already known.
fn deliver_and_reconcile_plan_review_revision_terminal(
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
    session_log_path: &Path,
    durable_session_scope_id: &str,
    outbox: &sigil_kernel::PublicEventOutboxEntryV1,
    terminal_was_published: bool,
) -> Result<(), HttpRunDriverError> {
    let delivery = if terminal_was_published {
        record_plan_review_revision_terminal_delivery_receipt(session_log_path, outbox).map_err(
            |error| {
                HttpRunDriverError::new(format!(
                    "durable plan-review revision delivery receipt failed: {error:#}"
                ))
            },
        )
    } else {
        publish_exact_plan_review_revision_terminal_outbox(
            event_bus,
            session_log_path,
            durable_session_scope_id,
            &outbox.run_id,
            outbox,
        )
        .map_err(|error| {
            HttpRunDriverError::new(format!(
                "durable plan-review revision terminal delivery failed: {error:#}"
            ))
        })
    };
    let registry_reconciliation = reconcile_plan_review_revision_terminal_registry_event(
        registry,
        event_bus,
        durable_session_scope_id,
        &outbox.run_id,
        &outbox.event,
    );
    match (delivery, registry_reconciliation) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(delivery_error), Ok(())) => Err(delivery_error),
        (Ok(()), Err(registry_error)) => Err(registry_error),
        (Err(delivery_error), Err(registry_error)) => Err(HttpRunDriverError::new(format!(
            "durable plan-review revision delivery failed ({delivery_error}); registry reconciliation also failed ({registry_error})"
        ))),
    }
}

/// Records the HTTP receipt only after the exact terminal event reached its canonical journal.
/// A receipt failure leaves the original outbox pending for idempotent replay.
fn record_plan_review_revision_terminal_delivery_receipt(
    session_log_path: &Path,
    outbox: &sigil_kernel::PublicEventOutboxEntryV1,
) -> Result<()> {
    let recorder = PublicEventOutboxRecorder::new(JsonlSessionStore::new(session_log_path)?);
    let receipt = PublicEventDeliveryReceiptV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: outbox.public_event_id.clone(),
        adapter: "http".to_owned(),
        delivered_at_unix_ms: current_unix_time_ms(),
    };
    recorder.append_delivery(&receipt).map(|_| ())
}

/// Projects an already durable revision terminal into the owned HTTP run registry.  Delivery
/// failures are deliberately ignored here: the original outbox remains pending and attachment
/// recovery will replay it and re-project this same domain fact.  Nothing in this adapter path
/// writes a replacement terminal.
fn reconcile_plan_review_revision_http_registry(
    registry: &Weak<HttpSessionRunRegistry>,
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    execution: &std::result::Result<
        sigil_runtime::application_run::PlanReviewRevisionExecution,
        anyhow::Error,
    >,
) {
    let Some(registry) = registry.upgrade() else {
        return;
    };
    let Ok(execution) = execution else {
        // A failed recovery/append leaves the domain attempt Started. Do not manufacture a
        // terminal, but never leave the adapter presenting an active worker that has unwound.
        let _ = registry.record_run_execution_uncertain(run_id);
        return;
    };
    if execution.waiting_public_event.is_some() {
        // The runtime emitted the exact awaiting event after it durably recorded the matching
        // PlanReview WaitingForInput fact. This is a resumable adapter checkpoint, not a final
        // revision outcome and must not seal the durable protocol stream.
        let _ = registry.record_supervised_revision_waiting(run_id);
        return;
    }
    if let Some(outbox) = execution.terminal_outbox.as_ref() {
        let _ = reconcile_plan_review_revision_terminal_registry_event(
            &registry,
            event_bus,
            durable_session_scope_id,
            run_id,
            &outbox.event,
        );
    }
}

fn reconcile_plan_review_revision_terminal_registry_event(
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    event: &PublicRunEvent,
) -> Result<(), HttpRunDriverError> {
    let outcome = http_terminal_from_durable_public_event(&event.event).ok_or_else(|| {
        HttpRunDriverError::new("plan-review revision terminal has no HTTP outcome")
    })?;
    registry
        .record_supervised_revision_terminal_with_reconciliation(run_id, outcome, || {
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
                "revision terminal stream could not be reconciled: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "unknown durable close failure".to_owned())
            ))
        })
        .map_err(registry_driver_error)?;
    Ok(())
}

/// Rolls back a partially registered plan review revision: removes only the run-map entry this
/// call inserted and wakes idle/shutdown waiters. Unlike [`release_owned_revision_run`], it never
/// unbinds the registry foreground slot, because the slot was not claimed yet.
fn rollback_revision_run_registration(
    active_runs: &Arc<std::sync::Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: &Arc<HttpActiveRunsReady>,
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
    active_runs_ready: &Arc<HttpActiveRunsReady>,
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
        if let Some(attached) = attachments.get(durable_session_scope_id) {
            return if attached.attachment.session_path() == canonical_session_path {
                Ok(Arc::clone(&attached.attachment))
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
        // composition, shared with CLI/TUI). A missing/invalid config may still expose the
        // bounded provider-setup recovery surface, but it never receives a runnable authority
        // route; every run/session mutation below checks `authority_ready` before proceeding.
        let mut authority_recovery_code = HttpSessionRouteRecoveryCode::AuthorityUnavailable;
        let (services, authority_ready) =
            match sigil_runtime::application_host::attach_boot_authority_to_services(
                services.clone(),
                &options.config_path,
                &options.launch_cwd,
            ) {
                Ok(services) => (services, true),
                Err(sigil_runtime::application_host::BootAuthorityErrorV1::Config(_)) => {
                    authority_recovery_code = HttpSessionRouteRecoveryCode::ConnectionConfigInvalid;
                    (services, false)
                }
                Err(error) => {
                    let detail = safe_persistence_text(&format!("{error:?}"));
                    tracing::warn!(error = %detail, "HTTP authority boot unavailable");
                    (services, false)
                }
            };
        let mut options = options;
        let current_schema = services.cutover().is_some_and(|cutover| {
            cutover.manifest().selected_epoch
                == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
        });
        if current_schema
            && let Some(composition) = services.authority_composition()
            && let Some(lifecycle) = options.session_lifecycle.take()
        {
            let lifecycle_namespace_key = lifecycle.workspace_id().to_owned();
            let lifecycle = lifecycle
                .with_managed_writer(
                    Arc::clone(&composition.storage_writer),
                    lifecycle_namespace_key,
                )
                .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
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
            authority_ready,
            authority_recovery_code,
            preparer,
            event_bus,
            runtime,
            registry: OnceLock::new(),
            application_reservations: OnceLock::new(),
            application_delivery_acks: Mutex::new(BTreeMap::new()),
            active_runs: Arc::new(Mutex::new(BTreeMap::new())),
            active_runs_ready: Arc::new(HttpActiveRunsReady::default()),
            active_artifact_stores: Arc::new(Mutex::new(BTreeMap::new())),
            artifact_access: Arc::new(ArtifactAccessCoordinator {
                states: Mutex::new(BTreeMap::new()),
                ready: Condvar::new(),
            }),
            terminal_owners: Arc::new(Mutex::new(BTreeMap::new())),
            exact_queue_prompts: Arc::new(Mutex::new(BTreeMap::new())),
            pending_compactions: Arc::new(Mutex::new(BTreeMap::new())),
            session_attachments: Mutex::new(BTreeMap::new()),
            session_projection_stores: Mutex::new(HttpSessionProjectionStoreCache::default()),
            readonly_query_owners: Mutex::new(HttpSessionQueryOwnerCache::default()),
            reconciled_terminal_sessions: Mutex::new(BTreeSet::new()),
        })
    }

    fn require_current_schema_authority(&self) -> Result<(), HttpRunDriverError> {
        if self.authority_ready {
            Ok(())
        } else {
            Err(HttpRunDriverError::new(
                "current-schema authority is unavailable; recovery setup only",
            ))
        }
    }

    fn require_current_schema_admission(&self) -> Result<(), HttpRunAdmissionError> {
        if self.authority_ready {
            Ok(())
        } else {
            Err(HttpRunAdmissionError::RouteRecovery(
                self.authority_recovery_view(),
            ))
        }
    }

    fn authority_recovery_view(&self) -> crate::HttpSessionRouteRecoveryView {
        let (allowed_actions, retryable) = match self.authority_recovery_code {
            HttpSessionRouteRecoveryCode::ConnectionConfigInvalid => (
                vec![
                    crate::HttpSessionRouteRecoveryAction::RepairConnection,
                    crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                false,
            ),
            HttpSessionRouteRecoveryCode::AuthorityUnavailable => (
                vec![
                    crate::HttpSessionRouteRecoveryAction::StartNewSession,
                    crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                false,
            ),
            _ => (
                vec![
                    crate::HttpSessionRouteRecoveryAction::StartNewSession,
                    crate::HttpSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                true,
            ),
        };
        crate::HttpSessionRouteRecoveryView {
            code: self.authority_recovery_code,
            allowed_actions,
            // Authority repair is a process-wide operation rather than a session attach, but
            // the public recovery contract still requires a bounded opaque binding. Keep it
            // path-free and stable so Desktop can safely project the recovery instead of
            // dropping it as malformed.
            recovery_binding: "authority-recovery-unavailable".to_owned(),
            retryable,
        }
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
                .attach_application_writer(
                    Arc::clone(&composition.storage_writer),
                    "http-application-reservations",
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

    fn application_reservation_store(
        &self,
    ) -> Result<Arc<sigil_runtime::ManagedApplicationReservationStore>, HttpRunDriverError> {
        if let Some(store) = self.application_reservations.get() {
            return Ok(Arc::clone(store));
        }
        let composition = self
            .services
            .authority_composition()
            .ok_or_else(|| HttpRunDriverError::new("application authority is unavailable"))?;
        let store = Arc::new(
            sigil_runtime::ManagedApplicationReservationStore::open(
                Arc::clone(&composition.storage_writer),
                "http-application",
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "application reservation journal is unavailable: {error}"
                ))
            })?,
        );
        if self
            .application_reservations
            .set(Arc::clone(&store))
            .is_err()
        {
            return self
                .application_reservations
                .get()
                .map(Arc::clone)
                .ok_or_else(|| HttpRunDriverError::new("application reservation owner lost"));
        }
        Ok(store)
    }

    fn application_context(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<crate::application_bridge::HttpApplicationContext, HttpRunDriverError> {
        self.require_current_schema_authority()?;
        let cutover = self
            .services
            .cutover()
            .ok_or_else(|| HttpRunDriverError::new("application cutover is unavailable"))?;
        let composition = self
            .services
            .authority_composition()
            .ok_or_else(|| HttpRunDriverError::new("application authority is unavailable"))?;
        let registry = self.attached_registry()?;
        let application_instance_id = cutover.manifest().application_instance_id.clone();
        let scope = crate::application_bridge::application_scope(&application_instance_id, session)
            .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
        let mut delivery_acks = self
            .application_delivery_acks
            .lock()
            .map_err(|_| HttpRunDriverError::new("application delivery ACK state unavailable"))?;
        let delivery_key = session.durable_session_scope_id.clone();
        let delivery_store = if let Some(store) = delivery_acks.get(&delivery_key) {
            Arc::clone(store)
        } else {
            if delivery_acks.len() >= MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES {
                return Err(HttpRunDriverError::new(
                    "application delivery ACK owner reached its bounded session capacity",
                ));
            }
            let store = Arc::new(
                sigil_runtime::RuntimeApplicationDeliveryAckStore::open(
                    Arc::clone(&composition.storage_writer),
                    &format!("http-application-delivery-{delivery_key}"),
                    scope,
                    1,
                )
                .map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "application delivery ACK journal is unavailable: {error}"
                    ))
                })?,
            );
            delivery_acks.insert(delivery_key, Arc::clone(&store));
            store
        };
        drop(delivery_acks);
        // Command preflight observes durable state. An idle session can lose the selected
        // attachment when another session is opened; that does not remove its read capability.
        // Dispatch still acquires the exact interactive attachment and revalidates authority.
        // query_projection_owner preserves failure when an active owner has no published handle.
        let projection_owner = Some(self.query_projection_owner(session)?);
        Ok(crate::application_bridge::HttpApplicationContext {
            config_path: self.options.config_path.clone(),
            launch_cwd: self.options.launch_cwd.clone(),
            application_instance_id,
            application_generation: cutover.manifest().application_generation,
            reservations: self.application_reservation_store()?,
            delivery_acks: delivery_store,
            registry,
            runtime: self.runtime.clone(),
            projection_owner,
        })
    }

    fn application_projection_owner(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<Option<sigil_runtime::RuntimeSessionProjectionOwner>, HttpRunDriverError> {
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("application active-run owner unavailable"))?;
        let mut has_active_run = false;
        for run in runs.values().filter(|run| run.session_id == session.id) {
            has_active_run = true;
            let owner = run
                .projection_owner
                .lock()
                .map_err(|_| HttpRunDriverError::new("application projection owner unavailable"))?;
            if let Some(owner) = owner.as_ref() {
                return owner.for_session(session).map(Some);
            }
        }
        drop(runs);
        let attachments = self
            .session_attachments
            .lock()
            .map_err(|_| HttpRunDriverError::new("application session owner unavailable"))?;
        let owner = attachments
            .get(&session.durable_session_scope_id)
            .and_then(|attached| attached.projection_owner.as_ref())
            .map(|owner| owner.for_session(session))
            .transpose()?;
        if has_active_run && owner.is_none() {
            return Err(HttpRunDriverError::new(
                "active application projection owner is not available yet",
            ));
        }
        Ok(owner)
    }

    fn query_projection_owner(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<sigil_runtime::RuntimeSessionProjectionOwner, HttpRunDriverError> {
        // An unavailable active owner fails at its own boundary; only the deliberate detached
        // browsing path receives a read-only capability, never a replacement writer.
        let owned = self.application_projection_owner(session)?;
        let mut queries = self
            .readonly_query_owners
            .lock()
            .map_err(|_| HttpRunDriverError::new("session query cache unavailable"))?;
        if let Some(owner) = owned {
            queries.entries.remove(&session.durable_session_scope_id);
            return Ok(owner);
        }
        queries.access_sequence = queries.access_sequence.saturating_add(1);
        let access_sequence = queries.access_sequence;
        if let Some((binding, last_used)) =
            queries.entries.get_mut(&session.durable_session_scope_id)
        {
            *last_used = access_sequence;
            return binding.for_session(session);
        }
        let reader = sigil_kernel::SessionRecordReadHandle::open_existing_observer(Path::new(
            &session.session_log_path,
        ))
        .map_err(|_| HttpRunDriverError::new("detached session query unavailable"))?;
        let owner = sigil_runtime::RuntimeSessionProjectionOwner::from_read_handle(reader);
        if queries.entries.len() >= MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES
            && let Some(evicted) = queries
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(scope, _)| scope.clone())
        {
            queries.entries.remove(&evicted);
        }
        queries.entries.insert(
            session.durable_session_scope_id.clone(),
            (
                HttpBoundProjectionOwner {
                    durable_session_scope_id: session.durable_session_scope_id.clone(),
                    session_log_path: PathBuf::from(&session.session_log_path),
                    owner: owner.clone(),
                },
                access_sequence,
            ),
        );
        Ok(owner)
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

    #[cfg(test)]
    pub(crate) fn read_managed_plan_review_child_artifact(
        &self,
        key: &str,
        scope_id: &str,
        session_log_path: &Path,
        artifact_ref: &ToolArtifactRefV1,
        selector: sigil_kernel::session::ToolArtifactSelectorV1,
    ) -> Result<sigil_kernel::session::ToolArtifactPageV1> {
        let composition = self
            .services
            .authority_composition()
            .context("current-schema authority composition is unavailable")?;
        let lease = sigil_runtime::managed_artifact_store::ManagedArtifactStoreLeaseV1::acquire_with_session_path(
            Arc::clone(&composition.storage_writer),
            key,
            scope_id,
            session_log_path.to_path_buf(),
        )?;
        let page = lease.store().read_page(artifact_ref, selector)?;
        lease.finalize()?;
        Ok(page)
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

        let run_context = application_run_start_view(
            &self.options.config_path,
            Path::new(&start.session.session_log_path),
            &start.session.durable_session_scope_id,
            None,
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
            review_annotations: Vec::new(),
            image_attachments: Vec::new(),
            session: start.session,
            run: start.run,
            prompt: queued.queued.prompt.clone(),
            model_ref: Some(crate::HttpProviderModelRef {
                connection_id: run_context.model_ref.connection_id.as_str().to_owned(),
                model_id: run_context.model_ref.model_id,
            }),
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
        let background_agent_events =
            self.bind_background_agent_monitor(&start.session, &session_attachment, &registry)?;
        let scan_existing_direct_task_results = start.task_continuation.is_none();
        let broker = Arc::new(HttpApprovalBroker::default());
        let (cancel_sender, cancel_receiver) = mpsc::unbounded_channel();
        let active = Arc::new(HttpProductionActiveRun {
            session_id: start.session.id.clone(),
            broker: Arc::clone(&broker),
            cancel_sender,
            projection_owner: Arc::new(Mutex::new(None)),
        });
        let projection_owner = Arc::clone(&active.projection_owner);
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
        if scan_existing_direct_task_results {
            let _ = background_agent_events.send(HttpBackgroundAgentSignal::Rescan);
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
            active_artifact_stores: Arc::clone(&self.active_artifact_stores),
            artifact_access: Arc::clone(&self.artifact_access),
            terminal_owners: Arc::clone(&self.terminal_owners),
            cancel_receiver,
            post_run_maintenance: Arc::clone(&post_run_maintenance),
            projection_owner,
        };
        let task = self.runtime.spawn(supervisor.run(preprepared));
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_ready = Arc::clone(&self.active_runs_ready);
        let active_artifact_stores = Arc::clone(&self.active_artifact_stores);
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
            if let Ok(mut stores) = active_artifact_stores.lock() {
                stores.remove(&run_id);
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
    fn bind_background_agent_monitor(
        &self,
        session: &crate::HttpSessionSnapshot,
        attachment: &Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        registry: &Arc<HttpSessionRunRegistry>,
    ) -> Result<tokio::sync::mpsc::UnboundedSender<HttpBackgroundAgentSignal>, HttpRunDriverError>
    {
        let sender = {
            let mut attachments = self
                .session_attachments
                .lock()
                .map_err(|_| HttpRunDriverError::new("application session owner unavailable"))?;
            let attached = attachments
                .get_mut(&session.durable_session_scope_id)
                .filter(|attached| Arc::ptr_eq(&attached.attachment, attachment))
                .ok_or_else(|| HttpRunDriverError::new("session attachment owner changed"))?;
            if attached.background_agent_monitor.is_none() {
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                let worker = self.runtime.spawn(run_http_background_agent_monitor(
                    session.clone(),
                    Arc::downgrade(attachment),
                    Arc::downgrade(registry),
                    Arc::clone(&self.active_runs),
                    Arc::clone(&self.active_runs_ready),
                    receiver,
                ));
                attached.background_agent_monitor = Some(HttpBackgroundAgentMonitor {
                    sender,
                    _worker: worker,
                });
            }
            attached
                .background_agent_monitor
                .as_ref()
                .expect("HTTP background monitor was installed")
                .sender
                .clone()
        };
        attachment
            .agent_tool_background_runs()
            .and_then(|owner| {
                owner.set_event_sink(Arc::new(HttpBackgroundAgentEventSink {
                    sender: sender.clone(),
                }))
            })
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "background-agent monitor binding failed: {error:#}"
                ))
            })?;
        Ok(sender)
    }

    fn install_session_attachment(
        &self,
        durable_session_scope_id: &str,
        session_log_path: &Path,
        attachment: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        projection_owner: Option<sigil_runtime::RuntimeSessionProjectionOwner>,
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
        let registry = self.registry.get().and_then(Weak::upgrade);
        replay_pending_http_public_outboxes(
            &canonical_session_path,
            durable_session_scope_id,
            &self.event_bus,
            registry.as_deref(),
        )
        .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        if let Some(registry) = registry {
            reconcile_registered_http_terminal_outboxes(
                &canonical_session_path,
                durable_session_scope_id,
                &registry,
                &self.event_bus,
            )
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        }
        let mut attachments = self
            .session_attachments
            .lock()
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        let previous = attachments
            .remove(durable_session_scope_id)
            .filter(|attached| attached.attachment.session_path() == canonical_session_path);
        let previous_owner = previous
            .as_ref()
            .and_then(|attached| attached.projection_owner.clone());
        let background_agent_monitor =
            previous.and_then(|attached| attached.background_agent_monitor);
        let projection_owner = projection_owner
            .map(|owner| HttpBoundProjectionOwner {
                durable_session_scope_id: durable_session_scope_id.to_owned(),
                session_log_path: canonical_session_path,
                owner,
            })
            .or(previous_owner);
        attachments.clear();
        attachments.insert(
            durable_session_scope_id.to_owned(),
            HttpAttachedSession {
                attachment: Arc::clone(&attachment),
                projection_owner,
                background_agent_monitor,
            },
        );
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
            None,
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
            .is_some_and(|attached| attached.attachment.session_path() == canonical_session_path);
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

    fn owned_tool_artifact_store(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<(String, sigil_kernel::ToolArtifactStore)>, HttpToolArtifactReadDriverError>
    {
        let stores = self
            .active_artifact_stores
            .lock()
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        let mut matches = session
            .run_ids
            .iter()
            .filter_map(|run_id| stores.get(run_id).map(|store| (run_id, store)));
        let owned = matches
            .next()
            .map(|(run_id, store)| (run_id.clone(), store.clone()));
        if matches.next().is_some() {
            return Err(HttpToolArtifactReadDriverError::Unavailable);
        }
        drop(stores);
        let Some((run_id, store)) = owned else {
            return Ok(None);
        };
        if store.session_scope_id() != session.durable_session_scope_id {
            return Err(HttpToolArtifactReadDriverError::Unavailable);
        }
        let expected_path = std::fs::canonicalize(&session.session_log_path)
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        let owned_path = std::fs::canonicalize(store.session_log_path())
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        if owned_path != expected_path {
            return Err(HttpToolArtifactReadDriverError::Unavailable);
        }
        Ok(Some((run_id, store)))
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
    let key = authority_artifact_store_key(services, session)?;
    let lease = sigil_runtime::managed_artifact_store::ManagedArtifactStoreLeaseV1::acquire_with_session_path(
        Arc::clone(&services.authority_composition()?.storage_writer),
        &key,
        &session.durable_session_scope_id,
        Path::new(&session.session_log_path).to_path_buf(),
    )
    .ok()?;
    Some(AuthorityArtifactStoreLease::managed(lease))
}

fn authority_artifact_store_key(
    services: &ApplicationRunServices,
    session: &crate::HttpSessionSnapshot,
) -> Option<String> {
    let current_schema = services.cutover().is_some_and(|cutover| {
        cutover.manifest().selected_epoch
            == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
    });
    let composition = services.authority_composition()?;
    let staging = sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging;
    let store = sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore;
    if !current_schema
        || !composition.declared_channels.contains(&staging)
        || !composition.declared_channels.contains(&store)
    {
        return None;
    }
    let session_log_path = Path::new(&session.session_log_path);
    let managed_session_log_root = composition
        .storage_writer
        .managed_leaf_path(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
        )
        .ok()?
        .canonicalize()
        .ok()?;
    let canonical_session_log_path = session_log_path.canonicalize().ok()?;
    let key = canonical_session_log_path
        .strip_prefix(&managed_session_log_root)
        .ok()
        .and_then(|relative| {
            let mut components = relative.components();
            let key = components.next()?.as_os_str().to_str()?;
            let leaf = components.next()?.as_os_str().to_str()?;
            (components.next().is_none() && leaf == "records.jsonl").then_some(key)
        });
    key.map(ToOwned::to_owned)
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

impl HttpProductionRunDriver {
    fn image_cache(
        &self,
    ) -> Result<sigil_runtime::ControlledImageAttachmentCache, HttpRunDriverError> {
        let config = RootConfig::load(&self.options.config_path)
            .map_err(|_| HttpRunDriverError::new("image cache configuration is unavailable"))?;
        let workspace = sigil_kernel::resolve_workspace_root(
            &self.options.config_path,
            &self.options.launch_cwd,
            &config.workspace.root,
        );
        let paths = sigil_runtime::resolve_sigil_paths(&config.storage, &config.session, workspace);
        Ok(sigil_runtime::ControlledImageAttachmentCache::new(
            paths.attachments_root,
        ))
    }
}

impl HttpRunDriver for HttpProductionRunDriver {
    fn application_operation_owner(
        &self,
        session: &HttpSessionSnapshot,
    ) -> Result<crate::driver::HttpApplicationOperationOwner, HttpRunDriverError> {
        let attachment = self
            .acquire_exact_session_attachment(
                &session.durable_session_scope_id,
                Path::new(&session.session_log_path),
            )
            .map_err(|_| {
                HttpRunDriverError::new("application session attachment is unavailable")
            })?;
        let projection_owner = if attachment.application_operation_owner().is_none() {
            let observed = bind_existing_application_session(
                &self.options.config_path,
                Path::new(&session.session_log_path),
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "application session identity inspection failed: {error}"
                ))
            })?;
            if observed.session_scope_id != session.durable_session_scope_id
                || observed.session_log_path != attachment.session_path()
            {
                return Err(HttpRunDriverError::new(
                    "application session reattachment identity changed",
                ));
            }
            // Switching away releases the idle controller. Reattach through the same host
            // binding used by open-session before preparing any operation on its new lease.
            let (binding, projection) =
                bind_existing_application_session_with_attachment_and_projection_owner(
                    &self.options.config_path,
                    Path::new(&session.session_log_path),
                    attachment.as_ref(),
                )
                .map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "application session reattachment failed: {error}"
                    ))
                })?;
            if binding.session_scope_id != session.durable_session_scope_id
                || binding.session_log_path != attachment.session_path()
            {
                return Err(HttpRunDriverError::new(
                    "application session reattachment identity changed",
                ));
            }
            Some(projection)
        } else {
            None
        };
        let attachment = self
            .install_session_attachment(
                &session.durable_session_scope_id,
                Path::new(&session.session_log_path),
                attachment,
                projection_owner,
            )
            .map_err(|_| HttpRunDriverError::new("application session owner install failed"))?;
        let owner = attachment
            .application_operation_owner()
            .ok_or_else(|| HttpRunDriverError::new("session has not issued its operation owner"))?;
        Ok(crate::driver::HttpApplicationOperationOwner {
            owner,
            _attachment: attachment,
        })
    }

    fn prepare_application_operation(
        &self,
        session: &HttpSessionSnapshot,
        binding: &sigil_kernel::ApplicationOperationBindingV1,
    ) -> Result<crate::driver::HttpApplicationOperationOwner, HttpRunDriverError> {
        let owner = self.application_operation_owner(session)?;
        let parent = owner.owner.attach_for_control().map_err(|error| {
            HttpRunDriverError::new(format!("application parent owner unavailable: {error}"))
        })?;
        let research =
            sigil_runtime::PlanReviewCoordinator::is_managed_research_application_target(
                &parent,
                &binding.target,
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!("application domain binding invalid: {error}"))
            })?;
        if research {
            let provisioner = self
                .services
                .authority_composition()
                .ok_or_else(|| {
                    HttpRunDriverError::new("application operation resource authority unavailable")
                })?
                .plan_review_child_resource_provisioner();
            sigil_runtime::PlanReviewCoordinator::prepare_managed_research_application_operation(
                &parent,
                binding,
                provisioner.as_ref(),
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!("research operation preparation failed: {error}"))
            })?
            .ok_or_else(|| HttpRunDriverError::new("research application target disappeared"))?;
        } else {
            owner.owner.prepare(binding).map_err(|error| {
                HttpRunDriverError::new(format!(
                    "application operation preparation failed: {error}"
                ))
            })?;
        }
        Ok(owner)
    }

    fn query_application_operation(
        &self,
        session: &HttpSessionSnapshot,
        binding: &sigil_kernel::ApplicationOperationBindingV1,
    ) -> Result<crate::driver::HttpApplicationOperationResolution, HttpRunDriverError> {
        let attachment = self.application_operation_owner(session)?;
        let parent = attachment.owner.attach_for_observation().map_err(|error| {
            HttpRunDriverError::new(format!("application parent owner unavailable: {error}"))
        })?;
        let research =
            sigil_runtime::PlanReviewCoordinator::is_managed_research_application_target(
                &parent,
                &binding.target,
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!("application domain binding invalid: {error}"))
            })?;
        if research {
            let provisioner = self
                .services
                .authority_composition()
                .ok_or_else(|| {
                    HttpRunDriverError::new("application operation resource authority unavailable")
                })?
                .plan_review_child_resource_provisioner();
            let (binding, proof) =
                sigil_runtime::PlanReviewCoordinator::query_managed_research_application_operation(
                    &parent,
                    binding,
                    provisioner.as_ref(),
                )
                .map_err(|error| {
                    HttpRunDriverError::new(format!("research operation query failed: {error}"))
                })?
                .ok_or_else(|| {
                    HttpRunDriverError::new("research application target disappeared")
                })?;
            return Ok(crate::driver::HttpApplicationOperationResolution { binding, proof });
        }
        let (binding, reader) = attachment
            .owner
            .observe_operation(binding)
            .map_err(|error| {
                HttpRunDriverError::new(format!("application domain owner unavailable: {error}"))
            })?;
        let mut proof = sigil_kernel::session::reconcile_application_operation(&reader, &binding)
            .map_err(|error| {
            HttpRunDriverError::new(format!("application operation query failed: {error}"))
        })?;
        if proof.is_none()
            && let sigil_kernel::ApplicationOperationTargetV1::ForkConversation {
                source_turn_digest,
                connection_id,
                model_id,
            } = &binding.target
        {
            let recover = || -> anyhow::Result<()> {
                // Binding reattachment verifies the original durable Prepared K/F. A query may
                // finish only that existing child; it never redispatches an unexecuted fork.
                let mut source = attachment.owner.attach_for_control()?;
                source.bind_application_operation(binding.clone())?;
                let lifecycle = self
                    .options
                    .session_lifecycle
                    .as_ref()
                    .ok_or_else(|| anyhow!("fork lifecycle is unavailable"))?;
                let source_ref = self
                    .catalog_reference_for_session(session)
                    .map_err(|_| anyhow!("fork source catalog binding is unavailable"))?;
                let config = RootConfig::load(&self.options.config_path)?;
                let model = sigil_kernel::ModelRef::new(
                    sigil_kernel::ConnectionId::new(connection_id.clone())?,
                    model_id.clone(),
                )?;
                if let Some(output) = lifecycle.recover_existing_fork_session_at_turn(
                    &source_ref,
                    &session.durable_session_scope_id,
                    source_turn_digest,
                    &binding.operation_id,
                    &config,
                    &model,
                )? {
                    source.append_control(ControlEntry::ConversationForkCommittedV1(
                        sigil_kernel::ConversationForkCommittedV1::from_output(
                            &session.durable_session_scope_id,
                            source_turn_digest,
                            &model,
                            &output,
                        )?,
                    ))?;
                }
                Ok(())
            };
            recover().map_err(|error| {
                HttpRunDriverError::new(format!(
                    "existing conversation fork recovery failed: {error:#}"
                ))
            })?;
            proof = sigil_kernel::session::reconcile_application_operation(&reader, &binding)
                .map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "recovered conversation fork proof is unavailable: {error}"
                    ))
                })?;
        }
        Ok(crate::driver::HttpApplicationOperationResolution { binding, proof })
    }
    fn requires_run_release_barrier(&self) -> bool {
        true
    }

    fn application_client(
        &self,
        session: &HttpSessionSnapshot,
        client_id: &str,
    ) -> Result<crate::application_bridge::HttpApplicationClient, HttpRunDriverError> {
        let context = self.application_context(session)?;
        crate::application_bridge::build_client(&context, session, client_id)
    }

    fn bind_session(
        &self,
        session_id: &str,
        model_ref: Option<&crate::HttpProviderModelRef>,
    ) -> Result<HttpSessionBinding, HttpRunDriverError> {
        self.require_current_schema_authority().map_err(|error| {
            if self.authority_ready {
                error
            } else {
                HttpRunDriverError::new(error.message)
                    .with_route_recovery(self.authority_recovery_view())
            }
        })?;
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
        let (binding, attachment, projection_owner) =
            bind_application_session_with_model_ref_and_projection_owner(
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
            Some(projection_owner),
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
        self.require_current_schema_authority()
            .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
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
        let registry = self
            .attached_registry()
            .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
        let _activation = if attachment.is_some() {
            let runs = self
                .active_runs
                .lock()
                .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
            let owns_active_run = registry.list_sessions().iter().any(|session| {
                session.durable_session_scope_id == candidate.session_id
                    && runs.values().any(|run| run.session_id == session.id)
            });
            let mutation = if owns_active_run {
                None
            } else {
                match registry.reserve_durable_session_mutation(&candidate.session_id) {
                    Ok(mutation) => Some(mutation),
                    Err(
                        HttpRegistryError::DurableSessionMutationActive
                        | HttpRegistryError::SessionForegroundRunActive { .. }
                        | HttpRegistryError::SessionRunCleanupActive { .. }
                        | HttpRegistryError::SessionVerificationActive { .. },
                    ) => None,
                    Err(_) => return Err(HttpSessionOpenBindingError::Unavailable),
                }
            };
            let Some(mutation) = mutation else {
                // An existing live or queued owner still controls this Plan. Preserve its
                // installed projection owner and return only the read binding; startup
                // inspection must not interrupt or settle work that can still complete.
                return Ok(HttpSessionBinding {
                    session_scope_id: read_binding.session_scope_id,
                    session_log_path: read_binding.session_log_path.display().to_string(),
                    route_transition: Some(http_session_route_transition(
                        read_binding.route_transition,
                    )),
                    route_recovery: None,
                });
            };
            // Registration and durable mutation admission stay excluded until activation and
            // installation finish, rather than racing a new owner after the initial check.
            Some((runs, mutation))
        } else {
            None
        };
        let (binding, route_recovery, projection_owner) = if let Some(attachment) =
            attachment.as_ref()
        {
            if let Some(composition) = self.services.authority_composition() {
                // The controller owns the exact parent attachment before reading its
                // authority-admitted child. Settle any completed draft before route
                // inspection invokes generic startup interruption recovery.
                let provisioner = composition.plan_review_child_resource_provisioner();
                sigil_runtime::PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
                        JsonlSessionStore::new(&candidate.session_log_path)
                            .map_err(|_| HttpSessionOpenBindingError::Unavailable)?,
                        provisioner.as_ref(),
                        sigil_runtime::current_unix_time_ms(),
                    )
                    .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
            }
            match bind_existing_application_session_with_attachment_and_projection_owner(
                &self.options.config_path,
                &candidate.session_log_path,
                attachment.as_ref(),
            ) {
                Ok((binding, owner)) => (binding, None, Some(owner)),
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
                    (read_binding, Some(recovery), None)
                }
            }
        } else {
            (read_binding, attachment_recovery, None)
        };
        if let Some(attachment) = attachment {
            self.install_session_attachment(
                &binding.session_scope_id,
                &binding.session_log_path,
                attachment,
                projection_owner,
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
        let plan_review_child_resource_provisioner = self
            .services
            .authority_composition()
            .map(|composition| composition.plan_review_child_resource_provisioner());
        let Some(command) = application_recoverable_user_input_decision(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            plan_review_child_resource_provisioner.as_deref(),
        )
        .map_err(|error| {
            HttpRunDriverError::new(format!("user input recovery projection failed: {error:#}"))
        })?
        else {
            return Ok(None);
        };
        Ok(Some(HttpUserInputDecisionDriverCommand {
            application_operation: None,
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
        self.require_current_schema_admission()?;
        self.acquire_session_attachment(session)?;
        let requested_model = request
            .model_ref
            .as_ref()
            .map(|model| {
                sigil_kernel::ModelRef::new(
                    sigil_kernel::ConnectionId::new(model.connection_id.clone())?,
                    model.model_id.clone(),
                )
            })
            .transpose()
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        let context = application_run_start_view(
            &self.options.config_path,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            requested_model.as_ref(),
        )
        .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        if !request.review_annotations.is_empty() {
            let workspace_root = application_recovery_workspace_root(
                &self.options.config_path,
                &self.options.launch_cwd,
            )
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
            sigil_runtime::materialize_review_annotations(
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &workspace_root,
                &request.review_annotations,
            )
            .map_err(|_| HttpRunAdmissionError::Unavailable)?;
        }
        if !request.image_attachments.is_empty() {
            use sigil_kernel::ImageAttachmentResolver as _;
            if request.agent_binding.is_some() || request.task_continuation.is_some() {
                return Err(HttpRunAdmissionError::ImageAttachmentInvalid);
            }
            let mut message = ModelMessage::user(request.prompt.clone());
            message.image_attachments = request.image_attachments.clone();
            sigil_kernel::validate_message_image_attachments(&message)
                .map_err(|_| HttpRunAdmissionError::ImageAttachmentInvalid)?;
            let cache = self
                .image_cache()
                .map_err(|_| HttpRunAdmissionError::Unavailable)?;
            for image in &message.image_attachments {
                cache
                    .resolve(image)
                    .map_err(|_| HttpRunAdmissionError::ImageAttachmentInvalid)?;
            }
            let config = RootConfig::load(&self.options.config_path)
                .map_err(|_| HttpRunAdmissionError::Unavailable)?;
            let selected = requested_model.as_ref().unwrap_or(&context.model_ref);
            let provider = sigil_runtime::build_provider_for_model_ref(&config, selected)
                .map_err(|_| HttpRunAdmissionError::Unavailable)?;
            if !provider
                .image_input_capability(&selected.model_id)
                .is_supported()
            {
                return Err(HttpRunAdmissionError::ImageInputUnsupported);
            }
        }
        let Some(recovery) = context.route_recovery.map(http_route_recovery) else {
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
                    && context.requested_model_available
            }
            HttpSessionRouteRecoveryCode::ModelRouteNotConfigured
            | HttpSessionRouteRecoveryCode::ConnectionConfigInvalid
            | HttpSessionRouteRecoveryCode::ProviderUnavailable
            | HttpSessionRouteRecoveryCode::AuthorityUnavailable
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
        self.readonly_query_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .remove(durable_session_scope_id);
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
        self.require_current_schema_authority()?;
        self.start_supervised_run(start, None, None)
    }

    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        let run = {
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
            Arc::clone(run)
        };
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
        let run = {
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
            Arc::clone(run)
        };
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
        self.require_current_schema_authority()?;
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
                HttpRunAdmissionError::Unavailable
                | HttpRunAdmissionError::ImageInputUnsupported
                | HttpRunAdmissionError::ImageAttachmentInvalid => {
                    HttpIntentStackDriverError::Unavailable
                }
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
        self.transcript_page_with_budget(
            session,
            before,
            limit,
            &sigil_kernel::SessionReadBudget::default(),
        )
    }

    fn transcript_page_with_budget(
        &self,
        session: &crate::HttpSessionSnapshot,
        before: Option<u64>,
        limit: usize,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<HttpSessionTranscriptPage, HttpRunDriverError> {
        budget
            .check()
            .map_err(|_| HttpRunDriverError::new("transcript query cancelled"))?;
        let page = self
            .query_projection_owner(session)?
            .transcript_page(&session.durable_session_scope_id, before, limit, budget)
            .map_err(|_| HttpRunDriverError::new("transcript projection failed"))?;
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

    fn ingest_image(
        &self,
        bytes: Vec<u8>,
    ) -> Result<sigil_kernel::ImageAttachment, HttpRunDriverError> {
        let cache = self.image_cache()?;
        cache
            .ingest_encoded_bytes(uuid::Uuid::new_v4().to_string(), bytes)
            .map(|attachment| attachment.without_resolved_bytes())
            .map_err(|_| {
                HttpRunDriverError::new(
                    "image could not be admitted; use a bounded PNG, JPEG, or WebP image",
                )
            })
    }

    fn message_image(
        &self,
        session: &crate::HttpSessionSnapshot,
        display_id: &str,
        attachment_id: &str,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<(String, Vec<u8>), HttpRunDriverError> {
        use sigil_kernel::ImageAttachmentResolver as _;
        let attachment = self
            .query_projection_owner(session)
            .map_err(|_| HttpRunDriverError::new("message image projection is unavailable"))?
            .message_image_attachment(
                &session.durable_session_scope_id,
                display_id,
                attachment_id,
                budget,
            )
            .map_err(|_| {
                HttpRunDriverError::new("image reference is not available in this conversation")
            })?;
        let bytes = self.image_cache()?.resolve(&attachment).map_err(|_| {
            HttpRunDriverError::new(
                "recorded image is missing or changed; attach the original again",
            )
        })?;
        Ok((attachment.mime_type.as_str().to_owned(), bytes))
    }

    fn message_content_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        query: &sigil_application::message_content::MessageContentQuery,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<
        sigil_application::message_content::MessageContentPage,
        sigil_application::message_content::MessageContentError,
    > {
        budget
            .check()
            .map_err(|_| sigil_application::message_content::MessageContentError::Unavailable)?;
        self.query_projection_owner(session)
            .map_err(|_| sigil_application::message_content::MessageContentError::Unavailable)?
            .message_content_page(&session.durable_session_scope_id, query, budget)
    }

    fn conversation_display_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HttpConversationDisplayPage, HttpConversationDisplayDriverError> {
        self.conversation_display_page_with_budget(
            session,
            cursor,
            limit,
            &sigil_kernel::SessionReadBudget::default(),
        )
    }

    fn conversation_display_page_with_budget(
        &self,
        session: &crate::HttpSessionSnapshot,
        cursor: Option<&str>,
        limit: usize,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<HttpConversationDisplayPage, HttpConversationDisplayDriverError> {
        budget
            .check()
            .map_err(|_| HttpConversationDisplayDriverError::Unavailable)?;
        let _artifact_read_permit = authority_artifact_store_key(&self.services, session)
            .map(|key| self.artifact_access.begin_read(&key, budget))
            .transpose()
            .map_err(|_| HttpConversationDisplayDriverError::Unavailable)?;
        let artifact_lease = authority_artifact_store_for_session(&self.services, session);
        let artifact_store = artifact_lease
            .as_ref()
            .map(AuthorityArtifactStoreLease::store);
        let owner = self
            .query_projection_owner(session)
            .map_err(|_| HttpConversationDisplayDriverError::Unavailable)?;
        let page = owner
            .conversation_display_page(
                sigil_runtime::ConversationDisplayQuery {
                    expected_session_scope_id: &session.durable_session_scope_id,
                    cursor,
                    limit,
                    // Display does not perform the auxiliary workspace scan. None preserves the
                    // existing conservative "snapshot unavailable / may be stale" projection;
                    // actual Plan/Task admission revalidates the workspace through its owner.
                    current_workspace_snapshot_id: None,
                    artifact_store: artifact_store.as_ref(),
                },
                budget,
            )
            .map_err(|error| match error {
                ConversationDisplayProjectionError::Corrupt { .. } => {
                    HttpConversationDisplayDriverError::Corrupt
                }
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

        let budget = sigil_kernel::SessionReadBudget::default();
        let _artifact_read_permit = authority_artifact_store_key(&self.services, session)
            .map(|key| self.artifact_access.begin_read(&key, &budget))
            .transpose()
            .map_err(|_| HttpToolArtifactReadDriverError::Unavailable)?;
        let binding = self.projected_tool_artifact_binding(session, &artifact_ref)?;
        // Terminal publication clears the foreground owner before its supervisor releases the
        // artifact lease. Reuse only an exact-session capability from its registered runs.
        let active_store = self.owned_tool_artifact_store(session)?;
        let read_from_store = |store: sigil_kernel::ToolArtifactStore| {
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
        };
        if let Some((run_id, store)) = active_store {
            match read_from_store(store) {
                Ok(page) => return Ok(page),
                Err(HttpToolArtifactReadDriverError::Unavailable) => {
                    let _ = self.wait_for_run_release(&run_id, DEFAULT_HTTP_CANCELLATION_TIMEOUT);
                }
                Err(error) => return Err(error),
            }
        }
        let artifact_lease = authority_artifact_store_for_session(&self.services, session)
            .ok_or(HttpToolArtifactReadDriverError::Unavailable)?;
        read_from_store(artifact_lease.store())
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
                provider_miss_without_local_mutation: usage.provider_miss_without_local_mutation,
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
            route_recovery: attachment_recovery
                .or_else(|| view.route_recovery.map(http_route_recovery)),
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

    fn branch_lineage(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<crate::HttpBranchLineage, HttpConversationRecoveryDriverError> {
        let lifecycle = self
            .options
            .session_lifecycle
            .as_ref()
            .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
        let reference = self.catalog_reference_for_session(session)?;
        sigil_runtime::application_branch_knowledge::application_branch_lineage_view(
            lifecycle,
            &reference,
            &session.durable_session_scope_id,
        )
        .map(Into::into)
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)
    }

    fn branch_knowledge_preview(
        &self,
        _session: &crate::HttpSessionSnapshot,
        source: &crate::HttpBranchKnowledgeSource,
    ) -> Result<crate::HttpBranchKnowledgePreview, HttpConversationRecoveryDriverError> {
        let lifecycle = self
            .options
            .session_lifecycle
            .as_ref()
            .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
        let reference = SessionRef::new_relative(&source.source_session_ref)
            .map_err(|_| HttpConversationRecoveryDriverError::StaleBinding)?;
        sigil_runtime::application_branch_knowledge::application_branch_knowledge_preview(
            lifecycle,
            &reference,
            &source.source_session_id,
        )
        .map(Into::into)
        .map_err(|_| HttpConversationRecoveryDriverError::StaleBinding)
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

    fn queued_review_context(
        &self,
        session: &crate::HttpSessionSnapshot,
        annotations: &[sigil_application::ReviewAnnotation],
    ) -> Result<String, HttpConversationRecoveryDriverError> {
        let workspace_root = application_recovery_workspace_root(
            &self.options.config_path,
            &self.options.launch_cwd,
        )
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        sigil_runtime::materialize_queued_review_annotations(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &workspace_root,
            annotations,
        )
        .map_err(|_| HttpConversationRecoveryDriverError::StaleBinding)
    }

    fn checkpoint_review(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &crate::HttpCheckpointRestoreRequest,
    ) -> Result<sigil_application::ApplicationCheckpointReview, HttpConversationRecoveryDriverError>
    {
        let workspace_root = application_recovery_workspace_root(
            &self.options.config_path,
            &self.options.launch_cwd,
        )
        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
        sigil_runtime::application_checkpoint_review(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &workspace_root,
            &request.checkpoint_id,
            &request.checkpoint_digest,
        )
        .map_err(|_| HttpConversationRecoveryDriverError::StaleBinding)
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
        let mut mutation_session = if matches!(
            command.action,
            HttpConversationRecoveryCommandAction::ImportBranchKnowledge { .. }
                | HttpConversationRecoveryCommandAction::ForkConversation { .. }
        ) {
            let owner = session_attachment
                .application_operation_owner()
                .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
            let mut writer = owner
                .attach_for_control()
                .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
            if writer.session_scope_id() != session.durable_session_scope_id {
                return Err(HttpConversationRecoveryDriverError::StaleBinding);
            }
            if let Some(binding) = &command.application_operation {
                writer
                    .bind_application_operation(binding.clone())
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
            }
            Some(writer)
        } else {
            None
        };
        let mut branch_knowledge = None;
        let mut compaction_receipt = None;
        let mut compaction_review = None;
        let mut tool_output_shrink = None;
        let mut restore_receipt = None;
        let mut fork_receipt = None;
        match &command.action {
            HttpConversationRecoveryCommandAction::ImportBranchKnowledge { selection } => {
                let lifecycle = self
                    .options
                    .session_lifecycle
                    .as_ref()
                    .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
                let target = mutation_session
                    .as_mut()
                    .ok_or(HttpConversationRecoveryDriverError::Unavailable)?;
                let request = sigil_runtime::application_branch_knowledge::ApplicationBranchKnowledgeImportRequest {
                    source_session_ref: SessionRef::new_relative(&selection.source_session_ref).map_err(|_| HttpConversationRecoveryDriverError::StaleBinding)?,
                    source_session_id: selection.source_session_id.clone(),
                    source_turn_digest: selection.source_turn_digest.clone(),
                    source_message_id: selection.source_message_id.clone(),
                    source_text_sha256: selection.source_text_sha256.clone(),
                    summary_sha256: selection.summary_sha256.clone(),
                };
                let receipt = sigil_runtime::application_branch_knowledge::import_application_branch_knowledge(lifecycle, target, &request)
                    .map_err(application_publication_driver_error)?;
                branch_knowledge = Some(crate::HttpBranchKnowledgeReceipt {
                    import_id: receipt.import_id,
                    already_imported: receipt.already_imported,
                });
            }

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
                let source_ref = self.catalog_reference_for_session(session)?;
                let root_config = RootConfig::load(&self.options.config_path)
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
                let target_model_ref = sigil_kernel::ModelRef::new(
                    sigil_kernel::ConnectionId::new(model_ref.connection_id.clone())
                        .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?,
                    model_ref.model_id.clone(),
                )
                .map_err(|_| HttpConversationRecoveryDriverError::Conflict)?;
                let artifact_budget = sigil_kernel::SessionReadBudget::default();
                let _artifact_read = authority_artifact_store_key(&self.services, session)
                    .map(|key| self.artifact_access.begin_read(&key, &artifact_budget))
                    .transpose()
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
                let source_artifacts = self
                    .owned_tool_artifact_store(session)
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?
                    .map(|(_, store)| store);
                if let Some(binding) = &command.application_operation {
                    let expected = sigil_kernel::ApplicationOperationTargetV1::ForkConversation {
                        source_turn_digest: source_turn_digest.clone(),
                        connection_id: model_ref.connection_id.clone(),
                        model_id: model_ref.model_id.clone(),
                    };
                    if binding.target != expected {
                        return Err(HttpConversationRecoveryDriverError::StaleBinding);
                    }
                }
                let legacy_key = format!("{}:{}", command.client_id, command.command_id);
                let destination_key = command
                    .application_operation
                    .as_ref()
                    .map_or(legacy_key.as_str(), |binding| binding.operation_id.as_str());
                let output = lifecycle
                    .fork_session_at_turn_with_artifacts(
                        &source_ref,
                        &session.durable_session_scope_id,
                        source_turn_digest,
                        destination_key,
                        &root_config,
                        &target_model_ref,
                        source_artifacts,
                    )
                    .map_err(|error| {
                        // Only pre-publication validation can establish no effect. Once the
                        // destination owner was entered, retain the original K/F for recovery.
                        if error.is::<sigil_runtime::application_operation_owner::ApplicationPublicationError>() {
                            HttpConversationRecoveryDriverError::Unavailable
                        } else {
                            HttpConversationRecoveryDriverError::Conflict
                        }
                    })?;
                mutation_session
                    .as_mut()
                    .ok_or(HttpConversationRecoveryDriverError::Unavailable)?
                    .append_control(sigil_kernel::ControlEntry::ConversationForkCommittedV1(
                        sigil_kernel::ConversationForkCommittedV1::from_output(
                            &session.durable_session_scope_id,
                            source_turn_digest,
                            &target_model_ref,
                            &output,
                        )
                        .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?,
                    ))
                    .map_err(|_| HttpConversationRecoveryDriverError::Unavailable)?;
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
            branch_knowledge,
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
        let attachment = self
            .acquire_session_attachment(session)
            .map_err(|error| match error {
                HttpRunAdmissionError::SessionAlreadyActive { .. }
                | HttpRunAdmissionError::RouteRecovery(_) => {
                    HttpConversationQueueDriverError::Conflict
                }
                HttpRunAdmissionError::Unavailable
                | HttpRunAdmissionError::ImageInputUnsupported
                | HttpRunAdmissionError::ImageAttachmentInvalid => {
                    HttpConversationQueueDriverError::Unavailable
                }
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
                ..
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
        let mutation = ConversationQueueMutationCommand {
            expected_queue_revision,
            mutation,
        };
        let result = if let Some(binding) = command.application_operation.as_ref() {
            attachment
                .application_operation_owner()
                .ok_or(HttpConversationQueueDriverError::Unavailable)?
                .append_queue_mutation(binding, mutation)
        } else {
            JsonlSessionStore::new(&session.session_log_path)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?
                .append_conversation_queue_mutation(mutation)
        };
        if result.is_err() {
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
                HttpRunAdmissionError::Unavailable
                | HttpRunAdmissionError::ImageInputUnsupported
                | HttpRunAdmissionError::ImageAttachmentInvalid => {
                    HttpConversationQueueDriverError::Unavailable
                }
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
        let context = application_run_start_view(
            &self.options.config_path,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            None,
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
        self.require_current_schema_authority()?;
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
        let receipt = sigil_runtime::application_plan_decision(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            &command,
        )
        .map_err(|error| HttpRunDriverError::new(format!("plan decision failed: {error}")))?;
        let revision_request = receipt.revision_request.clone();
        let mut http_receipt: HttpPlanDecisionCommandReceipt = receipt.into();
        http_receipt.session_id = session.durable_session_scope_id.clone();
        if let Some(revision_request) = revision_request {
            self.spawn_plan_review_revision_from_current_config(session, revision_request)?;
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

    fn user_input_continuation_run(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpUserInputRequest,
    ) -> Result<Option<String>, HttpRunDriverError> {
        let attachment = self.application_operation_owner(session)?;
        let parent = attachment.owner.attach_for_observation().map_err(|error| {
            HttpRunDriverError::new(format!("continuation parent observation failed: {error:#}"))
        })?;
        let candidate = match &request.source {
            sigil_kernel::UserInputSourceV1::PlanRevision { .. } => {
                sigil_runtime::PlanReviewCoordinator::accepted_revision_child_run_id(
                    &parent,
                    &request.identity,
                    &request.request_hash,
                )
                .map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "revision continuation observation failed: {error:#}"
                    ))
                })?
            }
            sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                plan_review_id,
                attempt_id,
            } => {
                // The immutable parent mirror authenticates this exact child question even
                // after its latest attempt no longer projects WaitingForInput.
                let mirrored = parent.entries().iter().any(|entry| matches!(entry,
                    sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::PlanReviewAttempt(attempt))
                    if &attempt.plan_review_id == plan_review_id && &attempt.attempt_id == attempt_id
                        && attempt.pending_user_input.as_ref().is_some_and(|pending|
                            pending.identity == request.identity && pending.request_hash == request.request_hash)
                ));
                if !mirrored {
                    return Err(HttpRunDriverError::new(
                        "research continuation has no exact parent mirror",
                    ));
                }
                Some(format!(
                    "plan-review-{}-{}",
                    plan_review_id.as_str(),
                    attempt_id.as_str()
                ))
            }
            _ => Some(
                sigil_kernel::user_input_continuation_logical_run_id(
                    &request.identity,
                    &request.request_hash,
                )
                .map_err(|error| {
                    HttpRunDriverError::new(format!("continuation identity failed: {error}"))
                })?
                .as_str()
                .to_owned(),
            ),
        };
        let Some(run_id) = candidate else {
            return Ok(None);
        };
        let registry = self.attached_registry()?;
        match registry.get_run(&run_id) {
            Ok(run) if run.session_id == session.id => Ok(Some(run.id)),
            Ok(_) => Err(HttpRunDriverError::new(
                "continuation run belongs to another HTTP session",
            )),
            Err(_) => Ok(None),
        }
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
        let request = ApplicationUserInputDecisionRequest {
            application_operation: command.application_operation.clone(),
            config_path: self.options.config_path.clone(),
            launch_cwd: self.options.launch_cwd.clone(),
            session_path: PathBuf::from(&session.session_log_path),
            session_attachment: Some(Arc::clone(&attachment)),
            expected_session_scope_id: session.durable_session_scope_id.clone(),
            run_id: run_id.clone(),
            identity: exact.identity.clone(),
            request_hash: exact.request_hash.clone(),
            command_id: sigil_kernel::UserInputCommandId::new(command.command_id.clone()).map_err(
                |error| HttpRunDriverError::new(format!("user input command id failed: {error}")),
            )?,
            decision: command.request.decision.clone(),
            interaction: ApplicationRunInteraction::ExternallyInteractive,
            permission_mode: command.request.permission_mode.map(Into::into),
        };
        // The runtime allocates the next sequence from the durable public outbox as it commits
        // the cancellation terminal. The HTTP journal is only the adapter replay projection: it
        // cannot reserve this sequence because a previously durable Waiting event may not yet
        // have reached the journal.
        let prepared = self
            .runtime
            .block_on(self.preparer.prepare_user_input(request, services))
            .map_err(|error| {
                HttpRunDriverError::new(format!("user input decision failed: {error:#}"))
            })?;
        let revision_terminal_outbox = prepared.revision_terminal_outbox().cloned();
        let (receipt, continuation, revision_request) = prepared.into_parts();
        let continuation_run_id = continuation.as_ref().map(|_| run_id.clone()).or_else(|| {
            revision_request
                .as_ref()
                .map(sigil_runtime::PlanReviewRunRequest::child_logical_run_id)
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
                review_annotations: Vec::new(),
                image_attachments: Vec::new(),
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
            self.spawn_plan_review_revision_from_current_config(session, revision_request)?;
        }
        if let Some(outbox) = revision_terminal_outbox {
            deliver_and_reconcile_plan_review_revision_terminal(
                &registry,
                &self.event_bus,
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &outbox,
                false,
            )?;
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
    HttpConversationQueueGeneration(
        sigil_application::queue_generation(revision.stream_sequence, &revision.event_id)
            .as_str()
            .to_owned(),
    )
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
    projection_owner: Arc<Mutex<Option<HttpBoundProjectionOwner>>>,
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
            sigil_runtime::application_run::ApplicationRunPrepareErrorClass::AuthorityUnavailable => {
                return PublicRunEventKind::RouteRecoveryRequired {
                    code: PublicRouteRecoveryCode::AuthorityUnavailable,
                    actions: vec![
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

type PlanReviewRevisionExecutionFuture = Pin<
    Box<
        dyn Future<Output = Result<sigil_runtime::application_run::PlanReviewRevisionExecution>>
            + Send,
    >,
>;

/// A revision execution that outlived HTTP cancellation acknowledgement and therefore retains
/// the sole session attachment until its actual future resolves.
struct DetachedPlanReviewRevision {
    run: PlanReviewRevisionExecutionFuture,
    attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

impl DetachedPlanReviewRevision {
    fn into_parts(
        self,
    ) -> (
        PlanReviewRevisionExecutionFuture,
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    ) {
        (self.run, self.attachment)
    }
}

enum PlanReviewRevisionCancellationWait {
    Joined {
        outcome: Box<Result<sigil_runtime::application_run::PlanReviewRevisionExecution>>,
        attachment:
            Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    },
    Deadline(DetachedPlanReviewRevision),
}

/// Cancels one revision future and either returns its observed result or transfers it, together
/// with the same durable attachment, to a late owner.  This is deliberately private to the HTTP
/// revision supervisor: it neither creates a second executor nor changes domain cancellation.
async fn await_plan_review_revision_cancellation(
    cancellation_owner: &sigil_kernel::RunCancellationOwner,
    cancellation: HttpProductionCancellationCommand,
    cancellation_timeout: Duration,
    mut run: PlanReviewRevisionExecutionFuture,
    attachment: Arc<
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
    >,
    registry: Option<&HttpSessionRunRegistry>,
    run_id: &str,
) -> PlanReviewRevisionCancellationWait {
    cancellation_owner.request_cancel();
    let deadline = cancellation_deadline(cancellation_timeout);
    match tokio::time::timeout(remaining_until(deadline), &mut run).await {
        Ok(outcome) => {
            let _ = cancellation.acknowledgement.send(Ok(()));
            PlanReviewRevisionCancellationWait::Joined {
                outcome: Box::new(outcome),
                attachment,
            }
        }
        Err(_) => {
            let error = HttpRunDriverError::new(
                "plan review revision did not quiesce before the cancellation deadline",
            );
            // The caller receives a deadline rejection while the detached revision still owns
            // its attachment.  The registry must expose uncertainty before the acknowledgement,
            // so no caller can mistake this in-flight worker for a completed terminal run.
            if let Some(registry) = registry {
                let _ = registry.record_run_execution_uncertain(run_id);
            }
            let _ = cancellation.acknowledgement.send(Err(error));
            PlanReviewRevisionCancellationWait::Deadline(DetachedPlanReviewRevision {
                run,
                attachment,
            })
        }
    }
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
    active_artifact_stores: Arc<Mutex<BTreeMap<String, sigil_kernel::ToolArtifactStore>>>,
    artifact_access: Arc<ArtifactAccessCoordinator>,
    terminal_owners: Arc<Mutex<BTreeMap<String, HttpProductionTerminalOwner>>>,
    cancel_receiver: mpsc::UnboundedReceiver<HttpProductionRunControlCommand>,
    post_run_maintenance: Arc<Mutex<Option<ApplicationPostRunMaintenance>>>,
    projection_owner: Arc<Mutex<Option<HttpBoundProjectionOwner>>>,
}

impl HttpRunSupervisor {
    fn retain_prepared_projection_owner(
        &self,
        prepared: &HttpPreparedApplicationRun,
    ) -> Result<(), HttpRunDriverError> {
        *self.projection_owner.lock().map_err(|_| {
            HttpRunDriverError::new("application prepared projection owner unavailable")
        })? = Some(HttpBoundProjectionOwner {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            session_log_path: prepared.session_log_path().to_path_buf(),
            owner: prepared.session_projection_owner(),
        });
        Ok(())
    }

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
        let terminal_io = HttpRunTerminalIo::new(
            &registry,
            &self.event_bus,
            &self.start.session,
            &self.start.run.id,
        );
        let selected_model = self.start.model_ref.as_ref();
        let model_connection_id = selected_model
            .map(|model_ref| sigil_kernel::ConnectionId::new(model_ref.connection_id.clone()))
            .transpose()
            .map_err(|error| {
                HttpRunDriverError::new(format!("invalid selected connection: {error}"))
            })?;
        let model_name = selected_model.map(|model_ref| model_ref.model_id.clone());
        let mut request = ApplicationRunRequest {
            review_annotations: self.start.review_annotations.clone(),
            config_path: self.options.config_path.clone(),
            launch_cwd: self.options.launch_cwd.clone(),
            prompt: self.start.prompt.clone(),
            image_attachments: self.start.image_attachments.clone(),
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
        let artifact_preparation_permit =
            authority_artifact_store_key(&self.services, &self.start.session)
                .map(|key| self.artifact_access.begin_preparation(&key));
        let mut preparation = Box::pin(async move {
            if let Some(prepared) = preprepared {
                if queued.is_some() || task_continuation.is_some() {
                    return Err(anyhow!(
                        "a pre-prepared run cannot also be queued or continue a Task"
                    ));
                }
                return Ok(prepared);
            }
            if queued.is_none()
                && task_continuation.is_none()
                && request.model_connection_id.is_none()
                && request.model_name.is_none()
            {
                bind_run_start_route(&mut request, &expected_session_scope_id).await?;
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
                    let guidance = run_start::task_review_guidance(
                        &request,
                        &expected_session_scope_id,
                        task.guidance,
                    )
                    .await?;
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
                                guidance,
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
                let _ = preparation.await;
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
                terminal_io.record(HttpRunTerminalOutcome::Failed).await?;
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
        self.retain_prepared_projection_owner(&prepared)?;
        if let Some(control) = prepared.terminal_control() {
            self.terminal_owners
                .lock()
                .map_err(|_| {
                    HttpRunDriverError::new("production terminal-owner state unavailable")
                })?
                .insert(
                    self.start.run.id.clone(),
                    HttpProductionTerminalOwner {
                        session_id: self.start.session.id.clone(),
                        durable_session_scope_id: self
                            .start
                            .session
                            .durable_session_scope_id
                            .clone(),
                        control,
                    },
                );
        }
        if let Some(store) = prepared.tool_artifact_store()
            && let Ok(mut stores) = self.active_artifact_stores.lock()
        {
            stores.insert(self.start.run.id.clone(), store);
        }
        if let Some(permit) = artifact_preparation_permit {
            permit.complete();
        }
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
                let terminal = require_durable_application_terminal(
                    terminal_io.observe_execution(&control, result).await?,
                    "application execution ended",
                )?;
                terminal_io.replay().await?;
                terminal_io.record(terminal).await?;
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
                        if terminal_io.record_natural_if_committed(&control, natural_result).await? {
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
                                if terminal_io.record_natural_if_committed(&control, natural_result).await? {
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
                                    if terminal_io.record_natural_if_committed(&control, natural_result).await? {
                                        return Ok(());
                                    }
                                    return Err(error);
                                }
                            };
                            let terminal = match terminal_io.observe_execution(&control, natural_result).await {
                                Ok(Some(terminal)) => terminal,
                                Ok(None) => {
                                    let error = HttpRunDriverError::new(
                                        "natural run completion won cancellation without an atomically committed domain terminal and public outbox",
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
                                Err(error) => {
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
                            if let Err(error) = terminal_io.replay().await {
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
                            if let Err(error) = terminal_io.record(terminal).await {
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
                let terminal = match terminal_io.observe(&control).await {
                    Ok(Some(observed)) if observed == terminal => observed,
                    Ok(Some(_)) => {
                        let error = HttpRunDriverError::new(
                            "production cancellation outcome conflicts with its durable application terminal",
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
                    Ok(None) => {
                        let error = HttpRunDriverError::new(
                            "production cancellation ended without an atomically committed domain terminal and public outbox",
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
                    Err(error) => {
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
                if let Err(error) = terminal_io.replay().await {
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
                if let Err(error) = terminal_io.record(terminal).await {
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
        if let Ok(mut stores) = self.active_artifact_stores.lock() {
            stores.remove(&self.start.run.id);
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
        let terminal_io = HttpRunTerminalIo::new(
            registry,
            &self.event_bus,
            &self.start.session,
            &self.start.run.id,
        );
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
                    if terminal_io
                        .record_natural_if_committed(&control, natural_result)
                        .await?
                    {
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
                                if terminal_io
                                    .record_natural_if_committed(&control, natural_result)
                                    .await?
                                {
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
        let terminal = match terminal_io.observe(&control).await {
            Ok(Some(observed)) if observed == terminal => observed,
            Ok(Some(_)) => {
                let error = HttpRunDriverError::new(
                    "production Task pause outcome conflicts with its durable application terminal",
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
            Ok(None) => {
                let error = HttpRunDriverError::new(
                    "production Task pause ended without an atomically committed domain terminal and public outbox",
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
            Err(error) => {
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
        };
        if let Err(error) = terminal_io.replay().await {
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
        if let Err(error) = terminal_io.record(terminal).await {
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
        let terminal_io = HttpRunTerminalIo::new(
            registry,
            &self.event_bus,
            &self.start.session,
            &self.start.run.id,
        );
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
        self.retain_prepared_projection_owner(&prepared)
            .map_err(|error| {
                quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                )
            })?;
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
        let result = async {
            let expected_terminal = finalized.map_err(|error| {
                HttpRunDriverError::new(format!(
                    "pre-execution cancellation terminal could not be durably proven: {error}"
                ))
            })?;
            let expected_terminal = match expected_terminal {
                sigil_kernel::RunCancellationTerminalOutcome::Cancelled => {
                    HttpRunTerminalOutcome::Cancelled
                }
                sigil_kernel::RunCancellationTerminalOutcome::Interrupted => {
                    HttpRunTerminalOutcome::Interrupted
                }
            };
            let terminal = terminal_io.observe(&control).await?.ok_or_else(|| {
                HttpRunDriverError::new(
                    "pre-execution cancellation ended without an atomically committed domain terminal and public outbox",
                )
            })?;
            if terminal != expected_terminal {
                return Err(HttpRunDriverError::new(
                    "pre-execution cancellation outcome conflicts with its durable application terminal",
                ));
            }
            terminal_io.replay().await?;
            terminal_io.record(terminal).await.map(|_| ())
        }.await;
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
        let terminal_io = HttpRunTerminalIo::new(
            registry,
            &self.event_bus,
            &self.start.session,
            &self.start.run.id,
        );
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
            terminal_io
                .record(HttpRunTerminalOutcome::Cancelled)
                .await
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
    fn handle_public_event(&self, public_event: PublicRunEvent) -> Result<()> {
        if public_event.session_id != self.durable_session_scope_id
            || public_event.run_id != self.run_id
        {
            return Err(anyhow!("terminal lifecycle route identity changed"));
        }
        let PublicRunEventKind::TerminalLifecycle { .. } = &public_event.event else {
            // The lifecycle wrapper first replays every older pending public outbox item through
            // this same adapter. Those events need exact delivery only: replay must not recreate
            // approval/tool/route side effects owned by the normal application handler.
            return publish_exact_http_outbox_event(
                &self.event_bus,
                &self.durable_session_scope_id,
                &self.run_id,
                public_event,
            )
            .map(|_| ());
        };
        let registry = self
            .registry
            .upgrade()
            .ok_or_else(|| anyhow!("terminal lifecycle registry is closed"))?;
        let all_terminal_tasks_settled = deliver_exact_http_terminal_lifecycle_public_event(
            &registry,
            &self.event_bus,
            &self.durable_session_scope_id,
            &self.run_id,
            public_event,
            None,
        )?;
        if all_terminal_tasks_settled {
            self.terminal_owners
                .lock()
                .map_err(|_| anyhow!("production terminal-owner state unavailable"))?
                .remove(&self.run_id);
        }
        Ok(())
    }

    fn public_event_adapter_id(&self) -> &'static str {
        "http"
    }
}

/// Projects one exact lifecycle outbox event through the registry before its HTTP receipt.
///
/// A new final lifecycle appends and seals the durable protocol stream in the same journal
/// transaction. A retry whose bytes are already retained only seals the still-open stream; it
/// never allocates a replacement sequence or replays the physical owner effect.
fn deliver_exact_http_terminal_lifecycle_public_event(
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    public_event: PublicRunEvent,
    planned_stream_close: Option<bool>,
) -> Result<bool> {
    if public_event.session_id != durable_session_scope_id || public_event.run_id != run_id {
        return Err(anyhow!("terminal lifecycle route identity changed"));
    }
    let PublicRunEventKind::TerminalLifecycle { event } = &public_event.event else {
        return Err(anyhow!("public event is not a terminal lifecycle"));
    };
    let applied = registry
        .record_terminal_lifecycle_with_publication(run_id, event, |_, should_close_stream| {
            publish_exact_http_outbox_event_with_stream_close(
                event_bus,
                durable_session_scope_id,
                run_id,
                public_event.clone(),
                planned_stream_close.unwrap_or(should_close_stream),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        })
        .map_err(|error| anyhow!(error))?;
    let all_terminal_tasks_settled = registry.get_run(run_id).ok().is_some_and(|run| {
        run.status.is_terminal()
            && !run.terminal_tasks.is_empty()
            && run
                .terminal_tasks
                .iter()
                .all(|task| task.status.is_terminal())
    });
    if applied.is_none() {
        // Registry state may already have committed before its outbox receipt. Re-check the
        // exact bytes and idempotently seal an otherwise-open final stream before ACKing.
        publish_exact_http_outbox_event_with_stream_close(
            event_bus,
            durable_session_scope_id,
            run_id,
            public_event,
            planned_stream_close.unwrap_or(all_terminal_tasks_settled),
        )?;
    }
    Ok(all_terminal_tasks_settled)
}

fn is_application_terminal_public_event(event: &PublicRunEventKind) -> bool {
    matches!(
        event,
        PublicRunEventKind::RunFinished { .. }
            | PublicRunEventKind::RunFailed { .. }
            | PublicRunEventKind::RunCancelled
            | PublicRunEventKind::RunInterrupted { .. }
            | PublicRunEventKind::RunPaused { .. }
            | PublicRunEventKind::RunBlocked { .. }
            | PublicRunEventKind::RunAwaitingUserInput { .. }
    )
}

/// Delivers one already durable public event to the HTTP adapter.
///
/// The public outbox is the durable authority for identity/sequence/payload. Durable HTTP event
/// classes are projected into the bounded replay journal exactly once; equal bytes at one
/// sequence establish a prior accepted projection. Live previews use the separate typed source.
fn publish_exact_http_outbox_event(
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    event: PublicRunEvent,
) -> Result<crate::HttpProtocolEvent> {
    publish_exact_http_outbox_event_with_stream_close(
        event_bus,
        durable_session_scope_id,
        run_id,
        event,
        false,
    )
}

fn publish_exact_http_outbox_event_with_stream_close(
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    run_id: &str,
    event: PublicRunEvent,
    close_stream_after_event: bool,
) -> Result<crate::HttpProtocolEvent> {
    if event.session_id != durable_session_scope_id || event.run_id != run_id {
        return Err(anyhow!(
            "pending public outbox event belongs to another production run"
        ));
    }
    let canonical = crate::HttpProtocolEvent::from_run_event(event.clone())?;
    let existing = event_bus
        .retained_run_event_at(durable_session_scope_id, run_id, event.sequence)
        .map_err(|error| anyhow!("HTTP public replay state is unavailable: {error}"))?;
    if let Some(existing) = existing {
        if serde_json::to_value(&existing.run_event)? == serde_json::to_value(&canonical.run_event)?
        {
            if close_stream_after_event {
                event_bus
                    .close_run_stream(durable_session_scope_id, run_id)
                    .map_err(anyhow::Error::new)?;
            }
            return Ok(existing);
        }
        return Err(anyhow!(
            "HTTP replay sequence conflicts with the durable public outbox payload"
        ));
    }
    let awaiting_input = matches!(
        &event.event,
        PublicRunEventKind::RunAwaitingUserInput { .. }
    );
    let protocol = if close_stream_after_event {
        event_bus.publish_run_event_and_close_stream(event)
    } else if is_application_terminal_public_event(&event.event) {
        // Root/revision terminal handling remains owned by A1.  Do not let this adapter replay
        // close the stream before its registry/lifecycle owner makes that decision.
        event_bus.publish_run_event_with_stream_continuation(event)
    } else {
        event_bus.publish_run_event(event)
    }
    .map_err(anyhow::Error::new)?;
    if awaiting_input {
        // A revision question ends the current live SSE response but keeps the durable protocol
        // stream open: an exact answer resumes this same run with the next public sequence.
        event_bus.close_live_run_delivery(durable_session_scope_id, run_id)?;
    }
    Ok(protocol)
}

impl ApplicationRunEventHandler for HttpProductionEventHandler {
    fn bind_live_preview_source(
        &mut self,
        source: sigil_runtime::RuntimeLivePreviewSource,
    ) -> Result<()> {
        if source.run_id() != self.run_id || source.session_id() != self.durable_session_scope_id {
            return Err(anyhow!(
                "live preview source belongs to another production owner"
            ));
        }
        self.event_bus
            .bind_live_preview_source(source)
            .map_err(anyhow::Error::new)
    }

    fn handle_live_update(&mut self, update: sigil_application::LiveRunUpdate) -> Result<()> {
        if update.run_id != self.run_id {
            return Err(anyhow!(
                "live application event belongs to another production run"
            ));
        }
        if update.session_id != self.durable_session_scope_id {
            return Err(anyhow!(
                "live application event belongs to another durable production session"
            ));
        }
        self.event_bus
            .publish_live_update(update)
            .map(|_| ())
            .map_err(anyhow::Error::new)
    }

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
        let application_terminal = is_application_terminal_public_event(&event.event);
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
                    event.sequence,
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
                        &self.durable_session_scope_id,
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
                let published = self
                    .event_bus
                    .publish_run_event_with_approval(event.clone(), Some(pending.clone()));
                if let Ok(protocol) = &published {
                    let sequence = protocol
                        .public_sequence()
                        .ok_or_else(|| anyhow!("approval publication has no public sequence"))?;
                    registry.update_approval_event_sequence(
                        &self.run_id,
                        &pending.call_id,
                        &pending.approval_request_id,
                        sequence,
                    );
                    if let Some(approval) = approval_request.as_mut() {
                        approval.display.event_sequence = sequence;
                    }
                }
                published.map(|_| ()).map_err(anyhow::Error::new)
            }
            _ if application_terminal => publish_exact_http_outbox_event(
                &self.event_bus,
                &self.durable_session_scope_id,
                &self.run_id,
                event,
            )
            .map(|_| ()),
            _ => publish_exact_http_outbox_event(
                &self.event_bus,
                &self.durable_session_scope_id,
                &self.run_id,
                event,
            )
            .map(|_| ()),
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
            return Err(error);
        }
        if let Some(transition) = route_transition {
            self.registry
                .upgrade()
                .ok_or_else(|| anyhow!("production route-transition registry is closed"))?
                .record_session_route_transition(&self.durable_session_scope_id, transition)?;
        }
        Ok(())
    }

    fn public_event_adapter_id(&self) -> &'static str {
        "http"
    }
}

struct HttpProductionApprovalHandler {
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
        if context.identity.call_id != call.id {
            return Err(anyhow!("production HTTP approval identity changed"));
        }
        let outcome = self.broker.wait_for_decision(&call.id, &context.identity)?;
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
        session_id: &str,
        call: &ToolCall,
        spec: &ToolSpec,
        identity: &ApprovalRequestIdentityV2,
        session_grant_available: bool,
        session_grant_unavailable_reason: Option<
            sigil_kernel::ToolApprovalSessionGrantUnavailableReason,
        >,
        display: HttpPendingApprovalDisplay,
    ) -> Result<HttpPendingApproval> {
        if identity.session_id != session_id
            || identity.run_id.trim().is_empty()
            || identity.call_id != call.id
        {
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
            identity: identity.clone(),
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
        identity: &ApprovalRequestIdentityV2,
    ) -> Result<HttpApprovalWaitOutcome> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?
            .get(&identity.approval_request_id)
            .cloned()
            .ok_or_else(|| anyhow!("production approval slot is missing"))?;
        if slot.call_id != call_id || slot.identity != *identity {
            return Err(anyhow!("production approval request identity changed"));
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
                    self.remove(&identity.approval_request_id, &slot);
                    return Ok(HttpApprovalWaitOutcome {
                        decision: Some(decision),
                    });
                }
                HttpApprovalSlotState::Cancelled => {
                    drop(state);
                    self.remove(&identity.approval_request_id, &slot);
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
    identity: ApprovalRequestIdentityV2,
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

fn record_natural_terminal_if_committed(
    control: &ApplicationRunControl,
    registry: &HttpSessionRunRegistry,
    event_bus: &Arc<HttpLiveEventBus>,
    durable_session_scope_id: &str,
    run_id: &str,
    session_log_path: &Path,
    result: &Result<ApplicationRunTerminalStatus>,
) -> Result<bool, HttpRunDriverError> {
    let Some(terminal) = durable_application_execution_terminal(control, result)? else {
        return Ok(false);
    };
    replay_pending_http_public_outbox(
        session_log_path,
        durable_session_scope_id,
        run_id,
        event_bus,
        registry,
    )?;
    record_run_terminal_and_reconcile_stream(
        registry,
        event_bus,
        durable_session_scope_id,
        run_id,
        terminal,
    )?;
    Ok(true)
}

fn replay_pending_http_public_outbox(
    session_log_path: &Path,
    durable_session_scope_id: &str,
    run_id: &str,
    event_bus: &Arc<HttpLiveEventBus>,
    registry: &HttpSessionRunRegistry,
) -> Result<usize, HttpRunDriverError> {
    replay_pending_http_public_outboxes_matching(
        session_log_path,
        durable_session_scope_id,
        event_bus,
        Some(run_id),
        Some(registry),
    )
}

fn replay_pending_http_public_outboxes(
    session_log_path: &Path,
    durable_session_scope_id: &str,
    event_bus: &Arc<HttpLiveEventBus>,
    registry: Option<&HttpSessionRunRegistry>,
) -> Result<usize, HttpRunDriverError> {
    replay_pending_http_public_outboxes_matching(
        session_log_path,
        durable_session_scope_id,
        event_bus,
        None,
        registry,
    )
}

/// Rebuilds only the registered terminal-lifecycle reducer from verified source. This does not
/// republish retained history, write any receipt, or invoke a terminal-owner cleanup effect.
fn restore_registered_http_terminal_lifecycle_projection(
    source: &[PublicRunEvent],
    session_log_path: &Path,
    durable_session_scope_id: &str,
    run_filter: Option<&str>,
    registry: &HttpSessionRunRegistry,
) -> Result<BTreeSet<(String, String, u64)>, HttpRunDriverError> {
    let canonical_session_path = canonical_http_session_path(session_log_path)
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let mut runs = BTreeSet::new();
    for event in source {
        if !matches!(&event.event, PublicRunEventKind::TerminalLifecycle { .. })
            || run_filter.is_some_and(|run_id| event.run_id != run_id)
        {
            continue;
        }
        if event.session_id != durable_session_scope_id {
            return Err(HttpRunDriverError::new(
                "terminal lifecycle outbox session does not match the attached HTTP session",
            ));
        }
        match registry.get_run(&event.run_id) {
            Ok(run) => {
                let session = registry
                    .get_session(&run.session_id)
                    .map_err(registry_driver_error)?;
                if session.durable_session_scope_id != durable_session_scope_id
                    || canonical_http_session_path(Path::new(&session.session_log_path))
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?
                        .as_path()
                        != canonical_session_path.as_path()
                {
                    return Err(HttpRunDriverError::new(
                        "registered HTTP run does not belong to the durable terminal lifecycle attachment",
                    ));
                }
                runs.insert((event.session_id.clone(), event.run_id.clone()));
            }
            Err(HttpRegistryError::RunNotFound { .. }) => {}
            Err(error) => return Err(registry_driver_error(error)),
        }
    }
    if runs.is_empty() {
        return Ok(BTreeSet::new());
    }
    registry
        .restore_terminal_lifecycle_projection_from_source(source, &runs)
        .map_err(registry_driver_error)
}

/// Replays pending public events from their durable source, rebuilding only a bounded HTTP
/// journal projection whose retained prefix no longer covers one pending durable event. The
/// source reset is never an acknowledgement: every selected source event must append before any
/// pending item is broadcast and receipted.
fn replay_pending_http_public_outboxes_matching(
    session_log_path: &Path,
    durable_session_scope_id: &str,
    event_bus: &Arc<HttpLiveEventBus>,
    run_filter: Option<&str>,
    registry: Option<&HttpSessionRunRegistry>,
) -> Result<usize, HttpRunDriverError> {
    let replay_projection_revision = event_bus.replay_projection_revision().map_err(|error| {
        HttpRunDriverError::new(format!(
            "HTTP public replay projection revision is unavailable: {error}"
        ))
    })?;
    let store = JsonlSessionStore::new(session_log_path)
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let records = store
        .read_event_records_writer()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let projection = PublicEventOutboxProjectionV1::from_records(&records)
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let pending_ids = projection
        .pending_for_adapter("http")
        .into_iter()
        .map(|entry| entry.public_event_id.as_str())
        .collect::<BTreeSet<_>>();
    let pending_entries = projection
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            pending_ids.contains(entry.public_event_id.as_str())
                && run_filter.is_none_or(|run_id| entry.run_id == run_id)
        })
        .collect::<Vec<_>>();
    for entry in &pending_entries {
        if entry.event.session_id != durable_session_scope_id {
            return Err(HttpRunDriverError::new(
                "pending public outbox session does not match the attached HTTP session",
            ));
        }
    }
    let source = projection
        .events_in_order()
        .into_iter()
        .map(|entry| entry.event.clone())
        .collect::<Vec<_>>();
    let lifecycle_close_stream_after = registry
        .map(|registry| {
            restore_registered_http_terminal_lifecycle_projection(
                &source,
                session_log_path,
                durable_session_scope_id,
                run_filter,
                registry,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let recorder = PublicEventOutboxRecorder::new(store);
    let mut expired_runs = BTreeSet::new();
    for entry in &pending_entries {
        if event_bus
            .retained_run_event_at(durable_session_scope_id, &entry.run_id, entry.sequence)
            .map_err(|error| {
                HttpRunDriverError::new(format!("HTTP public replay state is unavailable: {error}"))
            })?
            .is_none()
        {
            expired_runs.insert((entry.event.session_id.clone(), entry.run_id.clone()));
        }
    }
    let mut replayed = 0usize;
    let mut rebuilt_ids = BTreeSet::new();
    if !expired_runs.is_empty() {
        let rebuilt_pending = pending_entries
            .iter()
            .copied()
            .filter(|entry| {
                expired_runs.contains(&(entry.event.session_id.clone(), entry.run_id.clone()))
            })
            .collect::<Vec<_>>();
        let pending_by_key = rebuilt_pending
            .iter()
            .copied()
            .map(|entry| {
                (
                    (
                        entry.event.session_id.clone(),
                        entry.run_id.clone(),
                        entry.sequence,
                    ),
                    entry,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let pending_keys = pending_by_key.keys().cloned().collect::<BTreeSet<_>>();
        let source_has_lifecycle = source.iter().any(|event| {
            expired_runs.contains(&(event.session_id.clone(), event.run_id.clone()))
                && matches!(&event.event, PublicRunEventKind::TerminalLifecycle { .. })
        });
        if source_has_lifecycle && registry.is_none() {
            return Err(HttpRunDriverError::new(
                "verified terminal lifecycle rebuild requires its registered HTTP lifecycle projection",
            ));
        }
        let close_stream_after = if source_has_lifecycle {
            registry.ok_or_else(|| {
                HttpRunDriverError::new(
                    "verified terminal lifecycle rebuild requires its registered HTTP lifecycle projection",
                )
            })?;
            lifecycle_close_stream_after.clone()
        } else {
            BTreeSet::new()
        };
        let rebuilt_events = event_bus
            .rebuild_runs_from_verified_public_events(
                &source,
                &expired_runs,
                &pending_keys,
                &close_stream_after,
                replay_projection_revision.ok_or_else(|| {
                    HttpRunDriverError::new(
                        "HTTP public outbox journal rebuild requires a durable replay projection",
                    )
                })?,
            )
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "HTTP public outbox journal rebuild failed: {error}"
                ))
            })?;
        for event in rebuilt_events {
            let key = (
                event.session_id.clone(),
                event.run_id.clone(),
                event.sequence,
            );
            let entry = pending_by_key.get(&key).ok_or_else(|| {
                HttpRunDriverError::new(
                    "HTTP public outbox rebuild delivery lost its receipt identity",
                )
            })?;
            deliver_pending_http_public_outbox_event(
                registry,
                event_bus,
                durable_session_scope_id,
                entry,
                true,
                matches!(
                    &entry.event.event,
                    PublicRunEventKind::TerminalLifecycle { .. }
                )
                .then(|| close_stream_after.contains(&key)),
            )?;
            recorder
                .append_delivery(&PublicEventDeliveryReceiptV1 {
                    schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                    public_event_id: entry.public_event_id.clone(),
                    adapter: "http".to_owned(),
                    delivered_at_unix_ms: current_unix_time_ms(),
                })
                .map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "failed to persist HTTP delivery receipt for pending public outbox event {}: {error:#}",
                        entry.public_event_id
                    ))
                })?;
            rebuilt_ids.insert(entry.public_event_id.as_str());
            replayed = replayed.saturating_add(1);
        }
    }
    for entry in pending_entries
        .into_iter()
        .filter(|entry| !rebuilt_ids.contains(entry.public_event_id.as_str()))
    {
        deliver_pending_http_public_outbox_event(
            registry,
            event_bus,
            durable_session_scope_id,
            entry,
            false,
            matches!(
                &entry.event.event,
                PublicRunEventKind::TerminalLifecycle { .. }
            )
            .then(|| {
                lifecycle_close_stream_after.contains(&(
                    entry.event.session_id.clone(),
                    entry.run_id.clone(),
                    entry.sequence,
                ))
            }),
        )
        .map_err(|error| {
            HttpRunDriverError::new(format!(
                "HTTP adapter rejected pending public outbox event {}: {error:#}",
                entry.public_event_id
            ))
        })?;
        recorder
            .append_delivery(&PublicEventDeliveryReceiptV1 {
                schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                public_event_id: entry.public_event_id.clone(),
                adapter: "http".to_owned(),
                delivered_at_unix_ms: current_unix_time_ms(),
            })
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "failed to persist HTTP delivery receipt for pending public outbox event {}: {error:#}",
                    entry.public_event_id
                ))
            })?;
        replayed += 1;
    }
    Ok(replayed)
}

fn deliver_pending_http_public_outbox_event(
    registry: Option<&HttpSessionRunRegistry>,
    event_bus: &HttpLiveEventBus,
    durable_session_scope_id: &str,
    entry: &sigil_kernel::PublicEventOutboxEntryV1,
    already_live_delivered: bool,
    planned_stream_close: Option<bool>,
) -> Result<(), HttpRunDriverError> {
    if matches!(
        &entry.event.event,
        PublicRunEventKind::TerminalLifecycle { .. }
    ) {
        let registry = registry.ok_or_else(|| {
            HttpRunDriverError::new(
                "pending terminal lifecycle requires its registered HTTP lifecycle projection",
            )
        })?;
        deliver_exact_http_terminal_lifecycle_public_event(
            registry,
            event_bus,
            durable_session_scope_id,
            &entry.run_id,
            entry.event.clone(),
            planned_stream_close,
        )
        .map(|_| ())
        .map_err(|error| {
            HttpRunDriverError::new(format!(
                "HTTP adapter rejected pending terminal lifecycle {}: {error:#}",
                entry.public_event_id
            ))
        })
    } else if already_live_delivered {
        Ok(())
    } else {
        publish_exact_http_outbox_event(
            event_bus,
            durable_session_scope_id,
            &entry.run_id,
            entry.event.clone(),
        )
        .map(|_| ())
        .map_err(|error| {
            HttpRunDriverError::new(format!(
                "HTTP adapter rejected pending public outbox event {}: {error:#}",
                entry.public_event_id
            ))
        })
    }
}

/// Restores this process-local registry from already validated durable terminal pairs.
///
/// Receipt state is intentionally not consulted: a prior process can acknowledge the HTTP event
/// and still die before completing its in-memory registry transition. Only run ids that this
/// registry already owns are projected; historical durable runs remain untouched until a caller
/// explicitly re-registers them.
fn reconcile_registered_http_terminal_outboxes(
    session_log_path: &Path,
    durable_session_scope_id: &str,
    registry: &HttpSessionRunRegistry,
    event_bus: &HttpLiveEventBus,
) -> Result<usize, HttpRunDriverError> {
    let canonical_session_path = canonical_http_session_path(session_log_path)
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let store = JsonlSessionStore::new(&canonical_session_path)
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let records = store
        .read_event_records_writer()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let projection = PublicEventOutboxProjectionV1::from_records(&records)
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let mut records_by_event_id = BTreeMap::new();
    for record in &records {
        records_by_event_id
            .entry(record.stored_event().event_id.as_str())
            .or_insert(record);
    }
    let mut reconciled = 0usize;
    for entry in projection.events_in_order() {
        let revision_attempt = records_by_event_id
            .get(entry.domain_event_id.as_str())
            .map(|record| {
                record
                    .session_log_entry()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))
            })
            .transpose()?
            .and_then(|entry| match entry {
                Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))) => {
                    Some(attempt)
                }
                _ => None,
            });
        let revision_terminal = if let Some(attempt) = revision_attempt.as_ref() {
            if attempt.revision_request_id.is_some()
                && is_durable_revision_waiting_outbox(attempt, &entry.event.event)
            {
                // A revision Waiting bundle is a resumable checkpoint.  It must not enter the
                // terminal registry path merely because the public DTO is represented as a
                // paused HTTP snapshot.
                continue;
            }
            if attempt.revision_request_id.is_some()
                && !is_durable_revision_terminal_outbox(attempt, &entry.event.event)
            {
                return Err(HttpRunDriverError::new(
                    "plan-review revision outbox does not match a durable waiting or terminal attempt",
                ));
            }
            attempt.revision_request_id.is_some()
        } else {
            false
        };
        let Some(outcome) = http_terminal_from_durable_public_event(&entry.event.event) else {
            continue;
        };
        if entry.event.session_id != durable_session_scope_id {
            return Err(HttpRunDriverError::new(
                "durable terminal outbox session does not match the attached HTTP session",
            ));
        }
        match registry.get_run(&entry.run_id) {
            Ok(run) => {
                let session = registry
                    .get_session(&run.session_id)
                    .map_err(registry_driver_error)?;
                if session.durable_session_scope_id != durable_session_scope_id
                    || canonical_http_session_path(Path::new(&session.session_log_path))
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?
                        .as_path()
                        != canonical_session_path.as_path()
                {
                    return Err(HttpRunDriverError::new(
                        "registered HTTP run does not belong to the durable terminal outbox attachment",
                    ));
                }
                if revision_terminal {
                    reconcile_plan_review_revision_terminal_registry_event(
                        registry,
                        event_bus,
                        durable_session_scope_id,
                        &entry.run_id,
                        &entry.event,
                    )?;
                } else {
                    record_run_terminal_and_reconcile_stream(
                        registry,
                        event_bus,
                        durable_session_scope_id,
                        &entry.run_id,
                        outcome,
                    )?;
                }
                reconciled = reconciled.saturating_add(1);
            }
            Err(HttpRegistryError::RunNotFound { .. }) => {}
            Err(error) => return Err(registry_driver_error(error)),
        }
    }
    Ok(reconciled)
}

/// A revision waiting pair is recoverable input state, even though HTTP presents the current
/// snapshot as `Paused`.  Only its exact domain status and public payload establish that meaning.
fn is_durable_revision_waiting_outbox(
    attempt: &sigil_kernel::PlanReviewAttemptEntry,
    event: &PublicRunEventKind,
) -> bool {
    attempt.status == PlanReviewAttemptStatus::WaitingForInput
        && matches!(event, PublicRunEventKind::RunAwaitingUserInput { .. })
}

/// A PlanReview attempt can close the HTTP registry only when both durable sides agree on one
/// final revision outcome. `WaitingForInput` and `Started` are deliberately not
/// terminal registry facts.
fn is_durable_revision_terminal_outbox(
    attempt: &sigil_kernel::PlanReviewAttemptEntry,
    event: &PublicRunEventKind,
) -> bool {
    matches!(
        (attempt.status, event),
        (
            PlanReviewAttemptStatus::DraftReady | PlanReviewAttemptStatus::CompletedWithoutDraft,
            PublicRunEventKind::RunFinished { .. }
        ) | (
            PlanReviewAttemptStatus::Cancelled,
            PublicRunEventKind::RunCancelled
        ) | (
            PlanReviewAttemptStatus::Interrupted,
            PublicRunEventKind::RunInterrupted { .. }
        ) | (
            PlanReviewAttemptStatus::Blocked,
            PublicRunEventKind::RunBlocked { .. }
        ) | (
            PlanReviewAttemptStatus::Paused,
            PublicRunEventKind::RunPaused { .. }
        ) | (
            PlanReviewAttemptStatus::Failed,
            PublicRunEventKind::RunFailed { .. }
        )
    )
}

fn durable_application_execution_terminal(
    control: &ApplicationRunControl,
    result: &Result<ApplicationRunTerminalStatus>,
) -> Result<Option<HttpRunTerminalOutcome>, HttpRunDriverError> {
    let durable_status = control
        .durable_terminal_status()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    let Some(durable_status) = durable_status else {
        return Ok(None);
    };
    if let Ok(execution_status) = result
        && *execution_status != durable_status
    {
        return Err(HttpRunDriverError::new(
            "application execution result conflicts with its durable terminal",
        ));
    }
    Ok(Some(http_terminal_from_durable_application_status(
        durable_status,
    )))
}

fn durable_application_terminal(
    control: &ApplicationRunControl,
) -> Result<Option<HttpRunTerminalOutcome>, HttpRunDriverError> {
    let status = control
        .durable_terminal_status()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
    Ok(status.map(http_terminal_from_durable_application_status))
}

fn require_durable_application_terminal(
    terminal: Option<HttpRunTerminalOutcome>,
    context: &str,
) -> Result<HttpRunTerminalOutcome, HttpRunDriverError> {
    terminal.ok_or_else(|| {
        HttpRunDriverError::new(format!(
            "{context} without an atomically committed domain terminal and public outbox"
        ))
    })
}

fn http_terminal_from_durable_application_status(
    terminal_status: ApplicationRunTerminalStatus,
) -> HttpRunTerminalOutcome {
    match terminal_status {
        ApplicationRunTerminalStatus::Succeeded => HttpRunTerminalOutcome::Finished,
        ApplicationRunTerminalStatus::Failed => HttpRunTerminalOutcome::Failed,
        ApplicationRunTerminalStatus::Cancelled => HttpRunTerminalOutcome::Cancelled,
        ApplicationRunTerminalStatus::Interrupted => HttpRunTerminalOutcome::Interrupted,
        ApplicationRunTerminalStatus::Paused => HttpRunTerminalOutcome::Paused,
        ApplicationRunTerminalStatus::Blocked => HttpRunTerminalOutcome::Blocked,
        // HTTP keeps the existing snapshot contract (`Paused`), while the original durable
        // `RunAwaitingUserInput` event remains in the outbox/SSE stream for consumers that need
        // to distinguish input-required from an ordinary pause.
        ApplicationRunTerminalStatus::AwaitingUserInput => HttpRunTerminalOutcome::Paused,
    }
}

fn http_terminal_from_durable_public_event(
    event: &PublicRunEventKind,
) -> Option<HttpRunTerminalOutcome> {
    match event {
        PublicRunEventKind::RunFinished { .. } => Some(HttpRunTerminalOutcome::Finished),
        PublicRunEventKind::RunFailed { .. } => Some(HttpRunTerminalOutcome::Failed),
        PublicRunEventKind::RunCancelled => Some(HttpRunTerminalOutcome::Cancelled),
        PublicRunEventKind::RunInterrupted { .. } => Some(HttpRunTerminalOutcome::Interrupted),
        PublicRunEventKind::RunPaused { .. } => Some(HttpRunTerminalOutcome::Paused),
        PublicRunEventKind::RunBlocked { .. } => Some(HttpRunTerminalOutcome::Blocked),
        // The durable public DTO remains distinct for SSE consumers; the HTTP snapshot contract
        // represents unresolved input as its existing paused outcome.
        PublicRunEventKind::RunAwaitingUserInput { .. } => Some(HttpRunTerminalOutcome::Paused),
        _ => None,
    }
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
        Class::AuthorityUnavailable => (
            crate::HttpSessionRouteRecoveryCode::AuthorityUnavailable,
            vec![
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
