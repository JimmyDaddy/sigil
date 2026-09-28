use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentProfileId, AgentRunDisposition, AgentRunInput, AgentRunOptions, AgentRunOutcome,
    AgentRunOutput, AgentRunResult, AgentRunTerminalReason, AgentThreadStatus, ApprovalHandler,
    AssistantMessageKind, ConnectionId, ControlEntry, ConversationRunFinalizedEntryV1,
    ConversationRunLifecycleRecorder, ConversationRunStartedEntryV1, EgressDisclosurePresenter,
    EventHandler, FrozenProviderRequestMaterial, InteractionMode, JsonlSessionStore,
    McpServerStartup, MessageRole, ModelMessage, ModelRef, MutationEventRecorder, NoopEventHandler,
    PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION, PermissionMode, Provider,
    ProviderOutputPublicationIntentV1, PublicEventDeliveryReceiptV1, PublicEventOutboxEntryV1,
    PublicEventOutboxProjectionV1, PublicEventOutboxRecorder, PublicRunEvent, PublicRunEventKind,
    PublicTaskEventProjector, ReasoningEffort, ResolvedModelRoute, RootConfig,
    RunCancellationFinalizedEntry, RunCancellationHandle, RunCancellationOwner,
    RunCancellationRecorder, RunCancellationRequestedEntry, RunCancellationTarget,
    RunCancellationTerminalOutcome, RunEvent, RunQuiescenceOutcome, RunTaskGuard, SecretString,
    Session, SessionLogEntry, SessionPublicEventProjectionV1, SessionRef, TaskId, TaskPauseRequest,
    TaskRunStatus, TaskVerificationRerunRequest, ToolArtifactStore, ToolRegistryScope,
    VerificationProductView, WorkspaceTrust, rerun_task_verification_check, resolve_workspace_root,
    safe_persistence_text, verification_product_view, workspace_trust_from_entries,
};

/// The kernel owns the sole exhaustive conversation/application terminal status table.
pub use sigil_kernel::ConversationRunTerminalStatusV1 as ApplicationRunTerminalStatus;

use crate::{
    activate_eager_remote_mcp_server,
    application_queue::{ApplicationQueuedRunPrepareError, PreparedApplicationQueuedRunInput},
    attach_session_url_capability_store, context_candidates_from_safe_sources,
    current_unix_time_ms,
    product_view::{ApplicationAgentActivityView, agent_activity_product_view_from_entries},
    resolve_sigil_paths, secret_redactor_for_root_config, unsupported_mcp_elicitation_handler,
    unsupported_mcp_runtime_event_handler,
};

mod integration_control;
mod live_preview;
mod recorder;
mod run_start;
mod task_control;
mod user_input;

pub use live_preview::{RuntimeLivePreviewReader, RuntimeLivePreviewSource};
pub use recorder::ApplicationRunEventRecorder;
pub use run_start::{ApplicationRunStartView, application_run_start_view};

pub use integration_control::{
    APPLICATION_TASK_INTEGRATION_REVIEW_SCHEMA_VERSION, ApplicationIntegrationLaneCandidateKind,
    ApplicationIntegrationPromotionTargetKind, ApplicationTaskIntegrationAcceptanceView,
    ApplicationTaskIntegrationLaneView, ApplicationTaskIntegrationReviewView,
    accept_application_task_integration_review,
    accept_application_task_integration_review_with_attachment,
    application_task_integration_review_view,
};
pub use task_control::{
    ApplicationTaskContinuationExecution, ApplicationTaskContinuationOutput,
    ApplicationTaskContinuationRequest, PreparedApplicationTaskContinuation,
    prepare_application_task_continuation,
};
pub use user_input::{
    ApplicationUserInputDecisionRequest, PreparedApplicationUserInputDecision,
    application_recoverable_user_input_decision, application_session_has_unresolved_user_input,
    application_user_input_request_view, application_user_input_request_view_by_key,
    prepare_application_user_input_decision,
    recoverable_agent_user_input_decision_from_child_sessions,
};

// Timing is process-local diagnostics, never execution authority or public conversation content.
struct PreparationPhaseTimer {
    run_id: String,
    phase: sigil_kernel::run_diagnostics::RunTimingPhase,
    started: Instant,
}

impl PreparationPhaseTimer {
    fn new(run_id: &str, phase: sigil_kernel::run_diagnostics::RunTimingPhase) -> Self {
        Self {
            run_id: run_id.to_owned(),
            phase,
            started: Instant::now(),
        }
    }
}

impl Drop for PreparationPhaseTimer {
    fn drop(&mut self) {
        sigil_kernel::run_diagnostics::record_run_timing(
            &self.run_id,
            self.phase,
            self.started.elapsed(),
        );
    }
}

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

/// Read-only durable frontier for one scope-checked application session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSessionFrontierView {
    /// Durable session scope proven while reading the append-only stream.
    pub session_scope_id: String,
    /// Highest durable stream sequence visible to this read.
    pub through_stream_sequence: u64,
}

/// Stable preparation failure class used by machine adapters without parsing error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRunPrepareErrorClass {
    /// Request shape was invalid before configuration or durable state was opened.
    InvalidInvocation,
    /// Root configuration or provider construction was invalid.
    Configuration,
    /// Saved connection configuration could not be decoded or admitted.
    ConnectionConfigInvalid,
    /// The configured provider could not become ready.
    ProviderUnavailable,
    /// The authority plane could not be composed or verified.
    AuthorityUnavailable,
    /// No saved or explicit compound model route was available.
    ModelRouteNotConfigured,
    /// The current connection target needs an exact-bound user confirmation.
    SessionRouteConfirmationRequired,
    /// The saved connection is unavailable and a replacement must be selected.
    SessionRouteSelectionRequired,
    /// Another write-capable surface currently owns the durable session.
    SessionAlreadyActive,
    /// A durable writer could not be admitted after attachment ownership was established.
    SessionWriterBusy,
    /// The durable session stream could not be safely decoded.
    SessionStreamInvalid,
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
    #[error("connection configuration is invalid")]
    ConnectionConfigInvalid {
        #[source]
        source: anyhow::Error,
    },
    #[error("provider is unavailable")]
    ProviderUnavailable {
        #[source]
        source: anyhow::Error,
    },
    #[error("authority is unavailable")]
    AuthorityUnavailable {
        #[source]
        source: anyhow::Error,
    },
    /// Headless startup cannot choose a provider/model route without an explicit user decision.
    #[error("model route is not configured")]
    ModelRouteNotConfigured,
    /// The session can be read, but provider egress needs explicit confirmation.
    #[error("session route confirmation is required")]
    SessionRouteConfirmationRequired { recovery_binding: String },
    /// The saved connection can no longer be resolved.
    #[error("session route selection is required")]
    SessionRouteSelectionRequired { recovery_binding: String },
    /// Another interactive or headless owner holds the cross-process attachment.
    #[error("session is already active")]
    SessionAlreadyActive { recovery_binding: String },
    #[error("session writer is busy")]
    SessionWriterBusy { recovery_binding: String },
    #[error("session stream is invalid")]
    SessionStreamInvalid,
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
            Self::ConnectionConfigInvalid { .. } => {
                ApplicationRunPrepareErrorClass::ConnectionConfigInvalid
            }
            Self::ProviderUnavailable { .. } => {
                ApplicationRunPrepareErrorClass::ProviderUnavailable
            }
            Self::AuthorityUnavailable { .. } => {
                ApplicationRunPrepareErrorClass::AuthorityUnavailable
            }
            Self::ModelRouteNotConfigured => {
                ApplicationRunPrepareErrorClass::ModelRouteNotConfigured
            }
            Self::SessionRouteConfirmationRequired { .. } => {
                ApplicationRunPrepareErrorClass::SessionRouteConfirmationRequired
            }
            Self::SessionRouteSelectionRequired { .. } => {
                ApplicationRunPrepareErrorClass::SessionRouteSelectionRequired
            }
            Self::SessionAlreadyActive { .. } => {
                ApplicationRunPrepareErrorClass::SessionAlreadyActive
            }
            Self::SessionWriterBusy { .. } => ApplicationRunPrepareErrorClass::SessionWriterBusy,
            Self::SessionStreamInvalid => ApplicationRunPrepareErrorClass::SessionStreamInvalid,
            Self::Execution { .. } => ApplicationRunPrepareErrorClass::Execution,
            Self::Internal { .. } => ApplicationRunPrepareErrorClass::Internal,
        }
    }

    /// Returns the opaque exact recovery binding, when this failure admits a route action.
    #[must_use]
    pub fn recovery_binding(&self) -> Option<&str> {
        match self {
            Self::SessionRouteConfirmationRequired { recovery_binding }
            | Self::SessionRouteSelectionRequired { recovery_binding }
            | Self::SessionAlreadyActive { recovery_binding }
            | Self::SessionWriterBusy { recovery_binding } => Some(recovery_binding),
            _ => None,
        }
    }

    fn configuration(source: impl Into<anyhow::Error>) -> Self {
        Self::Configuration {
            source: source.into(),
        }
    }

    fn connection_config_invalid(source: impl Into<anyhow::Error>) -> Self {
        Self::ConnectionConfigInvalid {
            source: source.into(),
        }
    }

    fn provider_unavailable(source: impl Into<anyhow::Error>) -> Self {
        Self::ProviderUnavailable {
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
    /// Exact bounded route transition observed while opening this session.
    pub route_transition: crate::provider_connections::SessionRouteTransitionView,
}

/// Exact reasoning-effort capabilities for one selectable provider model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationModelOptionView {
    /// Exact connection/model identity accepted for a new session or the next run boundary.
    pub model_ref: ModelRef,
    /// Provider-owned display label.
    pub display_name: String,
    /// Whether the catalog proved the ID or presents it as a conservative reference.
    pub availability: crate::provider_connections::ModelAvailability,
    /// Provider-owned recommendation classification.
    pub recommendation: crate::provider_connections::ModelRecommendation,
    /// Catalog source for this exact connection.
    pub provenance: crate::provider_connections::ModelCatalogProvenance,
    /// Compatibility model-id projection for reasoning-effort lookup.
    pub model_name: String,
    /// Reasoning-effort values implemented for this model.
    pub available_reasoning_efforts: Vec<ReasoningEffort>,
    /// Configured default when it belongs to this model's exact support set.
    pub default_reasoning_effort: Option<ReasoningEffort>,
    /// Opaque provider/model binding required with an explicit effort selection.
    pub reasoning_effort_binding: Option<String>,
}

/// Returns exact Direct Tasks whose durable root attempt is complete and whose admitted
/// background children all have a persisted terminal result that a model continuation may read.
///
/// This projection is shared by interactive surfaces so a process-local completion signal alone
/// can never restart a Task after its child owner was lost.
#[must_use]
pub fn ready_direct_task_background_continuations(
    session: &sigil_kernel::Session,
) -> Vec<sigil_kernel::TaskId> {
    let tasks = session.task_state_projection();
    let agent_threads = session.agent_thread_state_projection();
    tasks
        .tasks
        .iter()
        .filter_map(|(task_id, task)| {
            let latest_attempt = task
                .direct_execution_attempts
                .values()
                .max_by_key(|attempt| attempt.ordinal);
            (task.status == sigil_kernel::TaskRunStatus::Running
                && task.latest_plan_version.is_none()
                && task.direct_execution_admission.is_some()
                && latest_attempt.is_some_and(|attempt| {
                    attempt.status == sigil_kernel::TaskExecutionAttemptStatus::Completed
                })
                && direct_task_background_results_are_ready(&tasks, &agent_threads, task_id))
            .then_some(task_id.clone())
        })
        .collect()
}

fn direct_task_background_results_are_ready(
    tasks: &sigil_kernel::TaskStateProjection,
    agent_threads: &sigil_kernel::AgentThreadStateProjection,
    task_id: &sigil_kernel::TaskId,
) -> bool {
    let thread_ids = tasks.direct_task_background_agents(task_id);
    !thread_ids.is_empty()
        && thread_ids.iter().all(|thread_id| {
            agent_threads
                .threads
                .get(thread_id)
                .is_some_and(|thread| match thread.status {
                    sigil_kernel::AgentThreadStatus::Completed => thread.result.is_some(),
                    // A collected failure has a durable status and reason for the model to inspect.
                    // An ownerless interruption or unresolved child state is not safe to resume.
                    sigil_kernel::AgentThreadStatus::Failed => true,
                    _ => false,
                })
        })
}

/// Provider-neutral facts needed to configure and explain the next application run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRunContextView {
    /// Compound connection/model identity selected for the next run in this session.
    pub model_ref: ModelRef,
    /// Provider identity selected for the next run in this session.
    pub provider_name: String,
    /// Model identity selected for the next run in this session.
    pub model_name: String,
    /// Exact connection-scoped catalog and effort projection for same-session selection.
    pub model_options: Vec<ApplicationModelOptionView>,
    /// Opaque binding proving the exact current model and connection-scoped catalog.
    pub model_selection_binding: String,
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
    /// Provider-neutral cumulative cache telemetry plus the latest local-layout diagnostic.
    pub cache_usage: Option<ApplicationCacheUsageView>,
    /// Source used to resolve the effective context window.
    pub context_window_source: crate::ContextWindowSource,
    /// Bounded command, skill, and agent metadata for application clients.
    pub extension_catalog: crate::ApplicationExtensionCatalogView,
    /// Exact-bound route recovery state; transcript and catalog reads remain available.
    pub route_recovery: Option<ApplicationSessionRouteRecoveryView>,
}

/// Provider-neutral cache telemetry shared by Desktop and other application adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationCacheUsageView {
    /// Cumulative provider-reported cache-read (hit) tokens.
    pub cache_read_tokens: u64,
    /// Cumulative provider-reported uncached input tokens.
    pub cache_miss_tokens: u64,
    /// Cumulative provider-reported cache-write tokens, when the route reports writes.
    pub cache_write_tokens: Option<u64>,
    /// Latest locally observed request-layout mutation.
    pub last_layout_mutation: Option<sigil_kernel::CacheLayoutMutationKind>,
    /// Latest request missed provider cache despite no locally observed prefix mutation.
    pub provider_miss_without_local_mutation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSessionRouteRecoveryCode {
    SessionRouteConfirmationRequired,
    SessionRouteSelectionRequired,
    ModelRouteNotConfigured,
    ConnectionConfigInvalid,
    ProviderUnavailable,
    AuthorityUnavailable,
    SessionAlreadyActive,
    SessionWriterBusy,
    SessionStreamInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSessionRouteRecoveryAction {
    ConfirmCurrentRoute,
    RepairConnection,
    SelectReplacement,
    StartNewSession,
    RetryProvider,
    RetrySessionAttach,
    BackToSessionLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSessionRouteRecoveryView {
    pub code: ApplicationSessionRouteRecoveryCode,
    pub allowed_actions: Vec<ApplicationSessionRouteRecoveryAction>,
    pub recovery_binding: String,
    pub retryable: bool,
}

/// Input required to prepare one application run.
#[derive(Debug, Clone)]
pub struct ApplicationRunRequest {
    /// Explicit host-local MCP declarations for this run; never saved to user configuration.
    pub additional_mcp_servers: Vec<crate::ApplicationMcpServerDeclaration>,
    /// User-selected recorded source comments; no authority to mutate the referenced files.
    pub review_annotations: Vec<sigil_application::ReviewAnnotation>,
    /// Resolved Sigil config path.
    pub config_path: PathBuf,
    /// Process launch working directory.
    pub launch_cwd: PathBuf,
    /// User prompt.
    pub prompt: String,
    /// Host-admitted references to the workspace image cache; bytes are never persisted inline.
    pub image_attachments: Vec<sigil_kernel::ImageAttachment>,
    /// Adapter-owned run identifier.
    pub run_id: String,
    /// Optional existing or preallocated durable V2 session path.
    pub session_path: Option<PathBuf>,
    /// Adapter-owned interactive attachment already acquired for this exact session.
    ///
    /// This supports one controller owning queue/control mutations and foreground runs without
    /// reacquiring the cross-process lock. The runtime still enforces its foreground run lease.
    pub session_attachment:
        Option<Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>>,
    /// Whether the adapter can provide explicit approvals after run start.
    pub interaction: ApplicationRunInteraction,
    /// Optional user-selected permission mode for this run.
    pub permission_mode: Option<PermissionMode>,
    /// Optional model selected for this run and subsequent runs in the same durable session.
    pub model_name: Option<String>,
    /// Optional exact connection selected by a headless caller; must accompany `model_name`.
    pub model_connection_id: Option<ConnectionId>,
    /// Opaque binding returned with the run-context model selection capability.
    pub model_selection_binding: Option<String>,
    /// Exact opaque route-recovery binding explicitly confirmed by the caller.
    pub route_recovery_binding: Option<String>,
    /// Optional exact effort selected for this run.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Opaque binding returned with the run-context effort capability.
    pub reasoning_effort_binding: Option<String>,
    /// Exact catalog binding for one user-invoked inline skill.
    pub skill_binding: Option<crate::ApplicationSkillBinding>,
    /// Exact catalog binding for one user-invoked supervised agent profile.
    pub agent_binding: Option<crate::ApplicationAgentBinding>,
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
            additional_mcp_servers: Vec::new(),
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            prompt: prompt.into(),
            image_attachments: Vec::new(),
            review_annotations: Vec::new(),
            run_id: run_id.into(),
            session_path: None,
            session_attachment: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
            model_name: None,
            model_connection_id: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
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

    #[cfg(test)]
    fn acquire(
        &self,
        path: &Path,
    ) -> std::result::Result<ApplicationSessionLease, ApplicationSessionLeaseError> {
        self.acquire_with_attachment(path, None)
    }

    fn acquire_with_attachment(
        &self,
        path: &Path,
        supplied_attachment: Option<
            Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        >,
    ) -> std::result::Result<ApplicationSessionLease, ApplicationSessionLeaseError> {
        let canonical = canonical_session_lease_path(path)
            .map_err(ApplicationSessionLeaseError::Unavailable)?;
        let mut active = self.active_paths.lock().map_err(|_| {
            ApplicationSessionLeaseError::Unavailable(anyhow!(
                "application session lease state is unavailable"
            ))
        })?;
        if !active.insert(canonical.clone()) {
            return Err(ApplicationSessionLeaseError::ProcessLocalActive {
                recovery_binding:
                    crate::interactive_session_attachment::session_attachment_path_recovery_binding(
                        &canonical,
                        "process-local-active",
                    ),
            });
        }
        let attachment = if let Some(attachment) = supplied_attachment {
            let attachment_path = canonical_session_lease_path(attachment.session_path())
                .map_err(ApplicationSessionLeaseError::Unavailable)?;
            if attachment_path != canonical {
                active.remove(&canonical);
                return Err(ApplicationSessionLeaseError::Unavailable(anyhow!(
                    "supplied session attachment belongs to another durable session: attachment={}, canonical={}",
                    attachment_path.display(),
                    canonical.display()
                )));
            }
            attachment
        } else {
            match crate::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                &canonical,
            ) {
                Ok(attachment) => Arc::new(attachment),
                Err(error) => {
                    active.remove(&canonical);
                    return match error {
                        crate::interactive_session_attachment::InteractiveSessionAttachmentError::Busy { observed_generation } => {
                            Err(ApplicationSessionLeaseError::AlreadyActive {
                                recovery_binding: crate::interactive_session_attachment::session_attachment_path_recovery_binding(
                                    &canonical,
                                    &observed_generation,
                                ),
                            })
                        }
                        error => Err(ApplicationSessionLeaseError::Unavailable(anyhow!(error))),
                    };
                }
            }
        };
        Ok(ApplicationSessionLease {
            path: canonical,
            active_paths: Arc::clone(&self.active_paths),
            attachment,
            route_execution_owner: Mutex::new(None),
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum ApplicationSessionLeaseError {
    #[error("application session already has an active foreground run")]
    ProcessLocalActive { recovery_binding: String },
    #[error("session_already_active")]
    AlreadyActive { recovery_binding: String },
    #[error("application session lease is unavailable")]
    Unavailable(#[source] anyhow::Error),
}

impl ApplicationSessionLeaseError {
    const fn is_already_active(&self) -> bool {
        matches!(
            self,
            Self::ProcessLocalActive { .. } | Self::AlreadyActive { .. }
        )
    }

    fn recovery_binding(&self) -> Option<&str> {
        match self {
            Self::ProcessLocalActive { recovery_binding }
            | Self::AlreadyActive { recovery_binding } => Some(recovery_binding),
            Self::Unavailable(_) => None,
        }
    }
}

#[derive(Debug)]
struct ApplicationSessionLease {
    path: PathBuf,
    active_paths: Arc<Mutex<BTreeSet<PathBuf>>>,
    attachment: Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    route_execution_owner: Mutex<Option<crate::provider_connections::SessionRouteExecutionOwner>>,
}

impl ApplicationSessionLease {
    fn route_mutation_authority(
        &self,
        session_scope_id: &str,
    ) -> Result<crate::provider_connections::SessionRouteMutationAuthority> {
        self.attachment.route_mutation_authority(session_scope_id)
    }

    fn acquire_route_execution_owner(&self, session_scope_id: &str) -> Result<()> {
        let authority = self.route_mutation_authority(session_scope_id)?;
        let mut owner = self
            .route_execution_owner
            .lock()
            .map_err(|_| anyhow!("session route execution owner state is unavailable"))?;
        if owner.is_none() {
            *owner = Some(authority.acquire_execution_owner()?);
        }
        Ok(())
    }
}

impl Drop for ApplicationSessionLease {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_paths.lock() {
            active.remove(&self.path);
        }
    }
}

/// Non-owning session tool views. The mutex only orders installation and exact trust changes.
pub type ApplicationExtensionRegistryViews = Arc<Mutex<Vec<sigil_kernel::WeakToolRegistry>>>;

/// Shared dependencies used while preparing application runs.
#[derive(Clone)]
pub struct ApplicationRunServices {
    extension_registry_views: Arc<Mutex<BTreeMap<String, ApplicationExtensionRegistryViews>>>,
    disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
    session_leases: Arc<ApplicationSessionLeaseManager>,
    supervisor_instance_id: Arc<str>,
    task_role_provider_builder:
        Option<Arc<dyn crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder>>,
    terminal_lifecycle_handler: Option<Arc<dyn crate::ApplicationTerminalLifecycleHandler>>,
    /// RFC-0062 14.1: process-scoped scratch lease registry shared by every run surface so
    /// tool/terminal leases, session-delete cleanup and TTL GC observe the same authority.
    scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
    /// RFC-0071 R71.6: the one application-global cutover decision. The boot owner selects the
    /// epoch exactly once; an unattached decision is unavailable and cannot start a run.
    cutover: Option<Arc<crate::r71_global_cutover::RuntimeGlobalCutoverV1>>,
    /// RFC-0071 R71.6: the boot authority composition (services + writer adapter + broker)
    /// shared by this surface's run paths; None before boot attach.
    authority_composition:
        Option<Arc<crate::r71_authority_composition::RuntimeAuthorityCompositionV1>>,
}

/// Process-local typed control for persistent terminal tasks admitted by one prepared run.
#[derive(Clone, Debug)]
pub struct ApplicationTerminalTaskControl {
    workspace_root: PathBuf,
    owner: sigil_tools_builtin::TerminalTaskControlHandle,
    _session_attachment:
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    _route_execution_owner: Arc<crate::provider_connections::SessionRouteExecutionOwner>,
}

impl ApplicationTerminalTaskControl {
    fn new(
        workspace_root: PathBuf,
        owner: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
        session_lease: &ApplicationSessionLease,
        session_scope_id: &str,
    ) -> Result<Option<Self>> {
        let Some(owner) = owner else {
            return Ok(None);
        };
        let route_execution_owner = session_lease
            .route_mutation_authority(session_scope_id)?
            .acquire_execution_owner()
            .map_err(anyhow::Error::new)?;
        Ok(Some(Self {
            workspace_root,
            owner,
            _session_attachment: Arc::clone(&session_lease.attachment),
            _route_execution_owner: Arc::new(route_execution_owner),
        }))
    }

    /// Cancels one exact terminal task through its original process owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the task identity is invalid, not owned by this run surface, or its
    /// process-tree cleanup cannot be confirmed.
    pub async fn cancel(&self, task_id: &str) -> Result<sigil_kernel::TerminalTaskEntry> {
        let task_id = sigil_kernel::TerminalTaskId::new(task_id)?;
        self.owner.cancel(&self.workspace_root, &task_id).await
    }

    /// Reads the latest exact owner state for one terminal task.
    ///
    /// # Errors
    ///
    /// Returns an error when the task identity is invalid or not owned by this run surface.
    pub async fn status(&self, task_id: &str) -> Result<sigil_kernel::TerminalTaskEntry> {
        let task_id = sigil_kernel::TerminalTaskId::new(task_id)?;
        self.owner.status(&self.workspace_root, &task_id).await
    }
}

impl std::fmt::Debug for ApplicationRunServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApplicationRunServices")
            .field("disclosure_presenter", &"configured")
            .field("session_leases", &self.session_leases)
            .field("supervisor_instance_id", &self.supervisor_instance_id)
            .field(
                "task_role_provider_builder",
                &self.task_role_provider_builder.is_some(),
            )
            .field(
                "terminal_lifecycle_handler",
                &self.terminal_lifecycle_handler.is_some(),
            )
            .finish()
    }
}

impl ApplicationRunServices {
    /// Creates shared run services with a process-local foreground session lease manager.
    #[must_use]
    pub fn new(disclosure_presenter: Arc<dyn EgressDisclosurePresenter>) -> Self {
        Self {
            extension_registry_views: Arc::new(Mutex::new(BTreeMap::new())),
            disclosure_presenter,
            session_leases: Arc::new(ApplicationSessionLeaseManager::new()),
            supervisor_instance_id: Arc::from(format!("runtime-{}", uuid::Uuid::new_v4())),
            task_role_provider_builder: None,
            terminal_lifecycle_handler: None,
            scratch_control: None,
            cutover: None,
            authority_composition: None,
        }
    }

    /// Creates shared run services with an injected session lease manager.
    #[must_use]
    pub fn with_session_leases(
        disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
        session_leases: Arc<ApplicationSessionLeaseManager>,
    ) -> Self {
        Self {
            extension_registry_views: Arc::new(Mutex::new(BTreeMap::new())),
            disclosure_presenter,
            session_leases,
            supervisor_instance_id: Arc::from(format!("runtime-{}", uuid::Uuid::new_v4())),
            task_role_provider_builder: None,
            terminal_lifecycle_handler: None,
            scratch_control: None,
            cutover: None,
            authority_composition: None,
        }
    }

    /// Shares non-owning registry views for one exact session across preparation and execution.
    /// The short lock orders registry installation with a plugin trust decision. It must not be
    /// held across network work or process settlement; registries retain their original owners.
    pub fn extension_registry_views(
        &self,
        session_scope_id: &str,
    ) -> Result<ApplicationExtensionRegistryViews> {
        anyhow::ensure!(
            !session_scope_id.is_empty(),
            "extension registry requires an exact session scope"
        );
        let mut sessions = self
            .extension_registry_views
            .lock()
            .map_err(|_| anyhow!("extension registry views unavailable"))?;
        Ok(Arc::clone(
            sessions.entry(session_scope_id.to_owned()).or_default(),
        ))
    }

    /// Replaces task role provider construction for an embedded adapter or deterministic test.
    #[must_use]
    pub fn with_task_role_provider_builder(
        mut self,
        builder: Arc<dyn crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder>,
    ) -> Self {
        self.task_role_provider_builder = Some(builder);
        self
    }

    /// Installs the adapter-owned bounded terminal lifecycle projection.
    #[must_use]
    pub fn with_terminal_lifecycle_handler(
        mut self,
        handler: Arc<dyn crate::ApplicationTerminalLifecycleHandler>,
    ) -> Self {
        self.terminal_lifecycle_handler = Some(handler);
        self
    }

    /// Shares the process-scoped scratch lease registry with every run tool surface.
    #[must_use]
    pub fn with_scratch_control(
        mut self,
        scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
    ) -> Self {
        self.scratch_control = scratch_control;
        self
    }

    /// Returns the process-scoped scratch lease registry, when the adapter shared one.
    #[must_use]
    pub fn scratch_control(&self) -> Option<&sigil_tools_builtin::ScratchNamespaceControl> {
        self.scratch_control.as_ref()
    }

    /// Reports whether this adapter can execute an accepted durable task handoff.
    #[must_use]
    pub fn task_executor_attached(&self) -> bool {
        self.task_role_provider_builder.is_some()
    }

    /// Attaches the one application-global cutover decision. The boot owner calls this exactly
    /// once per process; a second attachment replaces the previous decision but the manifest
    /// registry rejects a different manifest for the same instance (fixed-forward).
    #[must_use]
    pub fn with_global_cutover(
        mut self,
        cutover: crate::r71_global_cutover::RuntimeGlobalCutoverV1,
    ) -> Self {
        self.cutover = Some(Arc::new(cutover));
        self
    }

    /// Returns the attached cutover decision, when the boot owner selected an epoch.
    #[must_use]
    pub fn cutover(&self) -> Option<&crate::r71_global_cutover::RuntimeGlobalCutoverV1> {
        self.cutover.as_deref()
    }

    /// Attaches the boot authority composition (composed exactly once per process).
    #[must_use]
    pub fn with_authority_composition(
        mut self,
        composition: crate::r71_authority_composition::RuntimeAuthorityCompositionV1,
    ) -> Self {
        self.authority_composition = Some(Arc::new(composition));
        self
    }

    /// Returns the attached authority composition (None before boot attach).
    pub fn authority_composition(
        &self,
    ) -> Option<&crate::r71_authority_composition::RuntimeAuthorityCompositionV1> {
        self.authority_composition.as_deref()
    }

    /// Requires the complete current-schema authority surface before a run can be prepared.
    /// Keeping this check beside the service container prevents a caller from attaching only a
    /// cutover manifest while leaving the actual writer/tool authority absent.
    pub fn require_current_schema_authority(
        &self,
    ) -> Result<(), sigil_kernel::cutover_manifest::CutoverErrorV1> {
        let cutover = self
            .cutover
            .as_deref()
            .ok_or(sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable)?;
        if cutover.manifest().selected_epoch
            != sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
        {
            return Err(sigil_kernel::cutover_manifest::CutoverErrorV1::LegacySessionUnavailable);
        }
        if self.authority_composition.is_none() {
            return Err(sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable);
        }
        cutover.gate().map_err(Clone::clone)
    }

    /// Mandatory readiness check: the boot owner calls this after selecting the epoch. A
    /// NewCurrentSchema manifest with any failing adapter probe returns Err and the application
    /// must not start partially. No attached decision is also an error: there is no legacy
    /// runtime fallback.
    pub fn require_cutover_or_fail(
        &self,
    ) -> Result<(), sigil_kernel::cutover_manifest::CutoverErrorV1> {
        match self.cutover.as_deref() {
            None => Err(sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable),
            Some(decision) => decision.gate().map_err(|error| error.clone()),
        }
    }

    /// Old-schema session guard: after the publish, only current-schema sessions may be opened
    /// by this binary. Surfaces call this before opening any session store.
    pub fn admit_session_open(
        &self,
        session_epoch: sigil_kernel::cutover_manifest::StartupEpochV1,
    ) -> Result<(), sigil_kernel::cutover_manifest::CutoverErrorV1> {
        let binary_epoch = self
            .cutover
            .as_ref()
            .map(|decision| decision.manifest().selected_epoch)
            .ok_or(sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable)?;
        sigil_kernel::cutover_manifest::admit_session_open(
            sigil_kernel::cutover_manifest::SessionOpenAttemptV1 {
                session_epoch,
                binary_epoch,
            },
        )
    }
}

fn application_terminal_lifecycle_sink(
    recorder: MutationEventRecorder,
    handler: Option<Arc<dyn crate::ApplicationTerminalLifecycleHandler>>,
    events: ApplicationRunEventSequence,
) -> Arc<dyn sigil_kernel::TerminalLifecycleSink> {
    let router = crate::ApplicationTerminalLifecycleRouter::new(recorder);
    let router = if let Some(handler) = handler {
        router.with_application_public_events(handler, events)
    } else {
        router
    };
    Arc::new(router)
}

/// Sink for ordered provider-neutral application events.
pub trait ApplicationRunEventHandler {
    /// Handles one public event.
    ///
    /// # Errors
    ///
    /// Returns an error when this adapter cannot currently accept the event. Runtime retains the
    /// already durable outbox entry for ordered replay; this transport result does not decide the
    /// application run's domain terminal.
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()>;

    /// Attaches a read-only preview source. Interactive adapters poll this bounded source at
    /// frame cadence; provider deltas never become queued public events.
    fn bind_live_preview_source(&mut self, _source: RuntimeLivePreviewSource) -> Result<()> {
        Ok(())
    }

    /// Accepts an independently typed preview snapshot. Machine/durable-only adapters may
    /// ignore it, but must never convert its revision into a public event sequence.
    fn handle_live_update(&mut self, _update: sigil_application::LiveRunUpdate) -> Result<()> {
        Ok(())
    }

    /// Bounded durable adapter identity used by the public outbox receipt.  Implementations that
    /// share an application bridge can keep the generic value; dedicated HTTP/Desktop/TUI
    /// adapters may override it without changing domain terminal semantics.
    fn public_event_adapter_id(&self) -> &'static str {
        "application"
    }

    /// Commits an ordinary Plan review terminal bundle before delivering any of its public
    /// events. Implementations owning the application session may atomically persist the plan
    /// controls, assistant final answer, conversation lifecycle terminal, and terminal outbox;
    /// compatibility handlers return `false` so the caller can retain the older split path.
    fn commit_plan_review_terminal(
        &mut self,
        _session: &mut Session,
        _entries: Vec<SessionLogEntry>,
        _publications: Vec<SessionPublicEventProjectionV1>,
        _terminal: ConversationRunFinalizedEntryV1,
        _terminal_event: PublicRunEventKind,
    ) -> Result<bool> {
        Ok(false)
    }
}

/// Prepared application run and its root cancellation authority.
pub struct PreparedApplicationRun {
    execution: ApplicationRunExecution,
    control: ApplicationRunControl,
    terminal_control: Option<ApplicationTerminalTaskControl>,
}

impl PreparedApplicationRun {
    /// Attaches an already-prepared application's exact ordinary-input operation to this owner.
    /// The later durable run start, not preparation or a channel acknowledgement, accepts it.
    ///
    /// # Errors
    /// Rejects a different input, scope, or operation preparation.
    pub fn bind_conversation_run_operation(
        &mut self,
        binding: sigil_kernel::ApplicationOperationBindingV1,
        original_input_digest: &str,
    ) -> Result<()> {
        if binding.target
            != (sigil_kernel::ApplicationOperationTargetV1::ConversationRunAdmission {
                input_digest: original_input_digest.to_owned(),
            })
        {
            bail!("conversation run operation does not match its original input");
        }
        self.execution.session.bind_application_operation(binding)
    }

    /// Returns projection and delivery capabilities from this prepared run's actual session owner.
    #[must_use]
    pub fn session_projection_owner(&self) -> crate::RuntimeSessionProjectionOwner {
        self.execution.events.projection_owner.clone()
    }

    /// Returns the typed persistent-terminal owner retained beyond the foreground model turn.
    #[must_use]
    pub fn terminal_control(&self) -> Option<ApplicationTerminalTaskControl> {
        self.terminal_control.clone()
    }

    /// Returns the authority-admitted artifact facade already owned by this foreground run.
    ///
    /// Adapters may use this clone for read-only projections while the run is still active. It
    /// shares the runtime backend and does not attempt to acquire a second physical namespace
    /// lease.
    #[must_use]
    pub fn tool_artifact_store(&self) -> Option<ToolArtifactStore> {
        self.execution
            .managed_artifact_store
            .as_ref()
            .map(ManagedApplicationArtifactStoreLease::store)
    }

    /// Separates the execution payload from its root cancellation authority.
    ///
    /// The caller must keep `control` alive until the execution reaches a terminal state.
    #[must_use]
    pub fn into_parts(self) -> (ApplicationRunExecution, ApplicationRunControl) {
        (self.execution, self.control)
    }

    /// Returns the prepared agent run options (RFC-0071 R71.6: includes the composed kernel
    /// tool authority when the boot surface attached a composition).
    #[must_use]
    pub fn run_options(&self) -> &AgentRunOptions {
        &self.execution.options
    }

    /// Commits one validated queued promotion behind the application ownership boundary, then
    /// replaces the ordinary run input.
    ///
    /// URL capability material is staged before the writer-lock promotion CAS. That promotion is
    /// the unique durable user event and embeds the safe message plus capability descriptors. The
    /// already-durable promotion is then adopted by the live session projection before the
    /// capabilities are committed. Only a successful commit installs the no-persistence frozen
    /// queued input; any failure consumes this prepared run so it cannot dispatch without its
    /// durable promotion evidence.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the prepared run does not match the queued session, logical run,
    /// provider/model, or safe prompt, or when any durable promotion stage fails.
    pub(crate) fn commit_queued_promotion(
        mut self,
        queued: PreparedApplicationQueuedRunInput,
    ) -> Result<Self, ApplicationQueuedRunPrepareError> {
        if self.execution.session_id != queued.session_scope_id {
            return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
                "prepared run belongs to a different session scope",
            ));
        }
        if self.execution.run_id != queued.promotion.dispatch_run_id {
            return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
                "prepared run id does not match the queued dispatch run id",
            ));
        }
        if self.execution.prompt != queued.safe_prompt {
            return Err(ApplicationQueuedRunPrepareError::prompt_material_mismatch(
                "prepared run prompt is not the durable safe prompt",
            ));
        }
        if self.execution.session.provider_name() != queued.provider_name
            || self.execution.session.model_name() != queued.model_name
        {
            return Err(ApplicationQueuedRunPrepareError::frozen_request_mismatch(
                "provider or model does not match the prepared session",
            ));
        }
        let ApplicationRunExecutionKind::Main { input, .. } = &mut self.execution.kind else {
            return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
                "queued main-thread input cannot invoke an agent profile",
            ));
        };
        if self.execution.session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::User(message)
                    if message.id == queued.promotion.durable_user_message.id
            )
        }) {
            return Err(ApplicationQueuedRunPrepareError::QueueConflict {
                source: anyhow!("queued durable user message id is already present"),
            });
        }

        let durable_message_id = queued.promotion.durable_user_message.id.clone();
        let registrar = self.execution.session.user_url_capability_registrar();
        if !queued.capability_registrations.is_empty() && registrar.is_none() {
            return Err(ApplicationQueuedRunPrepareError::promotion_commit(
                "capability_stage",
                anyhow!("queued URL capability registrar is unavailable"),
            ));
        }
        if let Some(registrar) = registrar.as_ref() {
            for registration in &queued.capability_registrations {
                if let Err(source) = registrar.stage(registration.clone()) {
                    let _ = registrar.rollback_message(&durable_message_id);
                    return Err(ApplicationQueuedRunPrepareError::promotion_commit(
                        "capability_stage",
                        source,
                    ));
                }
            }
        }

        let promotion_store =
            JsonlSessionStore::new(&self.execution.session_log_path).map_err(|source| {
                rollback_queued_capabilities(registrar.as_deref(), &durable_message_id);
                ApplicationQueuedRunPrepareError::promotion_commit("promotion_store", source)
            })?;
        if let Err(source) =
            promotion_store.append_conversation_input_promoted(queued.promotion.clone())
        {
            rollback_queued_capabilities(registrar.as_deref(), &durable_message_id);
            return Err(ApplicationQueuedRunPrepareError::promotion_commit(
                "promotion_cas",
                source,
            ));
        }
        if let Err(source) = self
            .execution
            .session
            .record_durably_appended_conversation_input_promotion(queued.promotion.clone())
        {
            rollback_queued_capabilities(registrar.as_deref(), &durable_message_id);
            return Err(ApplicationQueuedRunPrepareError::promotion_commit(
                "promotion_projection",
                source,
            ));
        }
        if let Some(registrar) = registrar.as_ref()
            && let Err(source) = registrar.commit_message(&durable_message_id)
        {
            rollback_queued_capabilities(Some(registrar.as_ref()), &durable_message_id);
            return Err(ApplicationQueuedRunPrepareError::promotion_commit(
                "capability_commit",
                source,
            ));
        }

        let queued_input =
            if let Some(coordinator) = self.execution.conversation_coordinator.as_ref() {
                coordinator
                    .enforce_orchestration_route_kill_switch(
                        &mut self.execution.session,
                        current_unix_time_ms(),
                    )
                    .map_err(|source| {
                        ApplicationQueuedRunPrepareError::promotion_commit(
                            "orchestration_route_guard",
                            source,
                        )
                    })?;
                coordinator
                    .bind_conversation_input(
                        &self.execution.session,
                        queued.input,
                        self.execution.parent_session_ref.clone(),
                        self.execution.run_id.clone(),
                        Some(crate::ConversationSourceTurn {
                            message_id: durable_message_id,
                            objective: queued.safe_prompt,
                        }),
                        current_unix_time_ms(),
                    )
                    .map_err(|source| {
                        ApplicationQueuedRunPrepareError::promotion_commit(
                            "task_handoff_binding",
                            source,
                        )
                    })?
            } else {
                queued.input
            };
        **input = queued_input;
        Ok(self)
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

    #[cfg(test)]
    pub(crate) fn has_in_memory_queued_promotion(
        &self,
        queue_id: &sigil_kernel::ConversationInputQueueId,
    ) -> bool {
        self.execution.session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion))
                    if &promotion.queue_id == queue_id
            )
        })
    }
}

fn rollback_queued_capabilities(
    registrar: Option<&dyn sigil_kernel::UserUrlCapabilityRegistrar>,
    durable_message_id: &str,
) {
    if let Some(registrar) = registrar {
        let _ = registrar.rollback_message(durable_message_id);
    }
}

/// Root cancellation authority retained by the adapter while an application run is active.
pub struct ApplicationRunControl {
    owner: RunCancellationOwner,
    recorder: RunCancellationRecorder,
    cancellation_target: RunCancellationTarget,
    conversation_lifecycle: ConversationRunLifecycleRecorder,
    conversation_start: ConversationRunStartedEntryV1,
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

    /// Returns the terminal classification only after its domain record and exact public outbox
    /// event committed in the same durable bundle.
    ///
    /// A failed adapter delivery intentionally does not clear this fact. Callers must replay the
    /// pending outbox event instead of manufacturing a different terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns an error when writer recovery or the durable lifecycle/outbox projection fails.
    pub fn durable_terminal_status(&self) -> Result<Option<ApplicationRunTerminalStatus>> {
        Ok(self
            .conversation_lifecycle
            .finalized_for_run(self.conversation_start.run_id())?
            .map(|terminal| terminal.status()))
    }

    /// Returns whether a public adapter or its durable outbox acknowledgement degraded while the
    /// domain run continued. Callers must retry/replay delivery; they must not replace the domain
    /// terminal with a generic failure.
    pub fn public_delivery_is_degraded(&self) -> Result<bool> {
        self.events.delivery_is_degraded()
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
        self.request_stop(
            format!("cancel-{}", self.owner.handle().scope_id()),
            reason,
            timeout,
            unblock_approval,
        )
    }

    /// Validates and durably requests a pause for one exact accepted Task plan.
    ///
    /// Validation reads the active session while this control retains its foreground lease. A
    /// stale request does not reserve cancellation or stop the run.
    ///
    /// # Errors
    ///
    /// Returns an error without a ticket when the rendered Task binding is stale. When
    /// cancellation was activated but its durable request append failed, the error retains a
    /// ticket that the adapter must pass to [`Self::finalize_task_pause`].
    pub fn request_task_pause(
        &self,
        request: TaskPauseRequest,
        timeout: Option<Duration>,
        unblock_approval: impl FnOnce(),
    ) -> std::result::Result<ApplicationTaskPauseTicket, ApplicationTaskPauseRequestError> {
        let entries =
            application_bound_session_entries(&self._session_lease.path, &self.events.session_id)
                .map_err(ApplicationTaskPauseRequestError::without_ticket)?;
        crate::agent_supervisor::task_execution::validate_task_pause_request(
            &request,
            &self.cancellation_target,
            self.owner.handle().scope_id(),
            &entries,
        )
        .map_err(|error| {
            ApplicationTaskPauseRequestError::without_ticket(
                anyhow!(error).context("application Task pause binding is stale"),
            )
        })?;
        let cancellation_request_id =
            format!("{}-{}", request.request_id, self.owner.handle().scope_id());
        match self.request_stop(
            cancellation_request_id,
            "task pause requested",
            timeout,
            unblock_approval,
        ) {
            Ok(cancellation) => Ok(ApplicationTaskPauseTicket {
                request,
                cancellation,
            }),
            Err(error) => {
                let (source, ticket) = error.into_parts();
                Err(ApplicationTaskPauseRequestError {
                    source,
                    ticket: ticket.map(|cancellation| {
                        Box::new(ApplicationTaskPauseTicket {
                            request,
                            cancellation,
                        })
                    }),
                })
            }
        }
    }

    fn request_stop(
        &self,
        request_id: String,
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
        sigil_kernel::run_diagnostics::record_run_timing(
            self.conversation_start.run_id(),
            sigil_kernel::run_diagnostics::RunTimingPhase::CancellationRequested,
            Duration::ZERO,
        );
        let timeout = if requested_at.checked_add(requested_timeout).is_some() {
            requested_timeout
        } else {
            DEFAULT_CANCELLATION_QUIESCENCE_TIMEOUT
        };
        let deadline = requested_at + timeout;
        let requested_at_ms = current_unix_time_ms();
        let reason = reason.into();
        let request = RunCancellationRequestedEntry {
            request_id,
            run_scope_id: self.owner.handle().scope_id().to_owned(),
            target: self.cancellation_target.clone(),
            reason: safe_persistence_text(&reason),
            requested_at_ms,
            quiescence_deadline_ms: requested_at_ms
                .saturating_add(timeout.as_millis().try_into().unwrap_or(u64::MAX)),
        };
        let conversation_start = self
            .conversation_lifecycle
            .append_started(&self.conversation_start);
        let append = self.recorder.append_requested(&request);
        let activated = self.owner.activate_reserved_cancel();
        debug_assert!(
            activated,
            "reserved cancellation must activate exactly once"
        );
        unblock_approval();
        let ticket = ApplicationCancellationTicket {
            request,
            started: requested_at,
            deadline,
            request_recorded: append.is_ok(),
            conversation_start_recorded: conversation_start.is_ok(),
        };
        match (conversation_start, append) {
            (Ok(_), Ok(_)) => Ok(ticket),
            (Err(error), _) => Err(ApplicationCancellationRequestError::with_ticket(
                error.context("failed to persist application conversation run start"),
                ticket,
            )),
            (_, Err(error)) => Err(ApplicationCancellationRequestError::with_ticket(
                error.context("failed to persist application cancellation request"),
                ticket,
            )),
        }
    }

    async fn cancel_scoped_task_background_agents(&self, reason: &str) -> Result<()> {
        let background_runs = self
            ._session_lease
            .attachment
            .agent_tool_background_runs()?;
        let live_threads = background_runs.thread_ids()?;
        let store = JsonlSessionStore::new(&self._session_lease.path)?
            .with_live_background_agent_threads(live_threads);
        let mut session = Session::load_from_store_for_control(store)?;
        if session.session_scope_id() != self.events.session_id {
            bail!("application control session identity changed during Task cancellation");
        }
        let mut event_handler = NoopEventHandler;
        background_runs
            .cancel_task_background_agents_for_scope(
                &mut session,
                &self.cancellation_target,
                self.owner.handle().scope_id(),
                reason,
                &mut event_handler,
            )
            .await
            .map(|_| ())
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
        let conversation_start = if ticket.conversation_start_recorded {
            Ok(())
        } else {
            self.conversation_lifecycle
                .append_started(&self.conversation_start)
                .map(|_| ())
                .context("failed to recover application conversation run start")
        };
        if !ticket.request_recorded {
            if let Err(error) = self
                .cancel_scoped_task_background_agents(&ticket.request.reason)
                .await
            {
                self.owner.handle().mark_cleanup_incomplete();
                tracing::warn!(%error, "Task background cleanup could not be confirmed during unaudited cancellation");
            }
            let _ = self
                .owner
                .wait_for_quiescence(ticket.remaining_timeout())
                .await;
            let task_stop = self.append_related_task_stop_state(
                handler,
                crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
                "application Task cancellation request could not be durably audited",
            );
            let terminal_event = PublicRunEventKind::RunInterrupted {
                reason: "run interrupted because its cancellation request could not be audited"
                    .to_owned(),
            };
            let conversation_terminal = if conversation_start.is_ok() {
                emit_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.events,
                    handler,
                    self.conversation_start.run_id(),
                    ApplicationRunTerminalStatus::Interrupted,
                    None,
                    Some("cancellation request could not be durably audited"),
                    &sigil_kernel::SecretRedactor::empty(),
                    terminal_event,
                )
            } else {
                Ok(())
            };
            task_stop?;
            conversation_terminal?;
            conversation_start?;
            bail!("application cancellation request was not durably recorded");
        }
        if let Err(error) = self
            .cancel_scoped_task_background_agents(&ticket.request.reason)
            .await
        {
            self.owner.handle().mark_cleanup_incomplete();
            tracing::warn!(%error, "Task background cleanup could not be confirmed during cancellation");
        }
        let outcome = self
            .finalize_recorded_cancellation(ticket, execution_joined, conversation_start)
            .await?;
        self.append_related_task_stop_state(
            handler,
            match outcome {
                RunCancellationTerminalOutcome::Cancelled => {
                    crate::agent_supervisor::task_execution::TaskStopDisposition::Cancelled
                }
                RunCancellationTerminalOutcome::Interrupted => {
                    crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted
                }
            },
            match outcome {
                RunCancellationTerminalOutcome::Cancelled => {
                    "application Task cancellation quiescence confirmed"
                }
                RunCancellationTerminalOutcome::Interrupted => {
                    "application Task cancellation cleanup could not be confirmed"
                }
            },
        )?;
        let (terminal_status, conversation_summary, terminal) = match outcome {
            RunCancellationTerminalOutcome::Cancelled => (
                ApplicationRunTerminalStatus::Cancelled,
                "cancellation quiescence confirmed",
                PublicRunEventKind::RunCancelled,
            ),
            RunCancellationTerminalOutcome::Interrupted => (
                ApplicationRunTerminalStatus::Interrupted,
                "cancellation cleanup could not be confirmed",
                PublicRunEventKind::RunInterrupted {
                    reason: "run interrupted before cancellation cleanup could be confirmed"
                        .to_owned(),
                },
            ),
        };
        emit_application_conversation_terminal(
            &self.conversation_lifecycle,
            &self.events,
            handler,
            self.conversation_start.run_id(),
            terminal_status,
            None,
            Some(conversation_summary),
            &sigil_kernel::SecretRedactor::empty(),
            terminal,
        )?;
        Ok(outcome)
    }

    /// Finalizes one exact Task pause after the owned execution reaches its stop boundary.
    ///
    /// The pause binding is revalidated after quiescence. If its Task plan changed while
    /// cancellation propagated, the Task is recorded as interrupted rather than paused.
    ///
    /// # Errors
    ///
    /// Returns an error when the cancellation request or Task terminal transition cannot be
    /// durably recorded, or when the public terminal event cannot be delivered.
    pub async fn finalize_task_pause<H>(
        &self,
        ticket: ApplicationTaskPauseTicket,
        execution_joined: bool,
        handler: &mut H,
    ) -> Result<ApplicationTaskPauseOutcome>
    where
        H: ApplicationRunEventHandler,
    {
        let ApplicationTaskPauseTicket {
            request,
            cancellation,
        } = ticket;
        let conversation_start = if cancellation.conversation_start_recorded {
            Ok(())
        } else {
            self.conversation_lifecycle
                .append_started(&self.conversation_start)
                .map(|_| ())
                .context("failed to recover application conversation run start")
        };
        if !cancellation.request_recorded {
            if let Err(error) = self
                .cancel_scoped_task_background_agents("Task pause requested")
                .await
            {
                self.owner.handle().mark_cleanup_incomplete();
                tracing::warn!(%error, "Task background cleanup could not be confirmed during unaudited pause");
            }
            let _ = self
                .owner
                .wait_for_quiescence(cancellation.remaining_timeout())
                .await;
            let task_stop = self.load_control_session().and_then(|mut session| {
                self.append_task_stop_state_and_emit(
                    &mut session,
                    handler,
                    &request.task_id,
                    crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
                    "application Task pause request could not be durably audited",
                )
            });
            let terminal_event = PublicRunEventKind::RunInterrupted {
                reason: "Task interrupted because its pause request could not be audited"
                    .to_owned(),
            };
            let conversation_terminal = if conversation_start.is_ok() {
                emit_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.events,
                    handler,
                    self.conversation_start.run_id(),
                    ApplicationRunTerminalStatus::Interrupted,
                    None,
                    Some("Task pause request could not be durably audited"),
                    &sigil_kernel::SecretRedactor::empty(),
                    terminal_event,
                )
            } else {
                Ok(())
            };
            task_stop?;
            conversation_terminal?;
            conversation_start?;
            bail!("application Task pause request was not durably recorded");
        }
        if let Err(error) = self
            .cancel_scoped_task_background_agents("Task pause requested")
            .await
        {
            self.owner.handle().mark_cleanup_incomplete();
            tracing::warn!(%error, "Task background cleanup could not be confirmed during pause");
        }
        let cancellation_outcome = self
            .finalize_recorded_cancellation(cancellation, execution_joined, conversation_start)
            .await?;
        let mut session = self.load_control_session()?;
        let (disposition, reason) = match cancellation_outcome {
            RunCancellationTerminalOutcome::Cancelled => {
                match crate::agent_supervisor::task_execution::validate_task_pause_request(
                    &request,
                    &self.cancellation_target,
                    self.owner.handle().scope_id(),
                    session.entries(),
                ) {
                    Ok(()) => (
                        crate::agent_supervisor::task_execution::TaskStopDisposition::Paused,
                        "application Task paused after quiescence".to_owned(),
                    ),
                    Err(error) => (
                        crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
                        format!(
                            "Task pause binding became stale after cancellation: {}",
                            safe_persistence_text(&error.to_string())
                        ),
                    ),
                }
            }
            RunCancellationTerminalOutcome::Interrupted => (
                crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
                "application Task pause cleanup could not be confirmed".to_owned(),
            ),
        };
        let task_stop = self
            .append_task_stop_state_and_emit(
                &mut session,
                handler,
                &request.task_id,
                disposition,
                &reason,
            )?
            .context("exact application Task was not available during pause finalization")?;
        let task_status = task_stop.status();
        let (terminal_status, terminal_summary, terminal) = match task_status {
            TaskRunStatus::Paused => (
                ApplicationRunTerminalStatus::Paused,
                "Task paused",
                PublicRunEventKind::RunPaused {
                    reason: "Task is durably paused".to_owned(),
                },
            ),
            TaskRunStatus::Interrupted => (
                ApplicationRunTerminalStatus::Interrupted,
                "Task pause could not be confirmed",
                PublicRunEventKind::RunInterrupted {
                    reason: "Task interrupted before pause cleanup could be confirmed".to_owned(),
                },
            ),
            _ => bail!("application Task pause wrote an invalid terminal status"),
        };
        emit_application_conversation_terminal(
            &self.conversation_lifecycle,
            &self.events,
            handler,
            self.conversation_start.run_id(),
            terminal_status,
            None,
            Some(terminal_summary),
            &sigil_kernel::SecretRedactor::empty(),
            terminal,
        )?;
        Ok(ApplicationTaskPauseOutcome {
            task_id: request.task_id,
            task_status,
            cancellation_outcome,
        })
    }

    async fn finalize_recorded_cancellation(
        &self,
        ticket: ApplicationCancellationTicket,
        execution_joined: bool,
        conversation_start: Result<()>,
    ) -> Result<RunCancellationTerminalOutcome> {
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
        conversation_start?;
        sigil_kernel::run_diagnostics::record_run_timing(
            self.conversation_start.run_id(),
            sigil_kernel::run_diagnostics::RunTimingPhase::CancellationSettled,
            ticket.started.elapsed(),
        );
        Ok(outcome)
    }

    fn append_related_task_stop_state<H>(
        &self,
        handler: &mut H,
        disposition: crate::agent_supervisor::task_execution::TaskStopDisposition,
        reason: &str,
    ) -> Result<Option<crate::agent_supervisor::task_execution::AppendedTaskStopState>>
    where
        H: ApplicationRunEventHandler,
    {
        if matches!(
            self.cancellation_target,
            RunCancellationTarget::AgentThread { .. }
        ) {
            return Ok(None);
        }
        let mut session = self.load_control_session()?;
        let task_id = crate::agent_supervisor::task_execution::task_id_for_cancellation_scope(
            session.entries(),
            &self.cancellation_target,
            self.owner.handle().scope_id(),
        );
        let Some(task_id) = task_id else {
            return Ok(None);
        };
        self.append_task_stop_state_and_emit(&mut session, handler, &task_id, disposition, reason)
    }

    fn append_task_stop_state_and_emit<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        task_id: &TaskId,
        disposition: crate::agent_supervisor::task_execution::TaskStopDisposition,
        reason: &str,
    ) -> Result<Option<crate::agent_supervisor::task_execution::AppendedTaskStopState>>
    where
        H: ApplicationRunEventHandler,
    {
        let mut bridge = PublicApplicationEventBridge::new(self.events.clone(), handler)?;
        let task_stop = crate::agent_supervisor::task_execution::append_task_stop_state(
            session,
            &mut bridge,
            task_id,
            disposition,
            reason,
        )?;
        if let Some(task_stop) = task_stop.as_ref() {
            bridge.emit(PublicRunEventKind::TaskRunFinished {
                task_id: task_stop.task_id().as_str().to_owned(),
                status: task_stop.status().as_str().to_owned(),
            })?;
        }
        Ok(task_stop)
    }

    fn load_control_session(&self) -> Result<Session> {
        load_application_control_session(&self._session_lease.path, &self.events.session_id)
    }
}

/// Durable cancellation request retained until cleanup reaches a terminal observation.
#[derive(Debug)]
pub struct ApplicationCancellationTicket {
    request: RunCancellationRequestedEntry,
    started: Instant,
    deadline: Instant,
    request_recorded: bool,
    conversation_start_recorded: bool,
}

impl ApplicationCancellationTicket {
    /// Returns the time remaining before the cancellation request's bounded deadline.
    #[must_use]
    pub fn remaining_timeout(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

/// Exact Task pause request retained until cancellation cleanup reaches a terminal observation.
#[derive(Debug)]
pub struct ApplicationTaskPauseTicket {
    request: TaskPauseRequest,
    cancellation: ApplicationCancellationTicket,
}

impl ApplicationTaskPauseTicket {
    /// Returns the exact rendered Task pause binding.
    #[must_use]
    pub fn request(&self) -> &TaskPauseRequest {
        &self.request
    }

    /// Returns the time remaining before the pause request's bounded cleanup deadline.
    #[must_use]
    pub fn remaining_timeout(&self) -> Duration {
        self.cancellation.remaining_timeout()
    }
}

/// Durable result of one application-owned Task pause finalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTaskPauseOutcome {
    /// Exact Task selected by the rendered pause action.
    pub task_id: TaskId,
    /// Durable Task status after cleanup and final binding validation.
    pub task_status: TaskRunStatus,
    /// Physical run cancellation observation used to authorize the Task terminal state.
    pub cancellation_outcome: RunCancellationTerminalOutcome,
}

/// Pause activation failure that may still carry a ticket requiring cleanup finalization.
#[derive(Debug)]
pub struct ApplicationTaskPauseRequestError {
    source: anyhow::Error,
    ticket: Option<Box<ApplicationTaskPauseTicket>>,
}

impl ApplicationTaskPauseRequestError {
    fn without_ticket(source: anyhow::Error) -> Self {
        Self {
            source,
            ticket: None,
        }
    }

    /// Returns a ticket when cancellation was activated despite an audit append failure.
    #[must_use]
    pub fn into_ticket(self) -> Option<ApplicationTaskPauseTicket> {
        self.ticket.map(|ticket| *ticket)
    }
}

impl fmt::Display for ApplicationTaskPauseRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.source)
    }
}

impl std::error::Error for ApplicationTaskPauseRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
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

    fn into_parts(self) -> (anyhow::Error, Option<ApplicationCancellationTicket>) {
        (self.source, self.ticket.map(|ticket| *ticket))
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
    extension_registry: sigil_kernel::ToolRegistry,
    extension_background_runs: crate::AgentToolBackgroundRuns,
    kind: ApplicationRunExecutionKind,
    task_execution: Option<ApplicationTaskExecutionRuntime>,
    plan_review_runtime: Option<ApplicationPlanReviewRuntime>,
    session: Session,
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
    conversation_lifecycle: ConversationRunLifecycleRecorder,
    conversation_start: ConversationRunStartedEntryV1,
    events: ApplicationRunEventSequence,
    conversation_coordinator: Option<crate::ConversationCoordinator>,
    parent_session_ref: SessionRef,
    pending_session_title: Option<ApplicationSessionTitleRequest>,
    pending_user_input_continuation: Option<user_input::ApplicationUserInputContinuationContext>,
    route_transition: crate::provider_connections::SessionRouteTransitionView,
    managed_session_log: Option<ManagedApplicationSessionLogLease>,
    managed_artifact_store: Option<ManagedApplicationArtifactStoreLease>,
    _session_lease: Arc<ApplicationSessionLease>,
}

/// One authority-admitted session-log namespace held for the complete foreground run.
///
/// `JsonlSessionStore` remains a kernel-owned append-log implementation. Runtime first admits the
/// exact managed leaf and points that store at its authority-declared `records.jsonl`; this guard
/// makes preparation failures and terminal/error paths close the same current-schema namespace.
struct ManagedApplicationSessionLogLease {
    writer: Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    lease: Option<crate::managed_storage_writer::ManagedStorageWriterLeaseV1>,
}

impl ManagedApplicationSessionLogLease {
    fn acquire(
        writer: Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
        key: &str,
    ) -> Result<Self> {
        let lease = writer
            .acquire_session_log_key(key)
            .map_err(|error| anyhow!("managed session-log namespace admission failed: {error}"))?;
        Ok(Self {
            writer,
            lease: Some(lease),
        })
    }

    fn finalize(mut self) -> Result<()> {
        let Some(lease) = self.lease.take() else {
            return Ok(());
        };
        self.writer
            .finalize(lease)
            .map_err(|error| anyhow!("managed session-log namespace finalize failed: {error}"))?;
        Ok(())
    }
}

impl Drop for ManagedApplicationSessionLogLease {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        if let Err(error) = self.writer.finalize(lease) {
            tracing::error!(%error, "failed to finalize managed session-log namespace during cleanup");
        }
    }
}

/// Authority-admitted ArtifactStaging + ArtifactStore namespaces held for one foreground
/// operation. The kernel receives only the opaque backend facade; physical roots stay inside the
/// runtime artifact owner.
struct ManagedApplicationArtifactStoreLease {
    inner: crate::managed_artifact_store::ManagedArtifactStoreLeaseV1,
}

impl ManagedApplicationArtifactStoreLease {
    fn acquire(
        writer: Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
        key: &str,
        session_path: &Path,
        session_scope_id: &str,
    ) -> Result<Self> {
        Ok(Self {
            inner: crate::managed_artifact_store::ManagedArtifactStoreLeaseV1::acquire_with_session_path(
                writer,
                key,
                session_scope_id,
                session_path.to_path_buf(),
            )?,
        })
    }

    fn store(&self) -> ToolArtifactStore {
        self.inner.store()
    }

    fn finalize(self) -> Result<()> {
        self.inner.finalize()
    }
}

#[derive(Debug, Clone)]
struct ApplicationSessionTitleRequest {
    root_config: RootConfig,
    workspace_root: PathBuf,
    model_ref: ModelRef,
    session_log_path: PathBuf,
    session_id: String,
    prompt: String,
    /// RFC-0071 R71.6: composed storage writer backing the title journal (managed lifecycle
    /// namespaces); None keeps the legacy path-rooted journal.
    managed_writer:
        Option<std::sync::Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>>,
}

/// Non-critical maintenance produced by a completed foreground application run.
///
/// Adapters must release their foreground-run ownership before awaiting this work. A failed
/// maintenance action never changes the already durable terminal outcome of the run.
#[derive(Debug, Clone)]
pub struct ApplicationPostRunMaintenance {
    session_title: Option<ApplicationSessionTitleRequest>,
}

impl ApplicationPostRunMaintenance {
    fn from_session_title(request: Option<ApplicationSessionTitleRequest>) -> Option<Self> {
        request.map(|request| Self {
            session_title: Some(request),
        })
    }

    /// Builds the bounded semantic-title maintenance used by adapters that own their own
    /// foreground execution loop.
    #[must_use]
    pub fn session_title(
        root_config: RootConfig,
        workspace_root: PathBuf,
        model_ref: ModelRef,
        session_log_path: PathBuf,
        session_id: String,
        prompt: String,
    ) -> Self {
        Self {
            session_title: Some(ApplicationSessionTitleRequest {
                root_config,
                workspace_root,
                model_ref,
                session_log_path,
                session_id,
                prompt,
                managed_writer: None,
            }),
        }
    }

    /// Attaches the composed storage writer so the title journal writes through managed
    /// session-lifecycle namespaces (RFC-0071 R71.6).
    #[must_use]
    pub fn with_managed_writer(
        mut self,
        writer: std::sync::Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    ) -> Self {
        if let Some(request) = self.session_title.as_mut() {
            request.managed_writer = Some(writer);
        }
        self
    }

    /// Executes all bounded, non-critical maintenance associated with the completed run.
    ///
    /// # Errors
    ///
    /// Returns an error when title generation or its durable catalog update fails. The caller
    /// should report this diagnostically and must not rewrite the foreground terminal result.
    pub async fn execute(mut self) -> Result<()> {
        if let Some(request) = self.session_title.take() {
            crate::generate_and_persist_session_title(
                request.root_config,
                request.workspace_root,
                request.model_ref,
                request.session_log_path,
                request.session_id,
                request.prompt,
                request.managed_writer,
            )
            .await?;
        }
        Ok(())
    }
}

struct ApplicationTaskExecutionRuntime {
    root_config: RootConfig,
    parent_session_ref: SessionRef,
    options: AgentRunOptions,
    base_registry: sigil_kernel::ToolRegistry,
    agent_supervisor: crate::AgentSupervisor,
    role_provider_builder:
        Arc<dyn crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder>,
    verification_execution_port:
        Option<Arc<dyn sigil_kernel::verification::VerificationExecutionPortV1>>,
}

/// Runtime facts used to execute a read-only plan review after an automatic route decision.
struct ApplicationPlanReviewRuntime {
    options: AgentRunOptions,
    agent: Box<Agent<Box<dyn sigil_kernel::Provider>>>,
    tool_registry: sigil_kernel::ToolRegistry,
    workspace_snapshot_id: Option<String>,
    child_resource_provisioner:
        Option<Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>>,
}

enum ApplicationRunExecutionKind {
    Main {
        agent: Box<Agent<Box<dyn sigil_kernel::Provider>>>,
        input: Box<AgentRunInput>,
        agent_tool_runtime: Option<Box<crate::AgentToolRuntime>>,
    },
    AgentProfile {
        runtime: Box<crate::AgentToolRuntime>,
        profile_id: AgentProfileId,
    },
    ExplicitPlanReview {
        request: Box<crate::PlanReviewRunRequest>,
    },
}

/// Physical extension settlement failed after the run had entered its existing owners.
/// Adapters may distinguish this from an ordinary provider failure without parsing diagnostics.
#[derive(Debug, thiserror::Error)]
#[error(
    "application extension cleanup incomplete: {cleanup:#}; preceding run error: {execution:?}"
)]
pub struct ApplicationRunCleanupError {
    #[source]
    cleanup: anyhow::Error,
    execution: Option<anyhow::Error>,
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
    /// Machine-readable receipt for the exact route admitted by this invocation.
    pub route_transition: crate::provider_connections::SessionRouteTransitionView,
    /// Kernel agent output.
    pub agent_output: AgentRunOutput,
    /// Non-critical work that adapters must execute only after releasing foreground ownership.
    pub post_run_maintenance: Option<ApplicationPostRunMaintenance>,
}

impl ApplicationRunExecution {
    /// Consumes a prepared run that will not execute, joining its idle extension owners before
    /// releasing the foreground guard. Existing background children retain their generations.
    /// This records no success or cancellation terminal; that remains the control owner's job.
    ///
    /// # Errors
    /// Returns an error when extension cleanup could not be confirmed.
    pub async fn settle_without_execution(self) -> Result<()> {
        crate::verification_lifecycle::settle_idle_mcp(
            &self.extension_registry,
            &self.extension_background_runs,
        )
        .await
        .map_err(|cleanup| {
            ApplicationRunCleanupError {
                cleanup,
                execution: None,
            }
            .into()
        })
    }

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
        self.execute_with_settlement(handler, approval_handler, false)
            .await
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
        let registry = self.extension_registry.clone();
        let background_runs = self.extension_background_runs.clone();
        let runtime = tokio::runtime::Handle::current();
        match tokio::task::spawn_blocking(move || {
            runtime.block_on(self.execute_with_settlement(
                &mut handler,
                &mut approval_handler,
                true,
            ))
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let settlement =
                    crate::verification_lifecycle::settle_idle_mcp(&registry, &background_runs)
                        .await;
                let error = anyhow!(error).context("application run owned blocking worker failed");
                match settlement {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(ApplicationRunCleanupError {
                        cleanup,
                        execution: Some(error),
                    }
                    .into()),
                }
            }
        }
    }

    async fn execute_with_settlement<H, A>(
        self,
        handler: &mut H,
        approval_handler: &mut A,
        owned_blocking: bool,
    ) -> Result<ApplicationRunOutput>
    where
        H: ApplicationRunEventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let registry = self.extension_registry.clone();
        let background_runs = self.extension_background_runs.clone();
        let result = async {
            validate_execution_contract(self.interaction, approval_handler, owned_blocking)?;
            self.execute_inner(handler, approval_handler).await
        }
        .await;
        // Normal terminals settle before publishing below. Join on early errors as well;
        // registry retirement is idempotent and keeps live child generations intact.
        if let Err(error) = result {
            return match crate::verification_lifecycle::settle_idle_mcp(&registry, &background_runs)
                .await
            {
                Ok(()) => Err(error),
                Err(cleanup) => Err(ApplicationRunCleanupError {
                    cleanup,
                    execution: Some(error),
                }
                .into()),
            };
        }
        result
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
        self.conversation_lifecycle
            .append_started(&self.conversation_start)
            .context("failed to persist application conversation run start")?;
        let mut bridge = PublicApplicationEventBridge::new(self.events.clone(), handler)?;
        if let Err(error) = self
            .session
            .record_bound_conversation_run_admission(&self.run_id)
        {
            let error = error.context("failed to persist application command run admission");
            let safe_error = self.redactor.redact_text(&format!("{error:#}"));
            if let Err(terminal_error) = bridge.emit_conversation_terminal(
                &self.conversation_lifecycle,
                &self.run_id,
                ApplicationRunTerminalStatus::Failed,
                None,
                Some(&safe_error),
                &self.redactor,
                PublicRunEventKind::RunFailed {
                    error: safe_error.clone(),
                },
            ) {
                return Err(error).context(format!(
                    "application admission failure terminal was not confirmed; durable recovery must decide the terminal: {terminal_error:#}"
                ));
            }
            return Err(error);
        }
        if let Err(error) = bridge.emit(PublicRunEventKind::RunStarted {
            prompt: self.prompt.clone(),
        }) {
            if is_application_public_outbox_append_error(&error) {
                return Err(error).context(
                    "application run start public outbox append was not confirmed; durable recovery must decide the next terminal",
                );
            }
            let safe_error = self.redactor.redact_text(&format!("{error:#}"));
            bridge.emit_conversation_terminal(
                &self.conversation_lifecycle,
                &self.run_id,
                ApplicationRunTerminalStatus::Failed,
                None,
                Some(&safe_error),
                &self.redactor,
                PublicRunEventKind::RunFailed {
                    error: safe_error.clone(),
                },
            )?;
            return Err(error).context("application run start event delivery failed");
        }
        bridge.emit(PublicRunEventKind::RouteTransition {
            transition: application_public_route_transition(&self.route_transition),
        })?;
        for warning in std::mem::take(&mut self.warnings) {
            if let Err(error) = bridge.emit(PublicRunEventKind::Notice { message: warning }) {
                if is_application_public_outbox_append_error(&error) {
                    return Err(error).context(
                        "application run warning public outbox append was not confirmed; durable recovery must decide the next terminal",
                    );
                }
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                bridge.emit_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ApplicationRunTerminalStatus::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
                    PublicRunEventKind::RunFailed {
                        error: safe_error.clone(),
                    },
                )?;
                return Err(error).context("application run notice delivery failed");
            }
        }
        let user_input_continuation = self.pending_user_input_continuation.take();
        if let Some(context) = user_input_continuation.as_ref() {
            let request = match user_input::start_application_user_input_continuation(
                &mut self.session,
                context,
            ) {
                Ok(request) => request,
                Err(error) => {
                    let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                    bridge.emit_conversation_terminal(
                        &self.conversation_lifecycle,
                        &self.run_id,
                        ApplicationRunTerminalStatus::Failed,
                        None,
                        Some(&safe_error),
                        &self.redactor,
                        PublicRunEventKind::RunFailed {
                            error: safe_error.clone(),
                        },
                    )?;
                    return Err(error).context("failed to start user-input continuation");
                }
            };
            if let Err(error) =
                bridge.emit(user_input::application_user_input_changed_event(request))
            {
                if is_application_public_outbox_append_error(&error) {
                    return Err(error).context(
                        "application user-input public outbox append was not confirmed; durable recovery must decide the next terminal",
                    );
                }
                let resolution = user_input::reconcile_failed_application_user_input_continuation(
                    &mut self.session,
                    context,
                );
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                bridge.emit_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ApplicationRunTerminalStatus::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
                    PublicRunEventKind::RunFailed {
                        error: safe_error.clone(),
                    },
                )?;
                if let Err(resolution_error) = resolution {
                    return Err(error).context(format!(
                        "user-input continuation event delivery failed and resolution append failed: {resolution_error:#}"
                    ));
                }
                return Err(error).context("user-input continuation event delivery failed");
            }
        }
        let run = match self.kind {
            ApplicationRunExecutionKind::Main {
                agent,
                input,
                mut agent_tool_runtime,
            } => {
                if let Some(runtime) = agent_tool_runtime.as_mut() {
                    agent
                        .run_with_approval_input_and_agent_delegate(
                            &mut self.session,
                            *input,
                            self.options,
                            &mut bridge,
                            approval_handler,
                            runtime.as_mut(),
                        )
                        .await
                } else {
                    agent
                        .run_with_approval_input(
                            &mut self.session,
                            *input,
                            self.options,
                            &mut bridge,
                            approval_handler,
                        )
                        .await
                }
            }
            ApplicationRunExecutionKind::AgentProfile {
                mut runtime,
                profile_id,
            } => {
                execute_application_agent_profile(
                    &mut runtime,
                    &mut self.session,
                    profile_id,
                    self.prompt.clone(),
                    &self.options,
                    &mut bridge,
                    approval_handler,
                )
                .await
            }
            ApplicationRunExecutionKind::ExplicitPlanReview { request } => {
                let runtime = self
                    .plan_review_runtime
                    .take()
                    .ok_or_else(|| anyhow!("explicit plan review runtime is unavailable"))?;
                run_application_plan_review_request(
                    &mut self.session,
                    AgentRunOutput {
                        disposition: AgentRunDisposition::FinalAnswer,
                        result: AgentRunResult {
                            final_text: String::new(),
                            tool_calls: 0,
                            final_message_id: None,
                        },
                        outcome: AgentRunOutcome::default(),
                    },
                    runtime,
                    *request,
                    &mut bridge,
                    approval_handler,
                    &self.cancellation_handle,
                    &self.run_id,
                    &self.redactor,
                )
                .await
            }
        };
        let run = match run {
            Ok(agent_output) => {
                let output = continue_application_task_handoff(
                    &mut self.session,
                    agent_output,
                    self.task_execution.take(),
                    &mut bridge,
                    approval_handler,
                    &self.cancellation_handle,
                )
                .await;
                match output {
                    Ok(output) => {
                        continue_application_plan_review(
                            &mut self.session,
                            output,
                            self.plan_review_runtime.take(),
                            &mut bridge,
                            approval_handler,
                            &self.cancellation_handle,
                            &self.run_id,
                            &self.redactor,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        let run: Result<AgentRunOutput> = match (user_input_continuation.as_ref(), run) {
            (Some(context), Ok(agent_output)) => {
                match user_input::resolve_application_user_input_continuation(
                    &mut self.session,
                    context,
                    sigil_kernel::UserInputResolutionV1::Consumed,
                ) {
                    Ok(request) => bridge
                        .emit(user_input::application_user_input_changed_event(request))
                        .map(|()| agent_output),
                    Err(error) => Err(error),
                }
            }
            (Some(context), Err(error)) => {
                match user_input::reconcile_failed_application_user_input_continuation(
                    &mut self.session,
                    context,
                ) {
                    Ok(request) => {
                        if let Err(event_error) =
                            bridge.emit(user_input::application_user_input_changed_event(request))
                        {
                            Err(error.context(format!(
                                "user-input continuation failed and resolution delivery failed: {event_error:#}"
                            )))
                        } else {
                            Err(error)
                        }
                    }
                    Err(resolution_error) => Err(error.context(format!(
                        "user-input continuation failed and resolution append failed: {resolution_error:#}"
                    ))),
                }
            }
            (None, run) => run,
        };
        let settlement = crate::verification_lifecycle::settle_idle_mcp(
            &self.extension_registry,
            &self.extension_background_runs,
        )
        .await;
        let run = match (run, settlement) {
            (run, Ok(())) => run,
            (Ok(_), Err(cleanup)) => Err(ApplicationRunCleanupError {
                cleanup,
                execution: None,
            }
            .into()),
            (Err(error), Err(cleanup)) => Err(ApplicationRunCleanupError {
                cleanup,
                execution: Some(error),
            }
            .into()),
        };
        match run {
            Ok(agent_output) => {
                let (terminal_status, terminal_event) =
                    application_terminal_projection(&agent_output);
                let summary = match &terminal_event {
                    PublicRunEventKind::RunFailed { error } => Some(error.clone()),
                    PublicRunEventKind::RunBlocked { reason }
                    | PublicRunEventKind::RunPaused { reason }
                    | PublicRunEventKind::RunInterrupted { reason } => Some(reason.clone()),
                    _ => None,
                };
                let final_message_id = (terminal_status == ApplicationRunTerminalStatus::Succeeded)
                    .then(|| agent_output.result.final_message_id.clone())
                    .flatten();
                if !bridge.conversation_terminal_committed()? {
                    bridge.emit_conversation_terminal(
                        &self.conversation_lifecycle,
                        &self.run_id,
                        terminal_status,
                        final_message_id,
                        summary.as_deref(),
                        &self.redactor,
                        terminal_event,
                    )?;
                }
                if let Some(managed_session_log) = self.managed_session_log.take() {
                    managed_session_log
                        .finalize()
                        .context("failed to finalize managed session-log namespace")?;
                }
                if let Some(managed_artifact_store) = self.managed_artifact_store.take() {
                    managed_artifact_store
                        .finalize()
                        .context("failed to finalize managed artifact namespaces")?;
                }
                let post_run_maintenance = ApplicationPostRunMaintenance::from_session_title(
                    self.pending_session_title.take(),
                );
                Ok(ApplicationRunOutput {
                    session_id: self.session_id,
                    run_id: self.run_id,
                    session_log_path: self.session_log_path,
                    terminal_status,
                    route_transition: self.route_transition,
                    agent_output,
                    post_run_maintenance,
                })
            }
            // Cancellation has precedence over a provider recovery terminal.  A transport
            // wait can observe cancellation and return a recovery-specific error while the
            // foreground owner is already closing the run.  Let the adapter finalize that
            // cancellation instead of publishing a competing blocked terminal.
            Err(error) if self.cancellation_handle.is_cancel_requested() => Err(error)
                .context("application run cancellation is pending terminal cleanup confirmation"),
            Err(error) if is_application_public_outbox_append_error(&error) => Err(error).context(
                "application public outbox append was not confirmed; durable recovery must decide the next terminal",
            ),
            Err(error)
                if error
                    .downcast_ref::<sigil_kernel::ProviderTurnRecoveryTerminalError>()
                    .is_some() =>
            {
                let recovery = error
                    .downcast_ref::<sigil_kernel::ProviderTurnRecoveryTerminalError>()
                    .expect("recovery error was checked by the match guard");
                // The kernel already emitted the recovery detail and durably paused its logical
                // run. Commit the adapter-owned foreground terminal with its exact public
                // outbox event instead of allowing this outer async-error boundary to overwrite
                // it with a generic failed conversation run.
                let summary = format!(
                    "provider turn recovery {:?}: {}",
                    recovery.disposition, recovery.reason_code
                );
                let (terminal_status, terminal_event) =
                    application_provider_recovery_terminal(recovery, &summary);
                bridge.emit_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    terminal_status,
                    None,
                    Some(&summary),
                    &self.redactor,
                    terminal_event,
                )?;
                if let Some(managed_session_log) = self.managed_session_log.take() {
                    managed_session_log
                        .finalize()
                        .context("failed to finalize managed session-log namespace")?;
                }
                if let Some(managed_artifact_store) = self.managed_artifact_store.take() {
                    managed_artifact_store
                        .finalize()
                        .context("failed to finalize managed artifact namespaces")?;
                }
                Ok(ApplicationRunOutput {
                    session_id: self.session_id,
                    run_id: self.run_id,
                    session_log_path: self.session_log_path,
                    terminal_status,
                    route_transition: self.route_transition,
                    agent_output: AgentRunOutput {
                        disposition: match terminal_status {
                            ApplicationRunTerminalStatus::Paused => AgentRunDisposition::Blocked,
                            ApplicationRunTerminalStatus::Blocked => AgentRunDisposition::Blocked,
                            ApplicationRunTerminalStatus::Cancelled
                            | ApplicationRunTerminalStatus::Interrupted
                            | ApplicationRunTerminalStatus::Failed
                            | ApplicationRunTerminalStatus::Succeeded
                            | ApplicationRunTerminalStatus::AwaitingUserInput => {
                                AgentRunDisposition::Blocked
                            }
                        },
                        result: AgentRunResult {
                            final_text: String::new(),
                            tool_calls: 0,
                            final_message_id: None,
                        },
                        outcome: AgentRunOutcome {
                            terminal_reason: AgentRunTerminalReason::DelegationUnsatisfied,
                            ..AgentRunOutcome::default()
                        },
                    },
                    post_run_maintenance: ApplicationPostRunMaintenance::from_session_title(
                        self.pending_session_title.take(),
                    ),
                })
            }
            Err(error) => {
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                bridge.emit_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ApplicationRunTerminalStatus::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
                    PublicRunEventKind::RunFailed {
                        error: safe_error.clone(),
                    },
                )?;
                Err(error)
            }
        }
    }
}

/// Converts the shared route transition receipt into the stable public machine/event DTO.
#[must_use]
pub fn application_public_route_transition(
    transition: &crate::provider_connections::SessionRouteTransitionView,
) -> sigil_kernel::PublicSessionRouteTransitionView {
    sigil_kernel::PublicSessionRouteTransitionView {
        kind: match transition.kind {
            crate::provider_connections::SessionRouteTransitionKind::Exact => {
                sigil_kernel::PublicSessionRouteTransitionKind::Exact
            }
            crate::provider_connections::SessionRouteTransitionKind::Rebound => {
                sigil_kernel::PublicSessionRouteTransitionKind::Rebound
            }
            crate::provider_connections::SessionRouteTransitionKind::ExplicitlyConfirmed => {
                sigil_kernel::PublicSessionRouteTransitionKind::ExplicitlyConfirmed
            }
        },
        connection_id: transition.connection_id.clone(),
        model_id: transition.model_id.clone(),
        remote_context_reset: transition.remote_context_reset,
    }
}

async fn continue_application_task_handoff<H, A>(
    session: &mut Session,
    output: AgentRunOutput,
    task_execution: Option<ApplicationTaskExecutionRuntime>,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: &RunCancellationHandle,
) -> Result<AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    if let AgentRunDisposition::RunPendingPlan(action) = output.disposition.clone() {
        let Some(task_execution) = task_execution else {
            bail!("pending plan execution requires an attached durable Task executor");
        };
        if action.source_turn.session_scope_id != session.session_scope_id() {
            bail!("pending plan execution action belongs to another session");
        }
        // The shared model route uses the same direct execution spine as every other surface:
        // typed command -> atomic approval plus host execution unit -> runner.
        let plan_id = sigil_kernel::PlanId::new(action.plan_id.as_str().to_owned())
            .map_err(|error| anyhow!("invalid plan id for pending plan execution: {error}"))?;
        let command = sigil_kernel::PlanRunCommandV1 {
            command_id: sigil_kernel::stable_event_uuid(
                "sigil-plan-run-command-v1",
                &format!(
                    "{}:{}:{}:run:current_policy",
                    session.session_scope_id(),
                    action.plan_id.as_str(),
                    action.plan_hash
                ),
            ),
            session_id: session.session_scope_id().to_owned(),
            plan_id: plan_id.clone(),
            expected_plan_hash: action.plan_hash.clone(),
            expected_durable_frontier: session.durable_frontier_sequence(),
            start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
            permission: sigil_kernel::PlanRunPermissionChoiceV1::KeepCurrentPolicy,
            source: sigil_kernel::PlanRunCommandSource::ModelTypedRoute,
        };
        let receipt = crate::PlanExecutionService::approve(
            session,
            task_execution.parent_session_ref.clone(),
            &command,
            crate::now_ms(),
        )
        .map_err(|rejection| {
            anyhow!(
                "pending plan execution was rejected: {}",
                crate::plan_run_rejection_message(&rejection)
            )
        })?;
        return run_application_admitted_task(
            session,
            output,
            receipt.task_id,
            task_execution,
            handler,
            approval_handler,
            cancellation_handle,
        )
        .await;
    }
    if let AgentRunDisposition::ContinueDurableTask(action) = output.disposition.clone() {
        // Keep the nested continuation state machine off the caller's executor stack.
        return Box::pin(continue_application_existing_task(
            session,
            output,
            *action,
            task_execution,
            handler,
            approval_handler,
            cancellation_handle,
        ))
        .await;
    }
    let AgentRunDisposition::StartDurableTask(action) = output.disposition.clone() else {
        return Ok(output);
    };
    let Some(task_execution) = task_execution else {
        return Ok(output);
    };
    run_application_admitted_task(
        session,
        output,
        action.task_id,
        task_execution,
        handler,
        approval_handler,
        cancellation_handle,
    )
    .await
}

async fn run_application_admitted_task<H, A>(
    session: &mut Session,
    output: AgentRunOutput,
    task_id: TaskId,
    task_execution: ApplicationTaskExecutionRuntime,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: &RunCancellationHandle,
) -> Result<AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let task = session.task_state_projection().tasks.get(&task_id).cloned();
    let Some(task) = task else {
        if !cancellation_handle.is_naturally_finalized()
            && !cancellation_handle.try_finalize_naturally()
        {
            bail!("run cancellation won the missing-task terminal-state race");
        }
        bail!(
            "accepted task handoff {} is missing its durable task",
            task_id.as_str()
        );
    };
    let ApplicationTaskExecutionRuntime {
        root_config,
        parent_session_ref: _,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
    } = task_execution;
    let status = crate::agent_supervisor::task_execution::run_admitted_task_to_root_terminal(
        session,
        crate::agent_supervisor::task_execution::AdmittedTaskExecution {
            task_id: task_id.clone(),
            parent_session_ref: task.parent_session_ref,
            objective: task.objective,
            root_config,
            options,
            base_registry,
            agent_supervisor,
            role_provider_builder: role_provider_builder.as_ref(),
            handler,
            cancellation_handle: cancellation_handle.clone(),
            tool_artifact_read_budget: None,
            verification_execution_port: verification_execution_port
                .context("current-schema task execution requires the managed verification route")?,
        },
        approval_handler,
    )
    .await?;
    application_task_terminal_output(session, &task_id, status, output)
}

async fn continue_application_existing_task<H, A>(
    session: &mut Session,
    output: AgentRunOutput,
    action: sigil_kernel::ContinueDurableTaskAction,
    task_execution: Option<ApplicationTaskExecutionRuntime>,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: &RunCancellationHandle,
) -> Result<AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let Some(task_execution) = task_execution else {
        return Ok(output);
    };
    // A live application can continue a direct Task without crossing the session-open transition.
    let task = match crate::validate_task_continuation_action(session, &action) {
        Ok(task) => task,
        Err(error) => {
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the stale-task terminal-state race");
            }
            return Err(error).context("typed task continuation is stale");
        }
    };
    let ApplicationTaskExecutionRuntime {
        root_config,
        parent_session_ref: _,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
    } = task_execution;
    let result = crate::agent_supervisor::task_execution::bind_task_run_cancellation_scope(
        session,
        &action.task_id,
        cancellation_handle,
    );
    let continuation_entry_frontier = session.entries().len();
    let result =
        match result {
            Ok(()) => crate::agent_supervisor::task_execution::continue_task_execution(
                session,
                crate::agent_supervisor::task_execution::ContinuedTaskExecution {
                    requested_task_id: Some(action.task_id.clone()),
                    guidance: match action.control() {
                        sigil_kernel::TaskContinuationControl::ResumeTask => None,
                        sigil_kernel::TaskContinuationControl::ApplyTaskGuidance(guidance) => {
                            Some(guidance)
                        }
                    },
                    guidance_promotion: None,
                    continuation_guidance_receipt: Some(action.guidance_receipt),
                    explicit_guidance_run_id: None,
                    root_config,
                    options,
                    base_registry,
                    agent_supervisor,
                    role_provider_builder: role_provider_builder.as_ref(),
                    handler,
                    cancellation_handle: cancellation_handle.clone(),
                    tool_artifact_read_budget: None,
                    verification_execution_port: verification_execution_port.context(
                        "current-schema task continuation requires the managed verification route",
                    )?,
                },
                approval_handler,
            )
            .await,
            Err(error) => Err(error),
        };
    let result = match result {
        Err(error) if is_application_public_outbox_append_error(&error) => {
            return Err(error).context(
                "typed Task continuation public outbox append was not confirmed; durable recovery must decide the next terminal",
            );
        }
        result => result,
    };
    let status = crate::agent_supervisor::task_execution::finalize_task_continuation_root(
        session,
        &action.task_id,
        &task.parent_session_ref,
        &task.objective,
        cancellation_handle,
        continuation_entry_frontier,
        result,
    )?;
    application_task_terminal_output(session, &action.task_id, status, output)
}

async fn continue_application_plan_review<H, A>(
    session: &mut Session,
    output: AgentRunOutput,
    plan_review_runtime: Option<ApplicationPlanReviewRuntime>,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: &RunCancellationHandle,
    run_id: &str,
    redactor: &sigil_kernel::SecretRedactor,
) -> Result<AgentRunOutput>
where
    H: EventHandler + ApplicationRunEventHandler + Send,
    A: ApprovalHandler + Send,
{
    let AgentRunDisposition::StartPlanReview(action) = output.disposition.clone() else {
        return Ok(output);
    };
    let Some(runtime) = plan_review_runtime else {
        return Ok(output);
    };
    let request = crate::PlanReviewCoordinator::prepare_automatic_plan_review(
        session,
        &action,
        runtime.workspace_snapshot_id.clone(),
        current_unix_time_ms(),
    )?;
    run_application_plan_review_request(
        session,
        output,
        runtime,
        request,
        handler,
        approval_handler,
        cancellation_handle,
        run_id,
        redactor,
    )
    .await
}

async fn run_application_plan_review_request<H, A>(
    session: &mut Session,
    output: AgentRunOutput,
    runtime: ApplicationPlanReviewRuntime,
    request: crate::PlanReviewRunRequest,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: &RunCancellationHandle,
    run_id: &str,
    redactor: &sigil_kernel::SecretRedactor,
) -> Result<AgentRunOutput>
where
    H: EventHandler + ApplicationRunEventHandler + Send,
    A: ApprovalHandler + Send,
{
    if request.revision_request_id.is_some() {
        bail!("revision plan review must execute through the atomic revision terminal/outbox path");
    }
    let ApplicationPlanReviewRuntime {
        options,
        agent,
        tool_registry,
        child_resource_provisioner,
        ..
    } = runtime;
    let outcome = match child_resource_provisioner {
        Some(provisioner) => {
            crate::PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
                session,
                &request,
                agent.as_ref(),
                options,
                tool_registry,
                handler,
                approval_handler,
                cancellation_handle.clone(),
                provisioner,
            )
            .await
        }
        None => {
            #[cfg(test)]
            {
                crate::PlanReviewCoordinator::run_plan_review(
                    session,
                    &request,
                    agent.as_ref(),
                    options,
                    tool_registry,
                    handler,
                    approval_handler,
                    cancellation_handle.clone(),
                )
                .await
            }
            #[cfg(not(test))]
            {
                Err(anyhow!(
                    "current-schema plan review requires the managed child resource bundle"
                ))
            }
        }
    };
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if is_application_public_outbox_append_error(&error) {
                return Err(error).context("plan review public control append was not confirmed");
            }
            let close = crate::PlanReviewCoordinator::close_plan_review_run_if_open(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Failed(
                    "plan review run failed before an outcome".to_owned(),
                ),
                handler,
                current_unix_time_ms(),
            );
            if let Err(close_error) = close {
                return Err(close_error).context(format!(
                    "plan review run failed ({error:#}) and its terminal closure failed"
                ));
            }
            return Err(error);
        }
    };
    match outcome {
        crate::PlanReviewRunOutcome::AwaitingUserInput { request: pending } => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::AwaitingUserInput {
                    request: pending.clone(),
                },
                handler,
                current_unix_time_ms(),
            )?;
            Ok(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text: String::new(),
                    tool_calls: output.result.tool_calls,
                    final_message_id: None,
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::AwaitingUserInput(
                    sigil_kernel::UserInputRequestRefV1 {
                        identity: pending.identity.clone(),
                        request_hash: pending.request_hash.clone(),
                    },
                ),
            })
        }
        crate::PlanReviewRunOutcome::DraftReady { draft } => {
            let final_text = format!("Plan ready: {}", draft.summary);
            let recorded_at_ms = current_unix_time_ms();
            let controls = crate::PlanReviewCoordinator::plan_review_draft_terminal_controls(
                session,
                &draft,
                &request,
                recorded_at_ms,
            )?;
            let mut message =
                ModelMessage::assistant(Some(safe_persistence_text(&final_text)), Vec::new());
            message.assistant_kind = Some(AssistantMessageKind::FinalAnswer);
            let mut final_message_id = message.id.clone();
            let terminal = ConversationRunFinalizedEntryV1::new(
                run_id,
                ApplicationRunTerminalStatus::Succeeded,
                Some(final_message_id.clone()),
                None,
                recorded_at_ms,
                redactor,
            )?;
            let terminal_event = PublicRunEventKind::RunFinished {
                final_text: final_text.clone(),
            };
            let mut entries = controls
                .iter()
                .cloned()
                .map(SessionLogEntry::Control)
                .collect::<Vec<_>>();
            entries.push(SessionLogEntry::Assistant(message.clone()));
            if !handler.commit_plan_review_terminal(
                session,
                entries,
                vec![SessionPublicEventProjectionV1::assistant_message(
                    controls.len(),
                    message,
                )],
                terminal,
                terminal_event,
            )? {
                crate::PlanReviewCoordinator::commit_draft_from_child(
                    session,
                    &draft,
                    &request,
                    handler,
                    recorded_at_ms,
                )?;
                final_message_id =
                    append_application_final_answer(session, handler, final_text.clone())?;
            }
            Ok(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text,
                    tool_calls: output.result.tool_calls,
                    final_message_id: Some(final_message_id),
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::FinalAnswer,
            })
        }
        crate::PlanReviewRunOutcome::CompletedWithoutDraft => {
            let final_text = "Plan review closed without a ready draft or Task. Any unconfirmed candidate remains review-only; make an explicit decision before execution.".to_owned();
            let recorded_at_ms = current_unix_time_ms();
            let controls = crate::PlanReviewCoordinator::plan_review_no_draft_terminal_controls(
                session,
                &request,
                recorded_at_ms,
            )?;
            let mut message =
                ModelMessage::assistant(Some(safe_persistence_text(&final_text)), Vec::new());
            message.assistant_kind = Some(AssistantMessageKind::FinalAnswer);
            let mut final_message_id = message.id.clone();
            let terminal = ConversationRunFinalizedEntryV1::new(
                run_id,
                ApplicationRunTerminalStatus::Succeeded,
                Some(final_message_id.clone()),
                None,
                recorded_at_ms,
                redactor,
            )?;
            let terminal_event = PublicRunEventKind::RunFinished {
                final_text: final_text.clone(),
            };
            let mut entries = controls
                .iter()
                .cloned()
                .map(SessionLogEntry::Control)
                .collect::<Vec<_>>();
            entries.push(SessionLogEntry::Assistant(message.clone()));
            if !handler.commit_plan_review_terminal(
                session,
                entries,
                vec![SessionPublicEventProjectionV1::assistant_message(
                    controls.len(),
                    message,
                )],
                terminal,
                terminal_event,
            )? {
                crate::PlanReviewCoordinator::complete_without_draft(
                    session,
                    &request,
                    handler,
                    recorded_at_ms,
                )?;
                final_message_id =
                    append_application_final_answer(session, handler, final_text.clone())?;
            }
            Ok(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text,
                    tool_calls: output.result.tool_calls,
                    final_message_id: Some(final_message_id),
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::FinalAnswer,
            })
        }
        crate::PlanReviewRunOutcome::Cancelled => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Cancelled,
                handler,
                current_unix_time_ms(),
            )?;
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the plan review terminal-state race");
            }
            Ok::<AgentRunOutput, anyhow::Error>(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text: String::new(),
                    tool_calls: output.result.tool_calls,
                    final_message_id: None,
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::Interrupted,
            })
        }
        crate::PlanReviewRunOutcome::Interrupted(reason) => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Interrupted(reason.clone()),
                handler,
                current_unix_time_ms(),
            )?;
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the plan review interruption terminal-state race");
            }
            Ok::<AgentRunOutput, anyhow::Error>(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text: String::new(),
                    tool_calls: output.result.tool_calls,
                    final_message_id: None,
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::Interrupted,
            })
        }
        crate::PlanReviewRunOutcome::Blocked(reason) => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Blocked(reason),
                handler,
                current_unix_time_ms(),
            )?;
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the plan review blocker terminal-state race");
            }
            Ok(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text: String::new(),
                    tool_calls: output.result.tool_calls,
                    final_message_id: None,
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::Blocked,
            })
        }
        crate::PlanReviewRunOutcome::Paused(reason) => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Paused(reason),
                handler,
                current_unix_time_ms(),
            )?;
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the plan review pause terminal-state race");
            }
            Ok(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text: String::new(),
                    tool_calls: output.result.tool_calls,
                    final_message_id: None,
                },
                outcome: output.outcome,
                // ApplicationRun currently has no independent PlanReview pause disposition. The
                // durable plan-review status remains Paused; the enclosing run is blocked from
                // advancing until the retry/resource boundary is resolved.
                disposition: AgentRunDisposition::Blocked,
            })
        }
        crate::PlanReviewRunOutcome::Failed(error) => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Failed(error.clone()),
                handler,
                current_unix_time_ms(),
            )?;
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the plan review failure terminal-state race");
            }
            Ok::<AgentRunOutput, anyhow::Error>(AgentRunOutput {
                result: sigil_kernel::AgentRunResult {
                    final_text: String::new(),
                    tool_calls: output.result.tool_calls,
                    final_message_id: None,
                },
                outcome: output.outcome,
                disposition: AgentRunDisposition::Blocked,
            })
            .context(format!("plan review failed: {error}"))
        }
    }
}

fn append_application_final_answer(
    session: &mut Session,
    handler: &mut (impl EventHandler + ?Sized),
    text: String,
) -> Result<String> {
    let mut message = ModelMessage::assistant(Some(safe_persistence_text(&text)), Vec::new());
    message.assistant_kind = Some(AssistantMessageKind::FinalAnswer);
    let final_message_id = message.id.clone();
    handler.commit_session_publications(
        session,
        vec![SessionLogEntry::Assistant(message.clone())],
        vec![SessionPublicEventProjectionV1::assistant_message(
            0, message,
        )],
    )?;
    Ok(final_message_id)
}

struct ApplicationTaskFinalAnswer {
    message_id: String,
    text: String,
}

fn application_task_final_answer(
    session: &Session,
    task_id: &TaskId,
) -> Result<ApplicationTaskFinalAnswer> {
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(task_id)
        .ok_or_else(|| anyhow!("completed application task is missing"))?;
    let (message_id, expected_hash) = task
        .direct_execution_attempts
        .values()
        .filter(|attempt| attempt.status == sigil_kernel::TaskExecutionAttemptStatus::Completed)
        .max_by_key(|attempt| attempt.ordinal)
        .and_then(|attempt| {
            Some((
                attempt.final_message_id.clone()?,
                attempt.output_hash.clone()?,
            ))
        })
        .ok_or_else(|| anyhow!("completed application task has no committed final answer"))?;
    let message = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Assistant(message) if message.id == message_id => Some(message),
            _ => None,
        })
        .ok_or_else(|| anyhow!("completed application task final message is missing"))?;
    if message.assistant_kind != Some(AssistantMessageKind::FinalAnswer) {
        bail!("completed application task message is not a final answer");
    }
    let text = safe_persistence_text(message.content.as_deref().unwrap_or_default());
    if text.trim().is_empty() {
        bail!("completed application task final answer is empty");
    }
    if format!("sha256:{}", sigil_kernel::sha256_hex(text.as_bytes())) != expected_hash {
        bail!("completed application task final answer hash does not match durable authority");
    }
    Ok(ApplicationTaskFinalAnswer { message_id, text })
}

#[derive(Debug, thiserror::Error)]
#[error("task {} failed: {reason}", task_id.as_str())]
struct ApplicationTaskFailed {
    task_id: TaskId,
    reason: String,
}

fn application_task_terminal_output(
    session: &Session,
    task_id: &TaskId,
    status: TaskRunStatus,
    mut output: AgentRunOutput,
) -> Result<AgentRunOutput> {
    match status {
        TaskRunStatus::Completed => {
            let answer = application_task_final_answer(session, task_id)?;
            output.result.final_text = answer.text;
            output.result.final_message_id = Some(answer.message_id);
            output.disposition = AgentRunDisposition::FinalAnswer;
            output.outcome.terminal_reason = AgentRunTerminalReason::FinalAnswer;
        }
        TaskRunStatus::Cancelled | TaskRunStatus::Interrupted => {
            output.result.final_text.clear();
            output.result.final_message_id = None;
            output.disposition = AgentRunDisposition::Interrupted;
            output.outcome.terminal_reason = AgentRunTerminalReason::TaskHandoff;
        }
        TaskRunStatus::Paused => {
            output.result.final_text.clear();
            output.result.final_message_id = None;
            let pending = sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(
                session.entries(),
            )?
            .pending()
            .find(|route| {
                matches!(
                    &route.request.source,
                    sigil_kernel::UserInputSourceV1::Agent
                )
            })
            .map(|route| sigil_kernel::UserInputRequestRefV1 {
                identity: route.request.identity.clone(),
                request_hash: route.request.request_hash.clone(),
            });
            if let Some(request) = pending {
                output.disposition = AgentRunDisposition::AwaitingUserInput(request);
                output.outcome.terminal_reason = AgentRunTerminalReason::AwaitingUserInput;
            } else {
                output.disposition = AgentRunDisposition::Blocked;
                output.outcome.terminal_reason = AgentRunTerminalReason::TaskHandoff;
            }
        }
        TaskRunStatus::Failed => {
            let projection = session.task_state_projection();
            let task = projection
                .tasks
                .get(task_id)
                .ok_or_else(|| anyhow!("failed application task is missing"))?;
            return Err(ApplicationTaskFailed {
                task_id: task_id.clone(),
                reason: task.reason.clone().unwrap_or_else(|| {
                    "task execution failed without a recorded reason".to_owned()
                }),
            }
            .into());
        }
        TaskRunStatus::Started | TaskRunStatus::Running => {
            output.result.final_text.clear();
            output.result.final_message_id = None;
            output.disposition = AgentRunDisposition::Blocked;
            output.outcome.terminal_reason = AgentRunTerminalReason::TaskHandoff;
        }
    }
    Ok(output)
}

async fn execute_application_agent_profile(
    runtime: &mut crate::AgentToolRuntime,
    session: &mut Session,
    profile_id: AgentProfileId,
    prompt: String,
    options: &AgentRunOptions,
    handler: &mut (dyn EventHandler + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<AgentRunOutput> {
    let safe_prompt = safe_persistence_text(&prompt);
    session.append_user_message(ModelMessage::user(safe_prompt))?;
    let invocation = runtime
        .invoke_agent_profile(
            session,
            profile_id.clone(),
            prompt,
            options,
            handler,
            approval_handler,
        )
        .await?;
    if invocation.status != Some(AgentThreadStatus::Completed) {
        bail!(
            "agent @{} ended with status {}",
            profile_id.as_str(),
            application_agent_status_label(invocation.status)
        );
    }
    let child_result = invocation.result.as_ref().ok_or_else(|| {
        anyhow!(
            "agent @{} completed without a durable result",
            profile_id.as_str()
        )
    })?;
    let child_summary = child_result.summary.trim();
    if child_summary.is_empty() {
        bail!(
            "agent @{} completed with an empty durable result",
            profile_id.as_str()
        );
    }
    let parent_summary = format!(
        "Agent @{} completed.\n\n{}",
        profile_id.as_str(),
        child_summary
    );
    let mut message = ModelMessage::assistant(Some(parent_summary.clone()), Vec::new());
    message.assistant_kind = Some(AssistantMessageKind::FinalAnswer);
    let final_message_id = message.id.clone();
    handler.commit_session_publications(
        session,
        vec![SessionLogEntry::Assistant(message.clone())],
        vec![SessionPublicEventProjectionV1::assistant_message(
            0, message,
        )],
    )?;
    Ok(AgentRunOutput {
        disposition: AgentRunDisposition::FinalAnswer,
        result: AgentRunResult {
            final_text: parent_summary,
            tool_calls: 0,
            final_message_id: Some(final_message_id),
        },
        outcome: AgentRunOutcome::default(),
    })
}

fn application_agent_status_label(status: Option<AgentThreadStatus>) -> &'static str {
    match status {
        Some(AgentThreadStatus::Started) => "started",
        Some(AgentThreadStatus::Running) => "running",
        Some(AgentThreadStatus::Blocked) => "blocked",
        Some(AgentThreadStatus::Completed) => "completed",
        Some(AgentThreadStatus::Failed) => "failed",
        Some(AgentThreadStatus::Cancelled) => "cancelled",
        Some(AgentThreadStatus::Interrupted) => "interrupted",
        Some(AgentThreadStatus::Closed) => "closed",
        Some(AgentThreadStatus::Unavailable) => "unavailable",
        Some(AgentThreadStatus::Unknown) | None => "unknown",
    }
}

#[allow(clippy::too_many_arguments)]
async fn assemble_application_tool_surface(
    root_config: &RootConfig,
    provider_capabilities: &sigil_kernel::ProviderCapabilities,
    workspace_root: &Path,
    mutation_recorder: MutationEventRecorder,
    workspace_trust: WorkspaceTrust,
    options: &AgentRunOptions,
    session: &Session,
    services: &ApplicationRunServices,
    redactor: &sigil_kernel::SecretRedactor,
    skill_descriptor: Option<&sigil_kernel::SkillDescriptor>,
    agent_delegation_available: bool,
    tool_scope: Option<&ToolRegistryScope>,
    terminal_lifecycle_sink: Arc<dyn sigil_kernel::TerminalLifecycleSink>,
    process_environments: crate::application_mcp::ProcessEnvironments,
) -> Result<(crate::RuntimeToolSurface, Vec<String>)> {
    let managed_extension_execution = services
        .authority_composition()
        .and_then(|composition| composition.extension_execution.clone());
    let surface = crate::mcp_registry::build_tool_surface_with_terminal_lifecycle_and_managed_extension_execution(
        root_config,
        provider_capabilities,
        workspace_root.to_path_buf(),
        mutation_recorder.clone(),
        workspace_trust,
        sigil_kernel::ExtensionProcessNetworkAdmission::new(
            options.permission_context.network_policy,
            false,
        ),
        terminal_lifecycle_sink,
        services.scratch_control().cloned(),
        services
            .authority_composition()
            .map(|composition| std::sync::Arc::clone(&composition.storage_writer)),
        managed_extension_execution.clone(),
        services
            .authority_composition()
            .map(|composition| std::sync::Arc::clone(&composition.command_execution)),
    )
    .await?;
    let mut cleanup_registry = surface.registry.clone();
    let assembled = async {
        // RFC-0062 14.1: one TTL sweep over the workspace scratch namespaces per application run
        // assembly. Leases are in-memory only, so this fresh process cannot hold one; the sweep
        // reclaims namespaces abandoned by crashed or deleted sessions and never races a live tool.
        {
            let scratch_control = surface.scratch_control.clone();
            tokio::task::spawn_blocking(move || {
                match scratch_control.gc_scratch_namespaces(
                    &sigil_tools_builtin::ScratchGcConfig::default(),
                    current_unix_time_ms(),
                ) {
                    Ok(report) if report.deleted > 0 => {
                        tracing::debug!(
                            deleted = report.deleted,
                            reclaimed_bytes = report.deleted_bytes,
                            "application runtime scratch TTL sweep reclaimed expired namespaces"
                        );
                    }
                    Ok(_report) => {}
                    Err(error) => {
                        tracing::debug!(%error, "application runtime scratch TTL sweep failed");
                    }
                }
            });
        }
        let crate::RuntimeToolSurface {
            mut registry,
            context_resolver,
            terminal_control,
            scratch_control,
        } = surface;
        if agent_delegation_available {
            crate::register_agent_tools(&mut registry, root_config)?;
        }
        let elicitation_handler = unsupported_mcp_elicitation_handler();
        let runtime_event_handler = unsupported_mcp_runtime_event_handler();
        crate::mcp_registry::attach_remote_mcp_activation_presenter_with_managed_extension_execution(
            &mut registry,
            root_config,
            provider_capabilities,
            workspace_root.to_path_buf(),
            Arc::clone(&elicitation_handler),
            Arc::clone(&runtime_event_handler),
            Arc::clone(&services.disclosure_presenter),
            managed_extension_execution.clone(),
        )?;
        let eager_remote_servers = root_config
            .mcp_servers
            .iter()
            .filter(|server| {
                server.startup == McpServerStartup::Eager && server.streamable_http().is_some()
            })
            .map(|server| (server.name.clone(), server.required))
            .collect::<Vec<_>>();
        let mut warnings = Vec::new();
        if let Some(session_log_path) = session.store_path() {
            let source: Arc<dyn crate::McpPluginTrustSource> =
                Arc::new(crate::SessionMcpPluginTrustSource::new(session_log_path));
            if root_config.skills.enabled && root_config.composition.allows(sigil_kernel::OptionalCapability::Skills) {
                let mut skill_registry = registry.clone();
                let skill_workspace = workspace_root.to_path_buf();
                let skill_config = root_config.skills.clone();
                let skill_source = Arc::clone(&source);
                // Keep discovery/session reads off the async owner, and join them before any
                // prepare result can retire this registry.
                let discovery = tokio::task::spawn_blocking(move || {
                    let user_config_dir = sigil_kernel::default_user_config_dir().ok();
                    crate::register_session_skill_tools(&mut skill_registry, &skill_workspace, user_config_dir.as_deref(), &skill_config, skill_source)
                }).await.context("plugin skill discovery worker failed")?;
                match discovery {
                    Ok(report) => warnings.extend(report.warnings.into_iter().map(|warning| warning.message)),
                    Err(error) => warnings.push(format!("optional plugin skills unavailable: {}", redactor.redact_text(&error.to_string()))),
                }
            }
            let startup_context = (
                mutation_recorder,
                sigil_kernel::ExtensionProcessNetworkAdmission::new(
                    options.permission_context.network_policy,
                    false,
                ),
            );
            let registry_views = services.extension_registry_views(session.session_scope_id())?;
            if let Err(error) = crate::mcp_registry::register_session_plugin_mcp_tools_with_registry_slot(
                &mut registry,
                root_config,
                provider_capabilities,
                workspace_root.to_path_buf(),
                source,
                Arc::clone(&elicitation_handler),
                Arc::clone(&runtime_event_handler),
                managed_extension_execution,
                Arc::clone(&services.disclosure_presenter),
                Some(startup_context),
                Some(registry_views.as_ref()),
                process_environments,
            )
            .await
            {
                warnings.push(format!(
                    "optional plugin MCP discovery unavailable: {}",
                    redactor.redact_text(&error.to_string())
                ));
            }
        }

        for (server_name, required) in eager_remote_servers {
            let activation = activate_eager_remote_mcp_server(
                &mut registry,
                root_config,
                &server_name,
                provider_capabilities.tool_name_max_chars,
                workspace_root.to_path_buf(),
                session.egress_audit_recorder()?,
                Arc::clone(&services.disclosure_presenter),
                Arc::clone(&elicitation_handler),
            )
            .await;
            if let Err(error) = activation {
                if required {
                    return Err(error);
                }
                warnings.push(optional_eager_mcp_warning(redactor, &server_name, &error));
            }
        }
        if let Some(executor) = services
            .authority_composition()
            .and_then(|composition| composition.plugin_hook_execution.clone())
            && let Some(session_log_path) = session.store_path()
        {
            let source = Arc::new(crate::mcp_registry::SessionMcpPluginTrustSource::new(
                session_log_path,
            ));
            match crate::plugin_workflow::register_plugin_workflow_tools(
                &mut registry,
                workspace_root,
                source,
                executor,
                redactor.clone(),
            )
            .await
            {
                Ok(plugin_warnings) => warnings.extend(plugin_warnings),
                Err(error) => warnings.push(format!(
                    "optional plugin hooks unavailable: {}",
                    redactor.redact_text(&error.to_string())
                )),
            }
        }
        if let Some(skill_descriptor) = skill_descriptor {
            registry = crate::build_skill_tool_registry(&registry, skill_descriptor).into_registry();
        }
        if let Some(scope) = tool_scope {
            registry = constrain_application_tool_registry(registry, scope)?;
        }
        Ok((
            crate::RuntimeToolSurface {
                registry,
                context_resolver,
                terminal_control,
                scratch_control,
            },
            warnings,
        ))
    }
    .await;
    if assembled.is_err()
        && let Err(cleanup) = crate::shutdown_mcp_generations(&mut cleanup_registry).await
    {
        return assembled.context(format!("MCP preparation cleanup incomplete: {cleanup:#}"));
    }
    assembled
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
    services
        .require_current_schema_authority()
        .map_err(application_authority_prepare_error)?;
    let (prepared, frozen_request) =
        prepare_application_run_internal(request, services, None).await?;
    debug_assert!(frozen_request.is_none());
    Ok(prepared)
}

pub(crate) async fn prepare_application_run_with_exact_first_request(
    request: ApplicationRunRequest,
    services: &ApplicationRunServices,
    exact_prompt: SecretString,
    durable_user_message_id: String,
) -> std::result::Result<
    (PreparedApplicationRun, ApplicationExactFirstRequestAssembly),
    ApplicationRunPrepareError,
> {
    services
        .require_current_schema_authority()
        .map_err(application_authority_prepare_error)?;
    if exact_prompt.expose_secret().trim().is_empty() || durable_user_message_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "queued exact prompt and durable user message id must not be empty".to_owned(),
        });
    }
    let (prepared, assembly) = prepare_application_run_internal(
        request,
        services,
        Some((exact_prompt, durable_user_message_id)),
    )
    .await?;
    let assembly = assembly.ok_or_else(|| ApplicationRunPrepareError::Internal {
        source: anyhow!("queued first request was not frozen by application assembly"),
    })?;
    Ok((prepared, assembly))
}

pub(crate) struct ApplicationExactFirstRequestAssembly {
    pub(crate) frozen_request: FrozenProviderRequestMaterial,
    pub(crate) run_input: AgentRunInput,
}

async fn prepare_application_run_internal(
    request: ApplicationRunRequest,
    services: &ApplicationRunServices,
    queued_first_request: Option<(SecretString, String)>,
) -> std::result::Result<
    (
        PreparedApplicationRun,
        Option<ApplicationExactFirstRequestAssembly>,
    ),
    ApplicationRunPrepareError,
> {
    if request.prompt.trim().is_empty() && request.image_attachments.is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "prompt must not be empty".to_owned(),
        });
    }
    if request.run_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "run id must not be empty".to_owned(),
        });
    }
    if !request.image_attachments.is_empty()
        && (request.agent_binding.is_some() || queued_first_request.is_some())
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "image attachments require a foreground conversation or inline skill"
                .to_owned(),
        });
    }
    use sigil_kernel::run_diagnostics::RunTimingPhase;
    let _preparation_timer =
        PreparationPhaseTimer::new(&request.run_id, RunTimingPhase::Preparation);
    let local_preparation_timer =
        PreparationPhaseTimer::new(&request.run_id, RunTimingPhase::SessionPreparation);
    let conversation_start =
        ConversationRunStartedEntryV1::new(request.run_id.clone(), current_unix_time_ms())
            .map_err(|error| ApplicationRunPrepareError::InvalidInvocation {
                message: safe_persistence_text(&error.to_string()),
            })?;
    let session_leases = Arc::clone(&services.session_leases);
    let task_executor_attached = services.task_role_provider_builder.is_some();
    let tool_authority = current_schema_tool_authority(services);
    let expected_composition = current_schema_boot_composition(services);
    let managed_session_log_writer = current_schema_managed_session_log_writer(services);
    let managed_artifact_store_writer = current_schema_managed_artifact_store_writer(services);
    let managed_plan_review_child_resources = services
        .authority_composition()
        .map(|composition| composition.plan_review_child_resource_provisioner());
    let process_environments =
        crate::application_mcp::environments(&request.additional_mcp_servers)
            .map_err(ApplicationRunPrepareError::configuration)?;
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_application_run_blocking_with_writer(
            request,
            session_leases,
            task_executor_attached,
            tool_authority,
            expected_composition,
            managed_session_log_writer,
            managed_artifact_store_writer,
            managed_plan_review_child_resources,
        )
    })
    .await
    .map_err(|error| ApplicationRunPrepareError::Internal {
        source: anyhow!(error).context("application run blocking preparation task failed"),
    })??;
    drop(local_preparation_timer);
    let BlockingApplicationRunPreparation {
        mut root_config,
        workspace_root,
        session_path,
        session_lease,
        mutation_recorder,
        mut session,
        workspace_trust,
        cancellation_recorder,
        cancellation_owner,
        cancellation_handle,
        root_task_guard,
        model_ref,
        mut options,
        mut target_max_tokens,
        mut input,
        run_id,
        prompt,
        interaction,
        redactor,
        tool_scope,
        skill_descriptor,
        agent_invocation,
        task_agent_registry,
        generate_session_title,
        route_transition,
        managed_session_log,
        managed_artifact_store,
    } = prepared;
    let agent_background_runs = session_lease
        .attachment
        .agent_tool_background_runs()
        .map_err(ApplicationRunPrepareError::execution)?;
    let selected_composition =
        sigil_kernel::SessionCompositionSnapshotV1::new(root_config.selected_capabilities());
    crate::session_composition::validate_session_composition_snapshot(
        &session,
        &selected_composition,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let provider_timer = PreparationPhaseTimer::new(&run_id, RunTimingPhase::ProviderConstruction);
    let provider = crate::build_provider_for_model_ref_async(&root_config, &model_ref)
        .await
        .map_err(ApplicationRunPrepareError::provider_unavailable)?;
    drop(provider_timer);
    if !input.persisted_image_attachments.is_empty()
        && !provider
            .image_input_capability(session.model_name())
            .is_supported()
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "selected model does not support image input; choose an image-capable model"
                .to_owned(),
        });
    }
    if target_max_tokens.is_none() {
        let effective_context_window = crate::resolve_model_context_window_tokens(
            &root_config,
            &model_ref,
            session.provider_name(),
        )
        .tokens;
        target_max_tokens = crate::resolve_automatic_output_token_budget(
            effective_context_window,
            provider.default_max_output_tokens(session.model_name()),
        )
        .map_err(|source| ApplicationRunPrepareError::Configuration { source })?;
        if let Some(max_output_tokens) = target_max_tokens {
            input = input.with_max_output_tokens(max_output_tokens);
        }
    }
    crate::validate_provider_output_token_budget(
        provider.as_ref(),
        session.model_name(),
        target_max_tokens,
    )
    .map_err(|source| ApplicationRunPrepareError::Configuration { source })?;
    let orchestration_route_guard = root_config.task.enabled.then(|| {
        crate::OrchestrationRouteGuard::new(
            session.provider_name(),
            session.model_name(),
            crate::ORCHESTRATION_RUNTIME_BUILD_ID,
        )
    });
    if let Some(guard) = orchestration_route_guard.as_ref() {
        if queued_first_request.is_none() {
            guard
                .enforce(&mut session, current_unix_time_ms())
                .map_err(ApplicationRunPrepareError::execution)?;
        }
        guard.apply_effective_task_config(&session, &mut root_config.task);
    }
    let session_id = session.session_scope_id().to_owned();
    // Construct the only public-event sequence before terminal tools retain the lifecycle sink.
    // The sink may publish while the foreground execution is active or after it finalizes.
    let events = ApplicationRunEventSequence::with_outbox(
        session_id.clone(),
        run_id.clone(),
        JsonlSessionStore::new(&session_path).map_err(ApplicationRunPrepareError::execution)?,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let terminal_lifecycle_sink = application_terminal_lifecycle_sink(
        mutation_recorder.clone(),
        services.terminal_lifecycle_handler.clone(),
        events.clone(),
    );
    let agent_delegation_available = root_config.task.enabled
        && root_config.task.multi_agent_mode != sigil_kernel::MultiAgentMode::None
        && task_agent_registry.is_some()
        && services.task_role_provider_builder.is_some();
    let surface_timer = PreparationPhaseTimer::new(&run_id, RunTimingPhase::ToolSurface);
    let (surface, warnings) = assemble_application_tool_surface(
        &root_config,
        &provider.capabilities(),
        &workspace_root,
        mutation_recorder,
        workspace_trust,
        &options,
        &session,
        services,
        &redactor,
        skill_descriptor.as_ref(),
        agent_delegation_available,
        tool_scope.as_ref(),
        terminal_lifecycle_sink,
        process_environments,
    )
    .await
    .map_err(ApplicationRunPrepareError::execution)?;
    // Keep the already-created process owners until all fallible preparation is either
    // transferred into PreparedApplicationRun or explicitly joined on this error path.
    let mut cleanup_registry = surface.registry.clone();
    let assembled = async {
        drop(surface_timer);
        let context_timer = PreparationPhaseTimer::new(&run_id, RunTimingPhase::RequestContext);
        let context_prompt = queued_first_request
            .as_ref()
            .map_or(prompt.as_str(), |(exact_prompt, _)| {
                exact_prompt.expose_secret()
            });
        let runtime_context = surface
            .context_resolver
            .resolve(context_prompt)
            .await
            .unwrap_or_default();
        drop(context_timer);
        let pending_input_provider: Arc<dyn sigil_kernel::PendingConversationInputProvider> =
            Arc::new(crate::pending_input::DurableQueuePendingInputProvider::new(
                surface.context_resolver.clone(),
            ));
        input = input
            .with_runtime_context(runtime_context.clone())
            .with_pending_input_provider(Arc::clone(&pending_input_provider));
        let terminal_control = ApplicationTerminalTaskControl::new(
            workspace_root.clone(),
            surface.terminal_control.clone(),
            session_lease.as_ref(),
            session.session_scope_id(),
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        let registry = surface.registry;
        let writable_memory_available = options.memory_config.writable
            && registry
                .spec_for(sigil_kernel::REMEMBER_USER_PREFERENCE_TOOL_NAME)
                .is_some()
            && registry
                .spec_for(sigil_kernel::REMEMBER_PROJECT_FACT_TOOL_NAME)
                .is_some();
        // Tool scoping may remove one or both durable-memory tools after configuration was loaded.
        // Every frozen/live request must consume the same effective capability as the final registry
        // so the system prompt never advertises an unavailable write path.
        options.memory_config.writable = writable_memory_available;
        let parent_session_ref = SessionRef::new_relative(
            session_path
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("session.jsonl"),
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        let task_execution = if let Some((profile_registry, role_provider_builder)) =
            task_agent_registry.zip(services.task_role_provider_builder.as_ref())
        {
            let agent_supervisor = crate::AgentSupervisor::new(
                profile_registry,
                crate::AgentBudgetPolicy::from_root_config(&root_config),
                provider.capabilities(),
            )
            .with_background_runs(agent_background_runs.clone());
            Some(ApplicationTaskExecutionRuntime {
                root_config: root_config.clone(),
                parent_session_ref: parent_session_ref.clone(),
                options: options.clone(),
                base_registry: registry.clone(),
                agent_supervisor,
                role_provider_builder: Arc::clone(role_provider_builder),
                verification_execution_port: services.authority_composition().map(|composition| {
                    crate::verification_with_mcp_settlement(
                        Arc::clone(&composition.command_execution)
                            as Arc<dyn sigil_kernel::verification::VerificationExecutionPortV1>,
                        registry.clone(),
                        agent_background_runs.clone(),
                    )
                }),
            })
        } else {
            None
        };
        let conversation_coordinator = orchestration_route_guard.map(|guard| {
            crate::ConversationCoordinator::new(
                root_config.task.enabled,
                root_config.task.routing_policy,
            )
            .with_writable_memory_routing(writable_memory_available)
            .with_orchestration_route_guard(guard)
            .with_route_capability_evidence(crate::RouteCapabilityEvidence {
                provider_supports_routing_tools: provider.capabilities().supports_tool_stream,
                // DirectTask additionally requires an attached task executor; without one the route
                // stays at the ReviewFirst baseline so plan review remains usable.
                task_executor_available: task_execution.is_some(),
            })
        });
        if queued_first_request.is_none()
            && agent_invocation.is_none()
            && let Some(coordinator) = conversation_coordinator.as_ref()
        {
            coordinator
                .enforce_orchestration_route_kill_switch(&mut session, current_unix_time_ms())
                .map_err(ApplicationRunPrepareError::execution)?;
            input = coordinator
                .bind_conversation_input(
                    &session,
                    input,
                    parent_session_ref.clone(),
                    run_id.clone(),
                    None,
                    current_unix_time_ms(),
                )
                .map_err(ApplicationRunPrepareError::execution)?;
        }
        let queued_first_assembly =
            if let Some((exact_prompt, durable_user_message_id)) = queued_first_request.as_ref() {
                let mut exact_user_message = ModelMessage::user(exact_prompt.expose_secret());
                exact_user_message.id = durable_user_message_id.clone();
                let routing = conversation_coordinator.as_ref().and_then(|coordinator| {
                    let capability = coordinator.resolve_route_capability(&session);
                    capability
                        .routes_automatically()
                        .then_some((coordinator, capability))
                });
                let tool_specs = routing.map_or_else(
                    || registry.specs(),
                    |(coordinator, capability)| {
                        coordinator.conversation_tool_specs_for_session(
                            &session,
                            capability,
                            registry.specs(),
                        )
                    },
                );
                let mut transient_messages = vec![exact_user_message];
                if let Some(contract) = routing.and_then(|(coordinator, capability)| {
                    coordinator.conversation_contract_for_session(&session, capability)
                }) {
                    transient_messages.insert(0, ModelMessage::system(contract));
                }
                let request = session
                    .build_pre_turn_candidate_request(
                        &workspace_root,
                        &options.memory_config,
                        tool_specs,
                        target_max_tokens,
                        options.reasoning_effort.clone(),
                        session.latest_response_handle(provider.name()),
                        options.traffic_partition_key.clone(),
                        &transient_messages,
                        runtime_context.clone(),
                        &[],
                    )
                    .map_err(ApplicationRunPrepareError::execution)?;
                let frozen_request =
                    FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request)
                        .map_err(ApplicationRunPrepareError::execution)?;
                let mut run_input = AgentRunInput::without_persisted_user_message(Vec::new())
                    .with_runtime_context(runtime_context)
                    .with_logical_run_id(run_id.clone())
                    .with_cancellation(cancellation_handle.clone())
                    .with_initial_frozen_provider_request(frozen_request.clone())
                    .with_pending_input_provider(Arc::clone(&pending_input_provider));
                if let Some(max_output_tokens) = target_max_tokens {
                    run_input = run_input.with_max_output_tokens(max_output_tokens);
                }
                Some(ApplicationExactFirstRequestAssembly {
                    frozen_request,
                    run_input,
                })
            } else {
                None
            };
        let explicit_plan_review = agent_invocation
            .as_ref()
            .is_some_and(|(_, profile_id)| profile_id.as_str() == "plan");
        let plan_review_selected = explicit_plan_review
            || (agent_invocation.is_none()
                && conversation_coordinator
                    .as_ref()
                    .is_some_and(|coordinator| {
                        coordinator
                            .resolve_route_capability(&session)
                            .routes_automatically()
                    }));
        let plan_review_workspace_snapshot_id = if plan_review_selected {
            crate::plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
                .map_err(ApplicationRunPrepareError::execution)?
        } else {
            None
        };
        let explicit_plan_review_request = explicit_plan_review
            .then(|| {
                crate::PlanReviewCoordinator::prepare_explicit_plan_review(
                    &mut session,
                    &prompt,
                    &run_id,
                    plan_review_workspace_snapshot_id.clone(),
                    current_unix_time_ms(),
                )
            })
            .transpose()
            .map_err(ApplicationRunPrepareError::execution)?;
        let pending_session_title = (queued_first_request.is_none() && generate_session_title)
            .then(|| ApplicationSessionTitleRequest {
                root_config: root_config.clone(),
                workspace_root: workspace_root.clone(),
                model_ref: model_ref.clone(),
                session_log_path: session_path.clone(),
                session_id: session_id.clone(),
                prompt: prompt.clone(),
                managed_writer: services
                    .authority_composition()
                    .map(|composition| std::sync::Arc::clone(&composition.storage_writer)),
            });
        let conversation_lifecycle = session
            .conversation_run_lifecycle_recorder()
            .map_err(ApplicationRunPrepareError::execution)?;
        let kind = if let Some(request) = explicit_plan_review_request {
            ApplicationRunExecutionKind::ExplicitPlanReview {
                request: Box::new(request),
            }
        } else if let Some((registry_snapshot, profile_id)) = agent_invocation {
            let supervisor = crate::AgentSupervisor::new(
                registry_snapshot,
                crate::AgentBudgetPolicy::from_root_config(&root_config),
                provider.capabilities().clone(),
            )
            .with_background_runs(agent_background_runs.clone());
            let mut runtime =
                crate::AgentToolRuntime::new(supervisor, root_config.clone(), registry.clone());
            sigil_kernel::AgentToolDelegate::set_run_cancellation(
                &mut runtime,
                Some(cancellation_handle.clone()),
            );
            sigil_kernel::AgentToolDelegate::set_root_logical_run_id(&mut runtime, Some(&run_id));
            ApplicationRunExecutionKind::AgentProfile {
                runtime: Box::new(runtime),
                profile_id,
            }
        } else {
            let agent_tool_runtime = task_execution.as_ref().and_then(|task_execution| {
                (root_config.task.multi_agent_mode != sigil_kernel::MultiAgentMode::None).then(
                    || {
                        let mut runtime = crate::AgentToolRuntime::new(
                            task_execution.agent_supervisor.clone(),
                            root_config.clone(),
                            registry.clone(),
                        );
                        sigil_kernel::AgentToolDelegate::set_run_cancellation(
                            &mut runtime,
                            Some(cancellation_handle.clone()),
                        );
                        Box::new(runtime)
                    },
                )
            });
            ApplicationRunExecutionKind::Main {
                agent: Box::new(
                    crate::configured_agent(&root_config, provider, registry.clone())
                        .map_err(ApplicationRunPrepareError::execution)?,
                ),
                input: Box::new(input),
                agent_tool_runtime,
            }
        };
        crate::session_composition::bind_session_composition_snapshot(
            &mut session,
            selected_composition,
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        let prepared = PreparedApplicationRun {
            execution: ApplicationRunExecution {
                extension_registry: registry.clone(),
                extension_background_runs: agent_background_runs.clone(),
                plan_review_runtime: if plan_review_selected {
                    let tool_registry =
                        crate::build_plan_review_tool_registry(&registry, &root_config)
                            .into_registry();
                    Some(ApplicationPlanReviewRuntime {
                        options: options.clone(),
                        workspace_snapshot_id: plan_review_workspace_snapshot_id,
                        agent: Box::new(
                            crate::configured_agent(
                                &root_config,
                                crate::build_provider_for_model_ref_async(&root_config, &model_ref)
                                    .await
                                    .map_err(ApplicationRunPrepareError::provider_unavailable)?,
                                tool_registry.clone(),
                            )
                            .map_err(ApplicationRunPrepareError::execution)?,
                        ),
                        tool_registry,
                        child_resource_provisioner: services.authority_composition().map(
                            |composition| composition.plan_review_child_resource_provisioner(),
                        ),
                    })
                } else {
                    None
                },
                kind,
                task_execution,
                session,
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
                conversation_lifecycle: conversation_lifecycle.clone(),
                conversation_start: conversation_start.clone(),
                events: events.clone(),
                conversation_coordinator,
                parent_session_ref,
                pending_session_title,
                pending_user_input_continuation: None,
                route_transition,
                managed_session_log,
                managed_artifact_store,
                _session_lease: Arc::clone(&session_lease),
            },
            control: ApplicationRunControl {
                owner: cancellation_owner,
                recorder: cancellation_recorder,
                cancellation_target: RunCancellationTarget::Run,
                conversation_lifecycle,
                conversation_start,
                events,
                _session_lease: session_lease,
            },
            terminal_control,
        };
        Ok((prepared, queued_first_assembly))
    }
    .await;
    if let Err(error) = assembled {
        let preparation_cleanup = cleanup_registry
            .quiesce_background_work(&sigil_kernel::ToolBackgroundWorkSettlement::CancelIdle)
            .await;
        let generation_cleanup = crate::shutdown_mcp_generations(&mut cleanup_registry).await;
        let failures = [preparation_cleanup, generation_cleanup]
            .into_iter()
            .filter_map(Result::err)
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            return Err(ApplicationRunPrepareError::execution(anyhow!(
                "{error}; extension cleanup incomplete: {}",
                failures.join("; ")
            )));
        }
        return Err(error);
    }
    assembled
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
/// Returns a typed preparation error when the model identifier is invalid, the selected
/// connection is unavailable, or configuration/session recovery fails.
pub fn bind_application_session_with_model(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
    model_name: Option<&str>,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    bind_application_session_with_model_ref(config_path, launch_cwd, session_path, None, model_name)
}

/// Creates or reopens a durable V2 session using an exact optional connection/model identity.
pub fn bind_application_session_with_model_ref(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
    connection_id: Option<&ConnectionId>,
    model_name: Option<&str>,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    bind_application_session_with_model_ref_and_attachment(
        config_path,
        launch_cwd,
        session_path,
        connection_id,
        model_name,
    )
    .map(|(binding, _attachment)| binding)
}

/// Creates or reopens a durable session while retaining the exact cross-process attachment used
/// for identity initialization and automatic route recovery.
pub fn bind_application_session_with_model_ref_and_attachment(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
    connection_id: Option<&ConnectionId>,
    model_name: Option<&str>,
) -> std::result::Result<
    (
        ApplicationSessionBinding,
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    ),
    ApplicationRunPrepareError,
> {
    bind_application_session_with_model_ref_and_attachment_and_managed_writer(
        config_path,
        launch_cwd,
        session_path,
        connection_id,
        model_name,
        None,
    )
}

/// Creates or reopens a durable session using the authority-declared session-log leaf when the
/// boot composition provides a managed writer. The writer lease is held across session binding
/// and finalized before the attachment is returned; foreground runs acquire their own lease.
pub fn bind_application_session_with_model_ref_and_attachment_and_managed_writer(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
    connection_id: Option<&ConnectionId>,
    model_name: Option<&str>,
    managed_session_log_writer: Option<
        Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
) -> std::result::Result<
    (
        ApplicationSessionBinding,
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    ),
    ApplicationRunPrepareError,
> {
    bind_application_session_with_model_ref_and_projection_owner(
        config_path,
        launch_cwd,
        session_path,
        connection_id,
        model_name,
        managed_session_log_writer,
    )
    .map(|(binding, attachment, _owner)| (binding, attachment))
}

/// Binds a session and transfers projection capabilities from the same admitted store.
pub fn bind_application_session_with_model_ref_and_projection_owner(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: Option<&Path>,
    connection_id: Option<&ConnectionId>,
    model_name: Option<&str>,
    managed_session_log_writer: Option<
        Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
) -> std::result::Result<
    (
        ApplicationSessionBinding,
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        crate::RuntimeSessionProjectionOwner,
    ),
    ApplicationRunPrepareError,
> {
    let root_config = load_application_root_config(config_path)?;
    let (_, selected_route) =
        application_selected_model_route(&root_config, connection_id, model_name)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let requested_path = session_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_application_session_path(&sigil_paths.session_log_dir));
    let (requested_path, managed_session_log) = if let Some(writer) = managed_session_log_writer {
        let key = application_session_log_key(&writer, &requested_path)
            .map_err(ApplicationRunPrepareError::execution)?;
        let managed_path = writer
            .session_log_path_for_key(&key)
            .map_err(ApplicationRunPrepareError::execution)?
            .join("records.jsonl");
        let lease = ManagedApplicationSessionLogLease::acquire(writer, &key)
            .map_err(ApplicationRunPrepareError::execution)?;
        (managed_path, Some(lease))
    } else {
        (requested_path, None)
    };
    let canonical_path = canonical_session_lease_path(&requested_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let attachment = Arc::new(
        crate::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &canonical_path,
        )
        .map_err(|error| match error {
            crate::interactive_session_attachment::InteractiveSessionAttachmentError::Busy {
                observed_generation,
            } => ApplicationRunPrepareError::SessionAlreadyActive {
                recovery_binding:
                    crate::interactive_session_attachment::session_attachment_path_recovery_binding(
                        &canonical_path,
                        &observed_generation,
                    ),
            },
            error => ApplicationRunPrepareError::execution(error),
        })?,
    );
    let store =
        JsonlSessionStore::new(&canonical_path).map_err(ApplicationRunPrepareError::execution)?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        &root_config,
        &selected_route,
        store.clone(),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    crate::validate_session_composition(&inspected.session, &root_config)
        .map_err(ApplicationRunPrepareError::execution)?;
    let mut outcome = crate::provider_connections::load_session_for_route_transition(
        &root_config,
        &selected_route,
        store.clone(),
        None,
        None,
        Some(attachment.as_ref()),
    )
    .map_err(application_route_load_prepare_error)?;
    crate::bind_session_composition(&mut outcome.session, &root_config)
        .map_err(ApplicationRunPrepareError::execution)?;
    if let Some(managed_session_log) = managed_session_log {
        managed_session_log
            .finalize()
            .map_err(ApplicationRunPrepareError::execution)?;
    }
    attachment
        .bind_application_operation_owner(&outcome.session)
        .map_err(ApplicationRunPrepareError::execution)?;
    Ok((
        ApplicationSessionBinding {
            session_scope_id: outcome.session.session_scope_id().to_owned(),
            session_log_path: canonical_path,
            route_transition: outcome.transition,
        },
        attachment,
        crate::RuntimeSessionProjectionOwner::from_store(&store),
    ))
}

fn application_model_catalog_entries(
    root_config: &RootConfig,
    current_model: &ModelRef,
    cache_root: &Path,
) -> Vec<crate::provider_connections::ModelCatalogEntry> {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    let mut entries = Vec::new();
    for connection in loaded.connections.values() {
        let cached = crate::provider_connections::fresh_cached_model_entries_native(
            cache_root,
            root_config,
            &connection.config.id,
        );
        let mut connection_entries = cached.unwrap_or_else(|| {
            crate::provider_connections::bundled_model_entries(&connection.config)
        });
        for required in [Some(current_model), loaded.default_model.as_ref()]
            .into_iter()
            .flatten()
            .filter(|model_ref| model_ref.connection_id == connection.config.id)
        {
            if !connection_entries
                .iter()
                .any(|entry| entry.model_ref == *required)
            {
                connection_entries.push(crate::provider_connections::ModelCatalogEntry {
                    model_ref: required.clone(),
                    display_name: required.model_id.clone(),
                    availability: crate::provider_connections::ModelAvailability::Unverified,
                    recommendation: crate::provider_connections::ModelRecommendation::Standard,
                    provenance: crate::provider_connections::ModelCatalogProvenance::Configured,
                });
            }
        }
        entries.extend(connection_entries);
    }
    entries.sort_by(|left, right| {
        left.model_ref
            .connection_id
            .cmp(&right.model_ref.connection_id)
            .then_with(|| left.model_ref.model_id.cmp(&right.model_ref.model_id))
    });
    entries.dedup_by(|left, right| left.model_ref == right.model_ref);
    entries.sort_by(|left, right| {
        let left_current = left.model_ref != *current_model;
        let right_current = right.model_ref != *current_model;
        left_current
            .cmp(&right_current)
            .then_with(|| {
                left.model_ref
                    .connection_id
                    .cmp(&right.model_ref.connection_id)
            })
            .then_with(|| {
                let left_standard = left.recommendation
                    != crate::provider_connections::ModelRecommendation::Recommended;
                let right_standard = right.recommendation
                    != crate::provider_connections::ModelRecommendation::Recommended;
                left_standard.cmp(&right_standard)
            })
            .then_with(|| left.model_ref.model_id.cmp(&right.model_ref.model_id))
    });
    entries
}

// Bind the source selection, not the changing catalog, cache provenance, or recommendations.
// The requested destination is resolved against live connection configuration at admission.
fn application_model_selection_binding(current_model: &ModelRef) -> String {
    let material = format!(
        "sigil-application-model-selection-v4\n{}/{}\n",
        current_model.connection_id, current_model.model_id,
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

fn application_model_option_views(
    root_config: &RootConfig,
    catalog_entries: Vec<crate::provider_connections::ModelCatalogEntry>,
) -> Vec<ApplicationModelOptionView> {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    catalog_entries
        .into_iter()
        .filter_map(|entry| {
            let connection = loaded.connections.get(&entry.model_ref.connection_id)?;
            let provider_name =
                crate::provider_connections::runtime_provider_name(&connection.config);
            let model_name = entry.model_ref.model_id.clone();
            let mut model_config = root_config.clone();
            model_config.agent.runtime_provider = provider_name.to_owned();
            model_config.agent.model = model_name.clone();
            let available_reasoning_efforts =
                crate::reasoning_effort::supported_reasoning_efforts(provider_name, &model_name);
            let default_reasoning_effort =
                crate::reasoning_effort::configured_default_reasoning_effort(&model_config);
            let reasoning_effort_binding = crate::reasoning_effort::reasoning_effort_binding(
                provider_name,
                &model_name,
                &available_reasoning_efforts,
            );
            Some(ApplicationModelOptionView {
                model_ref: entry.model_ref,
                display_name: entry.display_name,
                availability: entry.availability,
                recommendation: entry.recommendation,
                provenance: entry.provenance,
                model_name,
                available_reasoning_efforts,
                default_reasoning_effort,
                reasoning_effort_binding,
            })
        })
        .collect()
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
    let _root_config = load_application_root_config(config_path)?;
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
    let records = sigil_kernel::SessionRecordReadHandle::open_existing_observer(&canonical_path)
        .and_then(|reader| reader.read_event_records())
        .map_err(ApplicationRunPrepareError::execution)?;
    let session_scope_id = records
        .first()
        .map(|record| record.session_id().to_owned())
        .ok_or_else(|| {
            ApplicationRunPrepareError::execution(anyhow!(
                "existing application session has no durable identity"
            ))
        })?;
    if records
        .iter()
        .any(|record| record.session_id() != session_scope_id)
    {
        return Err(ApplicationRunPrepareError::execution(anyhow!(
            "existing application session has mixed durable identity"
        )));
    }
    let entries = records
        .iter()
        .map(sigil_kernel::conversation_transcript_entry_from_record)
        .collect::<Result<Vec<_>>>()
        .map_err(ApplicationRunPrepareError::execution)?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let route = application_session_route(&entries).ok_or_else(|| {
        ApplicationRunPrepareError::execution(anyhow!(
            "existing application session has no resolved route"
        ))
    })?;
    Ok(ApplicationSessionBinding {
        session_scope_id,
        session_log_path: canonical_path,
        route_transition: crate::provider_connections::SessionRouteTransitionView {
            kind: crate::provider_connections::SessionRouteTransitionKind::Exact,
            connection_id: Some(route.model_ref.connection_id.as_str().to_owned()),
            model_id: Some(route.model_ref.model_id),
            remote_context_reset: false,
        },
    })
}

/// Reopens and activates an existing durable session under the exact controller attachment.
///
/// The returned receipt records automatic same-trust rebinds. Recovery decisions that require
/// user confirmation remain typed preparation errors so callers can keep a read-only handle.
pub fn bind_existing_application_session_with_attachment(
    config_path: &Path,
    session_path: &Path,
    attachment: &crate::interactive_session_attachment::InteractiveSessionAttachmentLease,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    bind_existing_application_session_with_attachment_and_projection_owner(
        config_path,
        session_path,
        attachment,
    )
    .map(|(binding, _owner)| binding)
}

/// Reopens an attached session and transfers projection capabilities from its actual owner.
pub fn bind_existing_application_session_with_attachment_and_projection_owner(
    config_path: &Path,
    session_path: &Path,
    attachment: &crate::interactive_session_attachment::InteractiveSessionAttachmentLease,
) -> std::result::Result<
    (
        ApplicationSessionBinding,
        crate::RuntimeSessionProjectionOwner,
    ),
    ApplicationRunPrepareError,
> {
    let read_binding = bind_existing_application_session(config_path, session_path)?;
    let root_config = load_application_root_config(config_path)?;
    let persisted_connection_id = read_binding
        .route_transition
        .connection_id
        .as_deref()
        .ok_or(ApplicationRunPrepareError::SessionStreamInvalid)
        .and_then(|value| {
            ConnectionId::new(value.to_owned())
                .map_err(|_| ApplicationRunPrepareError::SessionStreamInvalid)
        })?;
    let persisted_model_id = read_binding
        .route_transition
        .model_id
        .as_deref()
        .ok_or(ApplicationRunPrepareError::SessionStreamInvalid)?;
    let (_, fallback_route) = application_selected_model_route(
        &root_config,
        Some(&persisted_connection_id),
        Some(persisted_model_id),
    )?;
    let store = JsonlSessionStore::new(&read_binding.session_log_path)
        .map_err(ApplicationRunPrepareError::execution)?
        .with_live_background_agent_threads(
            attachment
                .agent_tool_background_runs()
                .and_then(|runs| runs.thread_ids())
                .map_err(ApplicationRunPrepareError::execution)?,
        );
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        &root_config,
        &fallback_route,
        store.clone(),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    crate::validate_session_composition(&inspected.session, &root_config)
        .map_err(ApplicationRunPrepareError::execution)?;
    let mut outcome = crate::provider_connections::load_session_for_route_transition(
        &root_config,
        &fallback_route,
        store.clone(),
        None,
        None,
        Some(attachment),
    )
    .map_err(application_route_load_prepare_error)?;
    crate::bind_session_composition(&mut outcome.session, &root_config)
        .map_err(ApplicationRunPrepareError::execution)?;
    attachment
        .bind_application_operation_owner(&outcome.session)
        .map_err(ApplicationRunPrepareError::execution)?;
    Ok((
        ApplicationSessionBinding {
            session_scope_id: outcome.session.session_scope_id().to_owned(),
            session_log_path: read_binding.session_log_path,
            route_transition: outcome.transition,
        },
        crate::RuntimeSessionProjectionOwner::from_store(&store),
    ))
}

fn application_route_load_prepare_error(
    error: crate::provider_connections::SessionRouteLoadError,
) -> ApplicationRunPrepareError {
    match error {
        crate::provider_connections::SessionRouteLoadError::ConfirmationRequired {
            recovery_binding,
            ..
        } => ApplicationRunPrepareError::SessionRouteConfirmationRequired { recovery_binding },
        crate::provider_connections::SessionRouteLoadError::SelectionRequired {
            reason: crate::provider_connections::SessionRouteUnavailableReason::ConnectionNotFound,
            recovery_binding,
            ..
        } => ApplicationRunPrepareError::SessionRouteSelectionRequired { recovery_binding },
        crate::provider_connections::SessionRouteLoadError::SelectionRequired {
            reason:
                crate::provider_connections::SessionRouteUnavailableReason::ConnectionConfigInvalid,
            ..
        }
        | crate::provider_connections::SessionRouteLoadError::SetupRequired {
            reason: crate::provider_connections::ModelRouteSetupReason::ConfigurationInvalid,
            ..
        } => ApplicationRunPrepareError::connection_config_invalid(anyhow!(
            "connection_config_invalid"
        )),
        crate::provider_connections::SessionRouteLoadError::SetupRequired {
            reason: crate::provider_connections::ModelRouteSetupReason::RouteNotConfigured,
            ..
        } => ApplicationRunPrepareError::ModelRouteNotConfigured,
        crate::provider_connections::SessionRouteLoadError::WriterBusy { recovery_binding } => {
            ApplicationRunPrepareError::SessionWriterBusy { recovery_binding }
        }
        crate::provider_connections::SessionRouteLoadError::Unavailable(_) => {
            ApplicationRunPrepareError::SessionStreamInvalid
        }
    }
}

fn load_application_root_config(
    config_path: &Path,
) -> std::result::Result<RootConfig, ApplicationRunPrepareError> {
    match std::fs::symlink_metadata(config_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApplicationRunPrepareError::ModelRouteNotConfigured);
        }
        Err(error) => {
            return Err(ApplicationRunPrepareError::connection_config_invalid(error));
        }
        Ok(_) => {}
    }
    RootConfig::load(config_path)
        .and_then(|config| config.with_effective_composition())
        .map_err(ApplicationRunPrepareError::connection_config_invalid)
}

fn application_selected_model_route(
    root_config: &RootConfig,
    connection_id: Option<&ConnectionId>,
    model_name: Option<&str>,
) -> std::result::Result<(String, ResolvedModelRoute), ApplicationRunPrepareError> {
    if let (Some(connection_id), Some(model_name)) = (connection_id, model_name) {
        let model_ref = ModelRef::new(connection_id.clone(), model_name).map_err(|error| {
            ApplicationRunPrepareError::InvalidInvocation {
                message: error.to_string(),
            }
        })?;
        return crate::provider_connections::resolve_model_route(root_config, &model_ref).map_err(
            |error| match error {
                crate::provider_connections::ResolvedRouteError::NotConfigured => {
                    ApplicationRunPrepareError::ModelRouteNotConfigured
                }
                other => ApplicationRunPrepareError::connection_config_invalid(other),
            },
        );
    }
    if connection_id.is_some() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "connection and model must be supplied together".to_owned(),
        });
    }
    let (provider_name, default_route) =
        crate::provider_connections::resolve_default_model_route(root_config).map_err(|error| {
            match error {
                crate::provider_connections::ResolvedRouteError::NotConfigured => {
                    ApplicationRunPrepareError::ModelRouteNotConfigured
                }
                other => ApplicationRunPrepareError::connection_config_invalid(other),
            }
        })?;
    let Some(model_name) = model_name else {
        return Ok((provider_name, default_route));
    };
    let requested = {
        let trimmed = model_name.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
    .ok_or_else(|| ApplicationRunPrepareError::InvalidInvocation {
        message: "application session model must not be empty".to_owned(),
    })?;
    let model_ref = ModelRef::new(default_route.model_ref.connection_id.clone(), requested)
        .map_err(|error| ApplicationRunPrepareError::InvalidInvocation {
            message: error.to_string(),
        })?;
    crate::provider_connections::resolve_model_route(root_config, &model_ref)
        .map_err(ApplicationRunPrepareError::connection_config_invalid)
}

pub(crate) fn load_application_session_for_route_with_attachment(
    root_config: &RootConfig,
    fallback_route: &ResolvedModelRoute,
    store: JsonlSessionStore,
    attachment: Option<&crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> Result<Session> {
    crate::provider_connections::load_session_for_route(
        root_config,
        fallback_route,
        store,
        None,
        None,
        attachment,
    )
    .map_err(anyhow::Error::new)
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
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<ApplicationRunContextView> {
    let records = application_bound_session_records(session_path, expected_session_scope_id)?;
    application_run_context_view_from_records(
        config_path,
        launch_cwd,
        &records,
        expected_session_scope_id,
    )
}

pub(crate) fn application_run_context_view_from_records(
    config_path: &Path,
    launch_cwd: &Path,
    records: &[sigil_kernel::SessionStreamRecord],
    expected_session_scope_id: &str,
) -> Result<ApplicationRunContextView> {
    if expected_session_scope_id.is_empty() {
        bail!("expected run-context session scope must not be empty");
    }
    let root_config = RootConfig::load(config_path)?.with_effective_composition()?;
    let entries =
        application_bound_session_entries_from_records(records, expected_session_scope_id)?;
    let ApplicationRunStartView {
        model_ref,
        provider_name,
        model_selection_binding,
        default_permission_mode,
        available_reasoning_efforts,
        default_reasoning_effort,
        reasoning_effort_binding,
        route_recovery,
        source_model_ref,
        requested_model_available: _,
    } = run_start::resolve_run_start_view(&root_config, &entries, expected_session_scope_id, None)?;
    let model_name = model_ref.model_id.clone();
    let resolved =
        crate::resolve_model_context_window_tokens(&root_config, &model_ref, &provider_name);
    let mut usage_stats = sigil_kernel::SessionStats::default();
    let mut observed_ordinary_usage = false;
    for entry in &entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) => {
                observed_ordinary_usage = true;
                usage_stats.apply_usage(usage);
            }
            SessionLogEntry::Control(ControlEntry::SemanticCompactionUsageSnapshot(usage)) => {
                usage_stats.apply_semantic_compaction_usage(usage);
            }
            _ => {}
        }
    }
    let last_prompt_tokens = observed_ordinary_usage.then_some(usage_stats.last_prompt_tokens);
    let cache_usage = observed_ordinary_usage.then(|| ApplicationCacheUsageView {
        cache_read_tokens: usage_stats.cache_hit_tokens,
        cache_miss_tokens: usage_stats.cache_miss_tokens,
        cache_write_tokens: usage_stats
            .cache_write_observed
            .then_some(usage_stats.cache_write_tokens),
        last_layout_mutation: usage_stats.last_cache_layout_mutation,
        provider_miss_without_local_mutation: usage_stats.last_provider_miss_without_local_mutation,
    });
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let extension_catalog =
        crate::application_extension_catalog_view(&root_config, &workspace_root, &entries)?;
    let catalog_entries =
        application_model_catalog_entries(&root_config, &source_model_ref, &sigil_paths.cache_root);
    let model_options = application_model_option_views(&root_config, catalog_entries);
    Ok(ApplicationRunContextView {
        model_ref,
        provider_name,
        model_name,
        model_options,
        model_selection_binding,
        default_permission_mode,
        available_reasoning_efforts,
        default_reasoning_effort,
        reasoning_effort_binding,
        context_window_tokens: resolved.tokens,
        last_prompt_tokens,
        cache_usage,
        context_window_source: resolved.source,
        extension_catalog,
        route_recovery,
    })
}

pub(crate) fn application_session_route_trust_binding(
    entries: &[SessionLogEntry],
) -> Option<sigil_kernel::RouteEgressTrustBinding> {
    let mut current_fingerprint = None::<String>;
    let mut binding = None;
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route,
                ..
            }) => {
                current_fingerprint = resolved_model_route
                    .as_ref()
                    .map(|route| route.semantic_fingerprint.clone());
                binding = None;
            }
            SessionLogEntry::Control(
                ControlEntry::SessionModelSelected {
                    resolved_model_route,
                    ..
                }
                | ControlEntry::SessionRouteRebound {
                    resolved_model_route,
                    ..
                },
            ) => {
                current_fingerprint = Some(resolved_model_route.semantic_fingerprint.clone());
                binding = None;
            }
            SessionLogEntry::Control(ControlEntry::SessionRouteTrustBound {
                route_semantic_fingerprint,
                egress_trust_binding,
            }) if current_fingerprint.as_deref() == Some(route_semantic_fingerprint.as_str()) => {
                binding = Some(egress_trust_binding.clone());
            }
            _ => {}
        }
    }
    binding
}

pub(crate) fn application_session_route(entries: &[SessionLogEntry]) -> Option<ResolvedModelRoute> {
    let mut route = None;
    let mut identity_seen = false;
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route,
                ..
            }) if !identity_seen => {
                identity_seen = true;
                route = resolved_model_route.clone();
            }
            SessionLogEntry::Control(ControlEntry::SessionModelSelected {
                resolved_model_route,
                ..
            })
            | SessionLogEntry::Control(ControlEntry::SessionRouteRebound {
                resolved_model_route,
                ..
            }) if identity_seen => route = Some(resolved_model_route.clone()),
            _ => {}
        }
    }
    route
}

/// Loads an existing session for local control decisions without resolving or changing a
/// provider route. Execution readiness belongs to the runner that actually uses the provider.
pub(crate) fn load_application_control_session(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Session> {
    let session = Session::load_from_store_for_control(JsonlSessionStore::new(session_path)?)?;
    if session.session_scope_id() != expected_session_scope_id {
        bail!("application control session identity changed");
    }
    Ok(session)
}

fn application_session_identity(entries: &[SessionLogEntry]) -> Option<(String, String)> {
    let mut identity = None;
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name,
                model_name,
                ..
            }) if identity.is_none() => {
                identity = Some((provider_name.clone(), model_name.clone()));
            }
            SessionLogEntry::Control(ControlEntry::SessionModelSelected {
                provider_name,
                model_name,
                ..
            })
            | SessionLogEntry::Control(ControlEntry::SessionRouteRebound {
                provider_name,
                model_name,
                ..
            }) if identity.is_some() => {
                if let Some((current_provider, current_model)) = identity.as_mut() {
                    *current_provider = provider_name.clone();
                    *current_model = model_name.clone();
                }
            }
            _ => {}
        }
    }
    identity
}

fn application_bound_session_entries(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Vec<SessionLogEntry>> {
    let records = application_bound_session_records(session_path, expected_session_scope_id)?;
    application_bound_session_entries_from_records(&records, expected_session_scope_id)
}

fn application_bound_session_entries_from_records(
    records: &[sigil_kernel::SessionStreamRecord],
    expected_session_scope_id: &str,
) -> Result<Vec<SessionLogEntry>> {
    validate_application_session_records(records, expected_session_scope_id)?;
    sigil_kernel::ConversationQueueDurableProjection::from_records(records)?;
    records
        .iter()
        .map(sigil_kernel::conversation_transcript_entry_from_record)
        .collect::<Result<Vec<_>>>()
        .context("failed to decode durable application session entry")
        .map(|entries| entries.into_iter().flatten().collect())
}

fn application_bound_session_records(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Vec<sigil_kernel::SessionStreamRecord>> {
    let records = sigil_kernel::SessionRecordReadHandle::open_existing_observer(session_path)?
        .read_event_records()?;
    validate_application_session_records(&records, expected_session_scope_id)?;
    Ok(records)
}

fn validate_application_session_records(
    records: &[sigil_kernel::SessionStreamRecord],
    expected_session_scope_id: &str,
) -> Result<()> {
    let actual_session_scope_id = records
        .first()
        .map(|record| record.session_id().to_owned())
        .ok_or_else(|| anyhow!("durable application session has no session identity"))?;
    if actual_session_scope_id != expected_session_scope_id
        || records
            .iter()
            .any(|record| record.session_id() != expected_session_scope_id)
    {
        bail!("durable application session scope does not match the bound session");
    }
    Ok(())
}

/// Reads the current append-only durable frontier for one bound application session.
///
/// This projection performs no writes and does not infer foreground ownership. The application
/// adapter combines it with its own process-local owner registry after the durable scope has been
/// revalidated.
///
/// # Errors
///
/// Returns an error when the expected scope is empty, the durable stream is empty or malformed,
/// or any record belongs to another session scope.
pub fn application_session_frontier_view(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<ApplicationSessionFrontierView> {
    if expected_session_scope_id.is_empty() {
        bail!("expected continuity session scope must not be empty");
    }
    let records = application_bound_session_records(session_path, expected_session_scope_id)?;
    let through_stream_sequence = records
        .last()
        .map(sigil_kernel::SessionStreamRecord::stream_sequence)
        .ok_or_else(|| anyhow!("durable application session has no frontier"))?;
    Ok(ApplicationSessionFrontierView {
        session_scope_id: expected_session_scope_id.to_owned(),
        through_stream_sequence,
    })
}

/// Reads one renderer-safe, bounded child-agent activity projection for a bound session.
///
/// # Errors
///
/// Returns an error when the durable scope differs from the adapter binding or the append-only
/// stream cannot be decoded safely.
pub fn application_agent_activity_view(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<ApplicationAgentActivityView> {
    if expected_session_scope_id.is_empty() {
        bail!("expected agent-activity session scope must not be empty");
    }
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    Ok(agent_activity_product_view_from_entries(&entries))
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
/// The projection deliberately excludes system and unrelated control data, tool arguments,
/// resolved image bytes and the source path. Durable `reasoning_trace` notes are admitted as
/// explicitly classified assistant rows so user-visible reasoning survives resume. `before` is an
/// exclusive one-based message ordinal so pagination remains stable while the append-only stream
/// grows.
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
    validate_application_transcript_page_request(expected_session_scope_id, before, limit)?;
    project_application_session_transcript(
        JsonlSessionStore::read_event_record_stream(session_path)?,
        expected_session_scope_id,
        before,
        limit,
    )
}

fn validate_application_transcript_page_request(
    expected_session_scope_id: &str,
    before: Option<u64>,
    limit: usize,
) -> Result<()> {
    if expected_session_scope_id.is_empty() {
        bail!("expected transcript session scope must not be empty");
    }
    if !(1..=MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE).contains(&limit) {
        bail!("transcript page size must be between 1 and {MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE}");
    }
    if before == Some(0) {
        bail!("transcript before ordinal must be positive");
    }

    Ok(())
}

fn project_application_session_transcript<
    R: std::borrow::Borrow<sigil_kernel::SessionStreamRecord>,
>(
    records: impl IntoIterator<Item = Result<R>>,
    expected_session_scope_id: &str,
    before: Option<u64>,
    limit: usize,
) -> Result<ApplicationTranscriptPage> {
    let mut tool_names = BTreeMap::new();
    let mut tool_name_order = VecDeque::new();
    const MAX_TOOL_NAMES: usize = 512;
    let mut messages = VecDeque::<ApplicationTranscriptMessage>::new();
    let mut message_bytes = 0_usize;
    let mut total_messages = 0_u64;
    let mut saw_record = false;
    for record in records {
        let record = record?;
        let record = record.borrow();
        saw_record = true;
        if record.session_id() != expected_session_scope_id {
            bail!("durable application session scope does not match the bound session");
        }
        let Some(entry) = sigil_kernel::conversation_transcript_entry_from_record(record)? else {
            continue;
        };
        total_messages = total_messages
            .checked_add(1)
            .ok_or_else(|| anyhow!("transcript message count exceeds supported range"))?;
        let Some(message) = project_application_transcript_entry(
            entry,
            total_messages,
            &mut tool_names,
            &mut tool_name_order,
            MAX_TOOL_NAMES,
        )?
        else {
            total_messages = total_messages.saturating_sub(1);
            continue;
        };
        if before.is_none_or(|boundary| message.ordinal < boundary) {
            let item_bytes = message.content.as_ref().map_or(0, String::len);
            if messages.len() == limit
                && let Some(removed) = messages.pop_front()
            {
                message_bytes =
                    message_bytes.saturating_sub(removed.content.as_ref().map_or(0, String::len));
            }
            message_bytes = message_bytes.saturating_add(item_bytes);
            messages.push_back(message);
            while messages.len() > 1 && message_bytes > MAX_APPLICATION_TRANSCRIPT_PAGE_BYTES {
                if let Some(removed) = messages.pop_front() {
                    message_bytes = message_bytes
                        .saturating_sub(removed.content.as_ref().map_or(0, String::len));
                }
            }
        }
    }
    if !saw_record {
        bail!("durable application session has no session identity");
    }

    let next_before = messages
        .front()
        .filter(|message| message.ordinal > 1)
        .map(|message| message.ordinal);

    Ok(ApplicationTranscriptPage {
        session_scope_id: expected_session_scope_id.to_owned(),
        total_messages,
        messages: messages.into_iter().collect(),
        next_before,
    })
}

fn application_transcript_reasoning_trace(control: &ControlEntry) -> Option<&str> {
    let ControlEntry::Note { kind, data } = control else {
        return None;
    };
    if kind != "reasoning_trace" {
        return None;
    }
    data.get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|trace| !trace.trim().is_empty())
}

pub(crate) fn project_application_transcript_entry(
    entry: SessionLogEntry,
    ordinal: u64,
    tool_names: &mut BTreeMap<String, String>,
    tool_name_order: &mut VecDeque<String>,
    max_tool_names: usize,
) -> Result<Option<ApplicationTranscriptMessage>> {
    let message = match entry {
        SessionLogEntry::Control(control) => {
            let Some(trace) = application_transcript_reasoning_trace(&control) else {
                return Ok(None);
            };
            let safe_content = safe_persistence_text(trace);
            let original_content_bytes = safe_content.len();
            return Ok(Some(ApplicationTranscriptMessage {
                ordinal,
                message_id: safe_application_transcript_message_id(&format!(
                    "reasoning-trace:{ordinal}"
                )),
                role: ApplicationTranscriptRole::Assistant,
                content: Some(truncate_application_transcript_text(
                    &safe_content,
                    MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES,
                )),
                assistant_kind: Some(AssistantMessageKind::ReasoningTrace),
                tool_name: None,
                image_attachment_count: 0,
                truncated: original_content_bytes > MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES,
                original_content_bytes: u64::try_from(original_content_bytes)
                    .map_err(|_| anyhow!("transcript content size exceeds supported range"))?,
            }));
        }
        SessionLogEntry::RuntimeContextSnapshotV2(_) => return Ok(None),
        SessionLogEntry::User(message) => {
            (message, ApplicationTranscriptRole::User, MessageRole::User)
        }
        SessionLogEntry::Assistant(message) => {
            for call in &message.tool_calls {
                if !tool_names.contains_key(&call.id) {
                    tool_name_order.push_back(call.id.clone());
                }
                tool_names.insert(
                    call.id.clone(),
                    truncate_application_transcript_text(&safe_persistence_text(&call.name), 128),
                );
            }
            while tool_name_order.len() > max_tool_names {
                if let Some(call_id) = tool_name_order.pop_front() {
                    tool_names.remove(&call_id);
                }
            }
            (
                message,
                ApplicationTranscriptRole::Assistant,
                MessageRole::Assistant,
            )
        }
        SessionLogEntry::ToolResultV3(result) => (
            result.model_message()?,
            ApplicationTranscriptRole::Tool,
            MessageRole::Tool,
        ),
    };
    let (message, role, expected_role) = message;
    if message.role != expected_role {
        bail!("durable transcript entry role does not match its entry class");
    }
    let safe_content = message.content.as_deref().map(safe_persistence_text);
    let original_content_bytes = safe_content.as_ref().map_or(0, String::len);
    Ok(Some(ApplicationTranscriptMessage {
        ordinal,
        message_id: safe_application_transcript_message_id(&message.id),
        role,
        content: safe_content.map(|content| {
            truncate_application_transcript_text(&content, MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES)
        }),
        assistant_kind: if role == ApplicationTranscriptRole::Assistant {
            message.assistant_kind
        } else {
            None
        },
        tool_name: message
            .tool_call_id
            .as_ref()
            .and_then(|call_id| tool_names.get(call_id))
            .cloned(),
        image_attachment_count: u64::try_from(message.image_attachments.len())
            .map_err(|_| anyhow!("transcript attachment count exceeds supported range"))?,
        truncated: original_content_bytes > MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES,
        original_content_bytes: u64::try_from(original_content_bytes)
            .map_err(|_| anyhow!("transcript content size exceeds supported range"))?,
    }))
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

pub(crate) fn safe_application_transcript_message_id(value: &str) -> String {
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
    rerun_application_verification_with_attachment(
        config_path,
        launch_cwd,
        session_path,
        expected_session_scope_id,
        services,
        request,
        None,
    )
    .await
}

/// Reruns verification while reusing a controller-owned cross-process session attachment.
pub async fn rerun_application_verification_with_attachment(
    config_path: &Path,
    launch_cwd: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    services: &ApplicationRunServices,
    request: &TaskVerificationRerunRequest,
    session_attachment: Option<
        Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    >,
) -> Result<VerificationProductView> {
    let config_path = config_path.to_owned();
    let launch_cwd = launch_cwd.to_owned();
    let session_path = session_path.to_owned();
    let expected_session_scope_id = expected_session_scope_id.to_owned();
    let session_leases = Arc::clone(&services.session_leases);
    let request = request.clone();
    let preparation = tokio::task::spawn_blocking(move || {
        let root_config = RootConfig::load(&config_path)?.with_effective_composition()?;
        anyhow::ensure!(
            root_config.task.enabled,
            "task orchestration is not selected for this session composition"
        );
        let workspace_root =
            resolve_workspace_root(&config_path, &launch_cwd, &root_config.workspace.root);
        let store = JsonlSessionStore::new(&session_path)?;
        let session_lease =
            session_leases.acquire_with_attachment(store.path(), session_attachment)?;
        let (_, fallback_route) = application_selected_model_route(&root_config, None, None)
            .map_err(|error| anyhow!(error))?;
        let session = load_application_session_for_route_with_attachment(
            &root_config,
            &fallback_route,
            store,
            Some(session_lease.attachment.as_ref()),
        )?;
        if session.session_scope_id() != expected_session_scope_id {
            bail!("durable session identity changed before verification rerun");
        }
        crate::validate_session_composition(&session, &root_config)?;
        Ok::<_, anyhow::Error>((session, session_lease, workspace_root, request))
    })
    .await
    .map_err(|_| anyhow!("verification rerun preparation worker failed"))??;
    let (mut session, _session_lease, workspace_root, request) = preparation;
    let verification_execution_port: Arc<
        dyn sigil_kernel::verification::VerificationExecutionPortV1,
    > = services
        .authority_composition()
        .ok_or_else(|| {
            anyhow!("current-schema verification rerun requires the managed verification route")
        })?
        .command_execution
        .clone();
    let mut handler = NoopEventHandler;
    rerun_task_verification_check(
        &mut session,
        &mut handler,
        verification_execution_port.as_ref(),
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
    let canonical_path = canonical_session_lease_path(session_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let attachment = Arc::new(
        crate::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &canonical_path,
        )
        .map_err(|error| match error {
            crate::interactive_session_attachment::InteractiveSessionAttachmentError::Busy {
                observed_generation,
            } => ApplicationRunPrepareError::SessionAlreadyActive {
                recovery_binding:
                    crate::interactive_session_attachment::session_attachment_path_recovery_binding(
                        &canonical_path,
                        &observed_generation,
                    ),
            },
            error => ApplicationRunPrepareError::execution(error),
        })?,
    );
    record_application_preparation_cancellation_with_attachment(
        config_path,
        &canonical_path,
        run_id,
        reason,
        attachment,
    )
}

/// Durably records a preparation cancellation while reusing a controller-owned attachment.
///
/// # Errors
///
/// Returns a typed preparation error when the attachment does not match the session,
/// configuration or route recovery fails, or durable cancellation evidence cannot be appended.
pub fn record_application_preparation_cancellation_with_attachment(
    config_path: &Path,
    session_path: &Path,
    run_id: &str,
    reason: &str,
    session_attachment: Arc<
        crate::interactive_session_attachment::InteractiveSessionAttachmentLease,
    >,
) -> std::result::Result<ApplicationSessionBinding, ApplicationRunPrepareError> {
    if run_id.trim().is_empty() || safe_persistence_text(run_id) != run_id {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "run id must be non-empty and persistence-safe".to_owned(),
        });
    }
    let root_config = load_application_root_config(config_path)?;
    let canonical_path = canonical_session_lease_path(session_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let store =
        JsonlSessionStore::new(&canonical_path).map_err(ApplicationRunPrepareError::execution)?;
    let (_, fallback_route) = application_selected_model_route(&root_config, None, None)?;
    let attachment_path = canonical_session_lease_path(session_attachment.session_path())
        .map_err(ApplicationRunPrepareError::execution)?;
    if attachment_path != canonical_path {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "supplied session attachment belongs to another durable session".to_owned(),
        });
    }
    let session = load_application_session_for_route_with_attachment(
        &root_config,
        &fallback_route,
        store,
        Some(session_attachment.as_ref()),
    )
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
        route_transition: crate::provider_connections::SessionRouteTransitionView {
            kind: crate::provider_connections::SessionRouteTransitionKind::Exact,
            connection_id: session
                .resolved_model_route()
                .map(|route| route.model_ref.connection_id.as_str().to_owned()),
            model_id: session
                .resolved_model_route()
                .map(|route| route.model_ref.model_id.clone()),
            remote_context_reset: false,
        },
    })
}

/// Creates the default durable V2 JSONL path for one new application session.
#[must_use]
pub fn default_application_session_path(session_log_dir: &Path) -> PathBuf {
    session_log_dir.join(format!("session-{}.jsonl", uuid::Uuid::new_v4()))
}

fn application_session_log_key(
    writer: &crate::managed_storage_writer::ManagedStorageWriterAdapterV1,
    requested_path: &Path,
) -> Result<String> {
    let managed_root = writer
        .managed_leaf_path(crate::managed_storage_writer::StorageWriterChannelV1::SessionLog)
        .map_err(|error| anyhow!("managed session-log root is unavailable: {error}"))?;
    let requested_parent = requested_path
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok());
    let key = if requested_path.file_name().and_then(|value| value.to_str())
        == Some("records.jsonl")
        && requested_parent
            .as_deref()
            .and_then(|parent| parent.strip_prefix(&managed_root).ok())
            .is_some_and(|relative| relative.components().count() == 1)
    {
        requested_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("managed session-log key is unavailable"))?
            .to_owned()
    } else {
        requested_path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("session-log path has no valid file stem"))?
            .to_owned()
    };
    Ok(key)
}

fn current_schema_managed_session_log_writer(
    services: &ApplicationRunServices,
) -> Option<Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>> {
    let cutover = services.cutover()?;
    if cutover.manifest().selected_epoch
        != sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
    {
        return None;
    }
    services
        .authority_composition()
        .map(|composition| Arc::clone(&composition.storage_writer))
}

/// Ordinary runs and continuations consume the same boot-owned broker and physical file port.
/// Legacy test fixtures remain absent; the public production entry point enforces boot readiness.
fn current_schema_tool_authority(
    services: &ApplicationRunServices,
) -> Option<Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>> {
    let cutover = services.cutover()?;
    if cutover.manifest().selected_epoch
        != sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
    {
        return None;
    }
    services
        .authority_composition()
        .map(|composition| Arc::new(composition.tool_authority.clone()))
}

fn current_schema_boot_composition(
    services: &ApplicationRunServices,
) -> Option<sigil_kernel::RuntimeCompositionConfig> {
    let cutover = services.cutover()?;
    (cutover.manifest().selected_epoch
        == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema)
        .then(|| cutover.manifest().composition.selection_only())
}

fn current_schema_managed_artifact_store_writer(
    services: &ApplicationRunServices,
) -> Option<Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>> {
    let cutover = services.cutover()?;
    if cutover.manifest().selected_epoch
        != sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
    {
        return None;
    }
    let composition = services.authority_composition()?;
    if !composition
        .declared_channels
        .contains(&crate::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging)
        || !composition
            .declared_channels
            .contains(&crate::managed_storage_writer::StorageWriterChannelV1::ArtifactStore)
    {
        return None;
    }
    Some(Arc::clone(&composition.storage_writer))
}

/// Builds provider input with safe repository context candidates.
#[must_use]
pub fn application_run_input(workspace_root: &Path, prompt: String) -> AgentRunInput {
    let runtime_context =
        context_candidates_from_safe_sources(workspace_root, &prompt, None).unwrap_or_default();
    let pending_input_provider = crate::pending_input::DurableQueuePendingInputProvider::new(
        crate::RequestContextResolver::new(workspace_root.to_path_buf(), None),
    );
    AgentRunInput::user(prompt)
        .with_runtime_context(runtime_context)
        .with_pending_input_provider(Arc::new(pending_input_provider))
}

#[cfg(test)]
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
    model_ref: sigil_kernel::ModelRef,
    options: AgentRunOptions,
    target_max_tokens: Option<u32>,
    input: AgentRunInput,
    run_id: String,
    prompt: String,
    interaction: ApplicationRunInteraction,
    redactor: sigil_kernel::SecretRedactor,
    tool_scope: Option<ToolRegistryScope>,
    skill_descriptor: Option<sigil_kernel::SkillDescriptor>,
    agent_invocation: Option<(crate::AgentProfileRegistry, AgentProfileId)>,
    task_agent_registry: Option<crate::AgentProfileRegistry>,
    generate_session_title: bool,
    route_transition: crate::provider_connections::SessionRouteTransitionView,
    managed_session_log: Option<ManagedApplicationSessionLogLease>,
    managed_artifact_store: Option<ManagedApplicationArtifactStoreLease>,
}

#[cfg(test)]
fn prepare_application_run_blocking(
    request: ApplicationRunRequest,
    session_leases: Arc<ApplicationSessionLeaseManager>,
    task_executor_attached: bool,
    tool_authority: Option<std::sync::Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>>,
) -> std::result::Result<BlockingApplicationRunPreparation, ApplicationRunPrepareError> {
    let fixture_config = load_application_root_config(&request.config_path)?;
    let expected_composition = sigil_kernel::RuntimeCompositionConfig::new(
        sigil_kernel::RuntimeCompositionProfile::Core,
        fixture_config.selected_capabilities(),
    );
    prepare_application_run_blocking_with_writer(
        request,
        session_leases,
        task_executor_attached,
        tool_authority,
        Some(expected_composition),
        None,
        None,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_application_run_blocking_with_writer(
    request: ApplicationRunRequest,
    session_leases: Arc<ApplicationSessionLeaseManager>,
    task_executor_attached: bool,
    tool_authority: Option<std::sync::Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>>,
    expected_composition: Option<sigil_kernel::RuntimeCompositionConfig>,
    managed_session_log_writer: Option<
        Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
    managed_artifact_store_writer: Option<
        Arc<crate::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
    managed_plan_review_child_resources: Option<
        Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    >,
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
    let mut root_config = load_application_root_config(&request.config_path)?;
    crate::application_mcp::merge(&mut root_config, &request.additional_mcp_servers)
        .map_err(ApplicationRunPrepareError::configuration)?;
    let expected_composition = expected_composition.as_ref().ok_or_else(|| {
        application_authority_prepare_error(
            sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable,
        )
    })?;
    crate::session_composition::validate_boot_composition(&root_config, expected_composition)
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
    let (requested_session_path, managed_session_log, managed_session_key) =
        if let Some(writer) = managed_session_log_writer {
            let key = application_session_log_key(&writer, &requested_session_path)
                .map_err(ApplicationRunPrepareError::execution)?;
            let managed_path = writer
                .session_log_path_for_key(&key)
                .map_err(ApplicationRunPrepareError::execution)?
                .join("records.jsonl");
            let lease = ManagedApplicationSessionLogLease::acquire(writer, &key)
                .map_err(ApplicationRunPrepareError::execution)?;
            (managed_path, Some(lease), Some(key))
        } else {
            (requested_session_path, None, None)
        };
    let session_store = JsonlSessionStore::new(&requested_session_path)
        .map_err(ApplicationRunPrepareError::execution)?;
    let session_path = session_store.path().to_owned();
    let session_lease = Arc::new(
        session_leases
            .acquire_with_attachment(&session_path, request.session_attachment.clone())
            .map_err(|error| {
                if error.is_already_active() {
                    ApplicationRunPrepareError::SessionAlreadyActive {
                        recovery_binding: error.recovery_binding().unwrap_or_default().to_owned(),
                    }
                } else {
                    ApplicationRunPrepareError::execution(error)
                }
            })?,
    );
    let background_runs = session_lease
        .attachment
        .agent_tool_background_runs()
        .map_err(ApplicationRunPrepareError::execution)?;
    let session_store = session_store.with_live_background_agent_threads(
        background_runs
            .thread_ids()
            .map_err(ApplicationRunPrepareError::execution)?,
    );
    let mutation_recorder = MutationEventRecorder::new(session_store.clone());
    if let Some(provisioner) = managed_plan_review_child_resources.as_deref() {
        crate::PlanReviewCoordinator::recover_managed_plan_review_drafts_from_store(
            session_store.clone(),
            provisioner,
            current_unix_time_ms(),
        )
        .map_err(ApplicationRunPrepareError::execution)?;
    }
    let (_, fallback_route) = application_selected_model_route(
        &root_config,
        request.model_connection_id.as_ref(),
        request.model_name.as_deref(),
    )?;
    let inspected = crate::provider_connections::inspect_session_for_route_resume(
        &root_config,
        &fallback_route,
        session_store,
    )
    .map_err(|_| ApplicationRunPrepareError::SessionStreamInvalid)?;
    let crate::provider_connections::InspectedSessionRouteResume {
        mut session,
        config_snapshot,
        plan,
        recovery_binding,
    } = inspected;
    crate::validate_session_composition(&session, &root_config)
        .map_err(ApplicationRunPrepareError::execution)?;
    let managed_artifact_store = if let (Some(writer), Some(key)) = (
        managed_artifact_store_writer,
        managed_session_key.as_deref(),
    ) {
        let lease = ManagedApplicationArtifactStoreLease::acquire(
            writer,
            key,
            &session_path,
            session.session_scope_id(),
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        session = session.with_tool_artifact_store_override(lease.store());
        Some(lease)
    } else {
        None
    };
    let route_authority = session_lease
        .route_mutation_authority(session.session_scope_id())
        .map_err(ApplicationRunPrepareError::execution)?;
    let explicit_model_selection = request.model_connection_id.is_some()
        && request.model_name.is_some()
        && request.model_selection_binding.is_some();
    let mut route_transition_kind = crate::provider_connections::SessionRouteTransitionKind::Exact;
    let mut route_remote_context_reset = false;
    match plan {
        crate::provider_connections::SessionRouteResumePlan::Exact { .. } => {}
        plan @ crate::provider_connections::SessionRouteResumePlan::RebindCurrentModel {
            ..
        } => {
            let permit = route_authority.issue_quiescence_permit().map_err(|error| {
                application_route_authority_prepare_error(error, &recovery_binding)
            })?;
            let outcome = crate::provider_connections::apply_session_route_resume_plan(
                &config_snapshot,
                &mut session,
                plan,
                permit,
            )
            .map_err(ApplicationRunPrepareError::execution)?;
            route_transition_kind =
                crate::provider_connections::SessionRouteTransitionKind::Rebound;
            route_remote_context_reset = outcome.private_state_reset;
        }
        crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation { .. }
            if explicit_model_selection
                && request.route_recovery_binding.as_deref() == Some(recovery_binding.as_str()) => {
        }
        plan @ crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation { .. }
            if request.route_recovery_binding.as_deref() == Some(recovery_binding.as_str()) =>
        {
            let permit = route_authority.issue_quiescence_permit().map_err(|error| {
                application_route_authority_prepare_error(error, &recovery_binding)
            })?;
            let outcome = crate::provider_connections::apply_session_route_confirmation_plan(
                &config_snapshot,
                &mut session,
                plan,
                &recovery_binding,
                permit,
            )
            .map_err(ApplicationRunPrepareError::execution)?;
            route_transition_kind =
                crate::provider_connections::SessionRouteTransitionKind::ExplicitlyConfirmed;
            route_remote_context_reset = outcome.private_state_reset;
        }
        crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation { .. } => {
            return Err(
                ApplicationRunPrepareError::SessionRouteConfirmationRequired { recovery_binding },
            );
        }
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement { .. }
            if explicit_model_selection
                && request.route_recovery_binding.as_deref() == Some(recovery_binding.as_str()) => {
        }
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement {
            reason: crate::provider_connections::SessionRouteUnavailableReason::ConnectionNotFound,
            ..
        } => {
            return Err(ApplicationRunPrepareError::SessionRouteSelectionRequired {
                recovery_binding,
            });
        }
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement {
            reason:
                crate::provider_connections::SessionRouteUnavailableReason::ConnectionConfigInvalid,
            ..
        }
        | crate::provider_connections::SessionRouteResumePlan::NeedsSetup {
            reason: crate::provider_connections::ModelRouteSetupReason::ConfigurationInvalid,
        } => {
            return Err(ApplicationRunPrepareError::connection_config_invalid(
                anyhow!("connection_config_invalid"),
            ));
        }
        crate::provider_connections::SessionRouteResumePlan::NeedsSetup {
            reason: crate::provider_connections::ModelRouteSetupReason::RouteNotConfigured,
        } => {
            return Err(ApplicationRunPrepareError::ModelRouteNotConfigured);
        }
    }
    let workspace_trust = workspace_trust_from_entries(session.entries(), &workspace_root)
        .map_err(ApplicationRunPrepareError::execution)?;
    let conversation_lifecycle = session
        .conversation_run_lifecycle_recorder()
        .map_err(ApplicationRunPrepareError::execution)?;
    conversation_lifecycle
        .reconcile_unfinished(current_unix_time_ms())
        .map_err(ApplicationRunPrepareError::execution)?;
    let selected_model = admit_application_model_selection(&request, &root_config, &session)?;
    let session_route = selected_model
        .as_ref()
        .map(|(_, route)| route.clone())
        .or_else(|| session.resolved_model_route().cloned())
        .ok_or_else(|| ApplicationRunPrepareError::execution(anyhow!("session_route_missing")))?;
    let runtime_provider_name =
        crate::provider_connections::validate_persisted_model_route(&root_config, &session_route)
            .map_err(ApplicationRunPrepareError::configuration)?;
    if let Some((selected_provider_name, _)) = selected_model.as_ref()
        && selected_provider_name != &runtime_provider_name
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "selected provider identity does not match its exact route".to_owned(),
        });
    }
    let mut identity_config = root_config.clone();
    identity_config.agent.runtime_provider = runtime_provider_name.clone();
    identity_config.agent.connection = Some(session_route.model_ref.connection_id.clone());
    identity_config.agent.model = session_route.model_ref.model_id.clone();
    admit_application_reasoning_effort(
        &request,
        &runtime_provider_name,
        &session_route.model_ref.model_id,
    )?;
    root_config.agent.runtime_provider = runtime_provider_name.clone();
    root_config.agent.connection = Some(session_route.model_ref.connection_id.clone());
    root_config.agent.model = session_route.model_ref.model_id.clone();
    if request.skill_binding.is_some() && request.agent_binding.is_some() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "a run cannot invoke an inline skill and an agent profile together".to_owned(),
        });
    }
    if let Some((provider_name, selected_route)) = selected_model {
        let outcome =
            if crate::provider_connections::explicit_session_route_selection_is_already_applied(
                &config_snapshot,
                &session,
                &provider_name,
                &selected_route,
            )
            .map_err(ApplicationRunPrepareError::execution)?
            {
                crate::provider_connections::SessionRouteResumeOutcome {
                    status: crate::provider_connections::SessionRouteResumeStatus::AlreadyApplied,
                    private_state_reset: false,
                }
            } else {
                let permit = route_authority.issue_quiescence_permit().map_err(|error| {
                    application_route_authority_prepare_error(error, &recovery_binding)
                })?;
                crate::provider_connections::apply_explicit_session_route_selection(
                    &config_snapshot,
                    &mut session,
                    &provider_name,
                    selected_route,
                    permit,
                )
                .map_err(ApplicationRunPrepareError::execution)?
            };
        route_transition_kind =
            crate::provider_connections::SessionRouteTransitionKind::ExplicitlyConfirmed;
        route_remote_context_reset = outcome.private_state_reset;
    }
    session_lease
        .acquire_route_execution_owner(session.session_scope_id())
        .map_err(ApplicationRunPrepareError::execution)?;
    let loaded_skill =
        admit_application_skill_binding(&request, &root_config, &workspace_root, &mut session)?;
    let agent_invocation = admit_application_agent_binding(
        &request,
        &root_config,
        &workspace_root,
        session.entries(),
    )?;
    let generate_session_title = root_config
        .composition
        .allows(sigil_kernel::OptionalCapability::SessionTitles)
        && request.constraints.is_none()
        && agent_invocation.is_none()
        && !session
            .entries()
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::User(_)));
    let task_agent_registry =
        if task_executor_attached && root_config.task.enabled && agent_invocation.is_none() {
            Some(
                crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
                    &root_config,
                    &workspace_root,
                    session.entries(),
                )
                .map_err(ApplicationRunPrepareError::execution)?,
            )
        } else {
            None
        };
    let model_ref = session_route.model_ref.clone();
    let effective_context_window = crate::resolve_model_context_window_tokens(
        &root_config,
        &model_ref,
        &runtime_provider_name,
    )
    .tokens;
    let requested_max_output_tokens = request
        .constraints
        .as_ref()
        .map(|constraints| constraints.max_output_tokens)
        .or(root_config.model_request.max_output_tokens);
    if let Err(error) =
        crate::validate_output_token_budget(effective_context_window, requested_max_output_tokens)
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: error.to_string(),
        });
    }
    let route_transition = crate::provider_connections::SessionRouteTransitionView {
        kind: route_transition_kind,
        connection_id: Some(session_route.model_ref.connection_id.as_str().to_owned()),
        model_id: Some(session_route.model_ref.model_id.clone()),
        remote_context_reset: route_remote_context_reset,
    };
    attach_session_url_capability_store(&mut session)
        .map_err(ApplicationRunPrepareError::execution)?;
    session
        .try_attach_image_attachment_resolver(Arc::new(crate::ControlledImageAttachmentCache::new(
            sigil_paths.attachments_root.clone(),
        )))
        .map_err(ApplicationRunPrepareError::execution)?;
    let mut image_message = ModelMessage::user("");
    image_message.image_attachments = request.image_attachments.clone();
    sigil_kernel::validate_message_image_attachments(&image_message)
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
        &identity_config,
        workspace_root.clone(),
        request.interaction.kernel_mode(),
        None,
    );
    // RFC-0071 R71.6: the boot surface already composed the real authority; hand the kernel
    // tool authority facade to the agent run so in-process file tools adjudicate against sealed
    // V3 plans (absent composition: legacy/shadow runs simply keep None).
    if let Some(authority) = tool_authority {
        options = options.with_tool_authority(authority);
    }
    if let Some(permission_mode) = request.permission_mode {
        options.permission_config.mode = permission_mode;
    }
    if let Some(reasoning_effort) = request.reasoning_effort {
        options.reasoning_effort = Some(reasoning_effort);
    }
    if let Some(constraints) = request.constraints.as_ref() {
        options.max_turns = Some(constraints.max_turns);
    }
    let configured_max_output_tokens = crate::configured_max_output_tokens(&root_config);
    let review_context = if request.review_annotations.is_empty() {
        String::new()
    } else {
        crate::materialize_review_annotations(
            request.session_path.as_deref().ok_or_else(|| {
                ApplicationRunPrepareError::InvalidInvocation {
                    message: "recorded change review requires its source session".to_owned(),
                }
            })?,
            session.session_scope_id(),
            &workspace_root,
            &request.review_annotations,
        )
        .map_err(ApplicationRunPrepareError::execution)?
    };
    let mut input = AgentRunInput::user(format!("{}{}", request.prompt, review_context))
        .with_image_attachments(request.image_attachments.clone())
        .with_logical_run_id(request.run_id.clone())
        .with_cancellation(cancellation_handle.clone())
        .with_pending_input_provider(Arc::new(
            crate::pending_input::DurableQueuePendingInputProvider::default(),
        ));
    if let Some(loaded_skill) = loaded_skill.as_ref() {
        input
            .transient_context
            .push(loaded_skill.transient_context.clone());
    }
    let target_max_tokens = request
        .constraints
        .as_ref()
        .map(|constraints| constraints.max_output_tokens)
        .or(configured_max_output_tokens);
    if let Some(max_output_tokens) = target_max_tokens {
        input = input.with_max_output_tokens(max_output_tokens);
    }
    let redactor = secret_redactor_for_root_config(&root_config);
    crate::bind_session_composition(&mut session, &root_config)
        .map_err(ApplicationRunPrepareError::execution)?;
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
        model_ref,
        options,
        target_max_tokens,
        input,
        run_id: request.run_id,
        prompt: request.prompt,
        interaction: request.interaction,
        redactor,
        tool_scope: request
            .constraints
            .map(|constraints| constraints.tool_scope),
        skill_descriptor: loaded_skill.map(|loaded| loaded.descriptor),
        agent_invocation,
        task_agent_registry,
        generate_session_title,
        route_transition,
        managed_session_log,
        managed_artifact_store,
    })
}

fn application_route_authority_prepare_error(
    error: crate::provider_connections::SessionRouteAuthorityError,
    recovery_binding: &str,
) -> ApplicationRunPrepareError {
    match error {
        crate::provider_connections::SessionRouteAuthorityError::ActiveOwners
        | crate::provider_connections::SessionRouteAuthorityError::TransitionInProgress => {
            ApplicationRunPrepareError::SessionWriterBusy {
                recovery_binding: recovery_binding.to_owned(),
            }
        }
        other => ApplicationRunPrepareError::execution(anyhow::Error::new(other)),
    }
}

#[cfg_attr(test, allow(dead_code))]
fn application_authority_prepare_error(
    error: sigil_kernel::cutover_manifest::CutoverErrorV1,
) -> ApplicationRunPrepareError {
    match error {
        sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable => {
            ApplicationRunPrepareError::AuthorityUnavailable {
                source: anyhow::Error::new(
                    sigil_kernel::cutover_manifest::CutoverErrorV1::AuthorityUnavailable,
                ),
            }
        }
        other => ApplicationRunPrepareError::Configuration {
            source: anyhow::Error::new(other),
        },
    }
}

fn admit_application_skill_binding(
    request: &ApplicationRunRequest,
    root_config: &RootConfig,
    workspace_root: &Path,
    session: &mut Session,
) -> std::result::Result<Option<crate::LoadedSkillContext>, ApplicationRunPrepareError> {
    let Some(binding) = request.skill_binding.as_ref() else {
        return Ok(None);
    };
    if !root_config.skills.enabled
        || !root_config
            .composition
            .allows(sigil_kernel::OptionalCapability::Skills)
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "skills are not selected for this session composition".to_owned(),
        });
    }
    let user_config_dir = sigil_kernel::default_user_config_dir().ok();
    let entries = match session.store_path() {
        Some(path) => sigil_kernel::JsonlSessionStore::read_entries(path)
            .map_err(ApplicationRunPrepareError::execution)?,
        None => session.entries().to_vec(),
    };
    let report = crate::discover_skill_index_with_session_entries(
        workspace_root,
        user_config_dir.as_deref(),
        &root_config.skills,
        &entries,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    if report.snapshot.fingerprint != binding.index_fingerprint {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "skill catalog binding is stale".to_owned(),
        });
    }
    let Some(descriptor) = report
        .snapshot
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id == binding.skill_id)
    else {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "bound skill is no longer present".to_owned(),
        });
    };
    if descriptor.sha256 != binding.skill_sha256 {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "skill content binding is stale".to_owned(),
        });
    }
    if descriptor.run_as != sigil_kernel::SkillRunMode::Inline {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "child-session skills require a supervised application owner".to_owned(),
        });
    }
    let loaded = crate::load_user_invoked_skill(
        workspace_root,
        &report.snapshot,
        &binding.skill_id,
        Some(request.run_id.clone()),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    session
        .append_control(ControlEntry::SkillLoaded(loaded.entry.clone()))
        .map_err(ApplicationRunPrepareError::execution)?;
    Ok(Some(loaded))
}

fn admit_application_agent_binding(
    request: &ApplicationRunRequest,
    root_config: &RootConfig,
    workspace_root: &Path,
    entries: &[SessionLogEntry],
) -> std::result::Result<
    Option<(crate::AgentProfileRegistry, AgentProfileId)>,
    ApplicationRunPrepareError,
> {
    let Some(binding) = request.agent_binding.as_ref() else {
        return Ok(None);
    };
    if !root_config.task.enabled
        || !root_config
            .composition
            .allows(sigil_kernel::OptionalCapability::TaskOrchestration)
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "agent profiles are not selected for this session composition".to_owned(),
        });
    }
    let profile_id = AgentProfileId::new(binding.profile_id.clone()).map_err(|_| {
        ApplicationRunPrepareError::InvalidInvocation {
            message: "agent profile binding contains an invalid profile id".to_owned(),
        }
    })?;
    let registry = crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        root_config,
        workspace_root,
        entries,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let profile =
        registry
            .get(&profile_id)
            .ok_or_else(|| ApplicationRunPrepareError::InvalidInvocation {
                message: "bound agent profile is no longer present".to_owned(),
            })?;
    if !profile.effective_enabled() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "bound agent profile is disabled".to_owned(),
        });
    }
    if profile.trust_state != sigil_kernel::AgentTrustState::Trusted {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "bound agent profile is not trusted".to_owned(),
        });
    }
    if !profile.effective_user_invocation_allowed() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "bound agent profile is not user-invocable".to_owned(),
        });
    }
    let snapshot = registry
        .capture_snapshot(&profile_id)
        .map_err(ApplicationRunPrepareError::execution)?;
    if snapshot.snapshot_id.as_str() != binding.snapshot_id {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "agent profile binding is stale".to_owned(),
        });
    }
    Ok(Some((registry, profile_id)))
}

fn admit_application_reasoning_effort(
    request: &ApplicationRunRequest,
    provider_name: &str,
    model_name: &str,
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
    let supported = crate::reasoning_effort::supported_reasoning_efforts(provider_name, model_name);
    let expected_binding =
        crate::reasoning_effort::reasoning_effort_binding(provider_name, model_name, &supported);
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

fn admit_application_model_selection(
    request: &ApplicationRunRequest,
    root_config: &RootConfig,
    session: &Session,
) -> std::result::Result<Option<(String, ResolvedModelRoute)>, ApplicationRunPrepareError> {
    match (
        request.model_connection_id.as_ref(),
        request.model_name.as_deref(),
        request.model_selection_binding.as_deref(),
    ) {
        (Some(connection_id), Some(model_name), None) => {
            let model_ref = ModelRef::new(connection_id.clone(), model_name).map_err(|error| {
                ApplicationRunPrepareError::InvalidInvocation {
                    message: error.to_string(),
                }
            })?;
            let selected =
                crate::provider_connections::resolve_model_route(root_config, &model_ref)
                    .map_err(ApplicationRunPrepareError::configuration)?;
            Ok(Some(selected))
        }
        (None, None, None) => Ok(None),
        (Some(connection_id), Some(model_name), Some(binding)) => {
            let current_route = session.resolved_model_route().ok_or_else(|| {
                ApplicationRunPrepareError::execution(anyhow!("session_route_missing"))
            })?;
            let expected_binding = application_model_selection_binding(&current_route.model_ref);
            if binding != expected_binding {
                return Err(ApplicationRunPrepareError::InvalidInvocation {
                    message: "model selection capability binding is stale".to_owned(),
                });
            }
            let model_ref = ModelRef::new(connection_id.clone(), model_name).map_err(|error| {
                ApplicationRunPrepareError::InvalidInvocation {
                    message: error.to_string(),
                }
            })?;
            let selected =
                crate::provider_connections::resolve_model_route(root_config, &model_ref)
                    .map_err(ApplicationRunPrepareError::configuration)?;
            Ok(Some(selected))
        }
        (Some(_), None, _) | (None, Some(_), _) | (None, None, Some(_)) => {
            Err(ApplicationRunPrepareError::InvalidInvocation {
                message: "connection, model, and capability binding must be supplied together"
                    .to_owned(),
            })
        }
    }
}

pub(crate) fn constrain_application_tool_registry(
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
pub(crate) struct ApplicationRunEventSequence {
    session_id: String,
    run_id: String,
    outbox_store: JsonlSessionStore,
    projection_owner: crate::RuntimeSessionProjectionOwner,
    outbox: PublicEventOutboxRecorder,
    live_preview: RuntimeLivePreviewSource,
    state: Arc<Mutex<ApplicationRunEventState>>,
    delivery_deferred: bool,
}

#[derive(Debug, Default)]
struct ApplicationRunEventState {
    sequence: u64,
    terminal: bool,
    terminal_delivered: bool,
    delivery_degraded: bool,
    live_delivery_prepared: bool,
}

/// Public publication preparation or durable outbox append failed.
///
/// This is a durability/authority failure, not an adapter-delivery failure. The execution owner
/// must leave its durable recovery path intact rather than manufacture a `RunFailed` terminal
/// from this wrapper.
#[derive(Debug, thiserror::Error)]
#[error("failed to prepare or durably append application public publication")]
struct ApplicationPublicOutboxAppendError {
    #[source]
    source: anyhow::Error,
}

fn is_application_public_outbox_append_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ApplicationPublicOutboxAppendError>()
        .is_some()
}

impl ApplicationRunEventSequence {
    /// Rejoins the one durable public stream for this run from the session outbox watermark.
    ///
    /// Runtime is the single live producer while this bridge is attached, but session outbox is
    /// the only durable source of its sequence. An adapter journal must never provide a second
    /// initial sequence or let a restarted bridge reuse an earlier value.
    pub(crate) fn with_outbox(
        session_id: String,
        run_id: String,
        outbox_store: JsonlSessionStore,
    ) -> Result<Self> {
        let records = outbox_store.read_event_records_writer()?;
        Self::with_outbox_records(session_id, run_id, outbox_store, &records)
    }

    // Used only while constructing a recorder from the same freshly validated durable prefix.
    // Delayed execution and adapter replay retain their own fresh reads.
    fn with_outbox_records(
        session_id: String,
        run_id: String,
        outbox_store: JsonlSessionStore,
        records: &[sigil_kernel::SessionStreamRecord],
    ) -> Result<Self> {
        let outbox = PublicEventOutboxRecorder::new(outbox_store.clone());
        let projection = PublicEventOutboxProjectionV1::from_records(records)?;
        let sequence = projection.durable_sequence(&run_id);
        let conversation_terminals = records
            .iter()
            .map(|record| record.stored_event())
            .filter(|event| {
                event.event_kind() == Some(sigil_kernel::DurableEventType::RunFinalized)
            })
            .map(|event| event.event_id.as_str())
            .collect::<BTreeSet<_>>();
        // Reopening the bridge must not reopen a finalized foreground run. The validated
        // pairs distinguish a root Awaiting terminal from a resumable revision Waiting event.
        let terminal = projection.events_in_order().into_iter().any(|entry| {
            entry.run_id == run_id
                && is_terminal_public_run_event(&entry.event.event)
                && (!matches!(
                    entry.event.event,
                    PublicRunEventKind::RunAwaitingUserInput { .. }
                ) || conversation_terminals.contains(entry.domain_event_id.as_str()))
        });
        Ok(Self {
            live_preview: RuntimeLivePreviewSource::new(&session_id, &run_id, terminal),
            projection_owner: crate::RuntimeSessionProjectionOwner::from_store(&outbox_store),
            session_id,
            run_id,
            outbox_store,
            outbox,
            state: Arc::new(Mutex::new(ApplicationRunEventState {
                sequence,
                terminal,
                ..ApplicationRunEventState::default()
            })),
            delivery_deferred: false,
        })
    }

    /// Replays every pending predecessor for this run before the live bridge can publish a new
    /// event. Delivery/receipt failure makes the bridge degraded and suppresses later live
    /// publication; malformed or unreadable durable state remains an authority error.
    fn replay_pending_before_live<H>(&self, handler: &mut H) -> Result<usize>
    where
        H: ApplicationRunEventHandler,
    {
        if self.delivery_deferred {
            self.mark_live_delivery_prepared()?;
            return Ok(0);
        }
        let records = self.outbox_store.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        let adapter = handler.public_event_adapter_id();
        let pending_ids = projection
            .pending_for_adapter(adapter)
            .into_iter()
            .filter(|entry| entry.run_id == self.run_id)
            .map(|entry| entry.public_event_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut replayed = 0;
        for entry in projection
            .events_in_order()
            .into_iter()
            .filter(|entry| pending_ids.contains(entry.public_event_id.as_str()))
        {
            if handler.handle_public_event(entry.event.clone()).is_err()
                || self
                    .outbox
                    .append_delivery(&PublicEventDeliveryReceiptV1 {
                        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                        public_event_id: entry.public_event_id.clone(),
                        adapter: adapter.to_owned(),
                        delivered_at_unix_ms: current_unix_time_ms(),
                    })
                    .is_err()
            {
                self.mark_delivery_degraded()?;
                return Ok(replayed);
            }
            replayed += 1;
        }
        self.mark_live_delivery_prepared()?;
        Ok(replayed)
    }

    /// Ensures control paths that do not construct `PublicApplicationEventBridge` still replay
    /// all older pending entries before their first live publication. This matters for durable
    /// cancellation and Task finalizers, which must not let a terminal overtake a prior failed
    /// progress delivery.
    fn ensure_pending_replayed_before_live<H>(&self, handler: &mut H) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        let prepared = self
            .state
            .lock()
            .map(|state| state.live_delivery_prepared)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if !prepared {
            self.replay_pending_before_live(handler)?;
        }
        Ok(())
    }

    fn emit<H>(&self, handler: &mut H, event: PublicRunEventKind) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        self.emit_nonterminal(handler, event, false)
    }

    /// Appends an owned terminal-task lifecycle update to the same public outbox stream.
    ///
    /// A persistent terminal task can outlive the foreground conversation terminal, so this is
    /// allowed to advance that stream afterwards. The deferred adapter recorder separately
    /// permits durable user-input lifecycle updates for a terminal Awaiting root. Ordinary
    /// application events remain rejected once the foreground terminal is durable.
    pub(crate) fn emit_terminal_lifecycle<H>(
        &self,
        handler: &mut H,
        event: sigil_kernel::TerminalLifecycleEvent,
    ) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        self.emit_nonterminal(
            handler,
            PublicRunEventKind::TerminalLifecycle { event },
            true,
        )
    }

    fn emit_nonterminal<H>(
        &self,
        handler: &mut H,
        event: PublicRunEventKind,
        allow_after_foreground_terminal: bool,
    ) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        if sigil_kernel::is_transient_public_run_event(&event) {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
            if state.terminal {
                bail!("application run event stream is already terminal");
            }
            return self.live_preview.apply_delta(&event, state.sequence);
        }
        self.ensure_pending_replayed_before_live(handler)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal && !allow_after_foreground_terminal {
            bail!("application run event stream is already terminal");
        }
        let sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        if is_terminal_public_run_event(&event) {
            bail!(
                "application run terminal requires an atomically committed conversation terminal and public outbox"
            );
        }
        let public = PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            sequence,
            event,
        );
        let public_event_id = application_public_event_id(&self.session_id, &self.run_id, sequence);
        let entry = PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            // A nonterminal public event is its own publication fact. Terminal bundles retain
            // their separately owned domain envelope below.
            domain_event_id: public_event_id.clone(),
            public_event_id,
            run_id: self.run_id.clone(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(
                &serde_json::to_vec(&public)
                    .context("failed to encode application public outbox event")?,
            ),
            event: public.clone(),
        };
        if let Err(source) = self.outbox.append_outbox(&entry) {
            // An append acknowledgement can be lost after the writer commits. The watermark
            // alone is not enough evidence: another entry can occupy the same sequence. Retry
            // the exact immutable entry so the kernel verifies every identity and payload field
            // before this bridge is allowed to publish it.
            if self.outbox.append_outbox(&entry).is_err() {
                return Err(anyhow::Error::new(ApplicationPublicOutboxAppendError {
                    source,
                }));
            }
        }
        state.sequence = sequence;
        self.deliver_committed(&mut state, handler, public, &entry.public_event_id);
        Ok(())
    }

    fn emit_terminal<H>(
        &self,
        lifecycle: &ConversationRunLifecycleRecorder,
        handler: &mut H,
        terminal: &ConversationRunFinalizedEntryV1,
        event: PublicRunEventKind,
    ) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        if !is_terminal_public_run_event(&event) {
            bail!("application conversation terminal requires a terminal public run event");
        }
        self.ensure_pending_replayed_before_live(handler)?;
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
        let public = PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            sequence,
            event,
        );
        let public_event_id = application_public_event_id(&self.session_id, &self.run_id, sequence);
        let outbox_entry = PublicEventOutboxEntryV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.clone(),
            domain_event_id: format!("application-domain:{}:{}", self.run_id, sequence),
            run_id: self.run_id.clone(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(
                &serde_json::to_vec(&public)
                    .context("failed to encode application terminal public outbox event")?,
            ),
            event: public.clone(),
        };
        // The kernel validates this exact public event against `terminal.status()` while it
        // atomically persists the durable conversation finalizer and its outbox record.
        lifecycle
            .append_finalized_with_outbox(terminal, &outbox_entry)
            .context(
                "failed to atomically persist application conversation terminal and public outbox",
            )?;

        // From this point on, the terminal fact is durable. Adapter delivery and receipt writes
        // are recoverable transport concerns and must never revise this business conclusion.
        state.sequence = sequence;
        state.terminal = true;
        state.terminal_delivered =
            self.deliver_committed(&mut state, handler, public, &outbox_entry.public_event_id);
        Ok(())
    }

    /// Prepares the exact resumable revision-waiting publication. The corresponding attempt and
    /// outbox entry must be committed together by the kernel before this payload is delivered.
    fn prepare_plan_review_revision_waiting(
        &self,
        event: PublicRunEventKind,
    ) -> Result<PublicRunEvent> {
        if !matches!(event, PublicRunEventKind::RunAwaitingUserInput { .. }) {
            bail!("resumable plan-review event must be RunAwaitingUserInput");
        }
        let state = self
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
        Ok(PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            sequence,
            event,
        ))
    }

    /// Advances this live producer only after the kernel has committed the exact Waiting pair.
    fn mark_plan_review_revision_waiting_committed(&self, event: &PublicRunEvent) -> Result<()> {
        if event.session_id != self.session_id || event.run_id != self.run_id {
            bail!("committed plan-review waiting belongs to another event stream");
        }
        if !matches!(event.event, PublicRunEventKind::RunAwaitingUserInput { .. }) {
            bail!("committed plan-review waiting has the wrong public payload");
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        let expected_sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        if event.sequence != expected_sequence {
            bail!("committed plan-review waiting sequence does not match the runtime bridge");
        }
        state.sequence = event.sequence;
        Ok(())
    }

    /// Delivers an already committed revision Waiting notification without creating a second
    /// writer path. A delivery failure leaves this exact outbox entry pending for ordered replay.
    fn deliver_plan_review_revision_waiting<H>(
        &self,
        handler: &mut H,
        event: PublicRunEvent,
        public_event_id: &str,
    ) -> Result<()>
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
        if event.sequence != state.sequence {
            bail!("committed plan-review waiting does not match the runtime bridge frontier");
        }
        self.deliver_committed(&mut state, handler, event, public_event_id);
        Ok(())
    }

    fn prepare_terminal_public_event(&self, event: PublicRunEventKind) -> Result<PublicRunEvent> {
        if !is_terminal_public_run_event(&event) {
            bail!("application terminal preparation requires a terminal public run event");
        }
        let state = self
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
        Ok(PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            sequence,
            event,
        ))
    }

    fn mark_terminal_committed(&self, event: &PublicRunEvent) -> Result<()> {
        if event.session_id != self.session_id || event.run_id != self.run_id {
            bail!("committed application terminal belongs to another event stream");
        }
        if !is_terminal_public_run_event(&event.event) {
            bail!("committed application terminal has a nonterminal public payload");
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        let expected_sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        if event.sequence != expected_sequence {
            bail!("committed application terminal sequence does not match the runtime bridge");
        }
        state.sequence = event.sequence;
        state.terminal = true;
        Ok(())
    }

    /// Attempts the adapter edge only after its event is durable. Once an adapter or receipt
    /// fails, the live bridge stops publishing later events so a future replay preserves order.
    /// These errors are deliberately not returned into the agent event path: they are transport
    /// degradation, not a new domain terminal.
    fn deliver_committed<H>(
        &self,
        state: &mut ApplicationRunEventState,
        handler: &mut H,
        event: PublicRunEvent,
        public_event_id: &str,
    ) -> bool
    where
        H: ApplicationRunEventHandler,
    {
        self.live_preview
            .apply_committed(&event.event, state.terminal);
        if self.delivery_deferred {
            return false;
        }
        if state.delivery_degraded {
            return false;
        }
        if handler.handle_public_event(event).is_err() {
            state.delivery_degraded = true;
            return false;
        }
        let receipt = PublicEventDeliveryReceiptV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.to_owned(),
            adapter: handler.public_event_adapter_id().to_owned(),
            delivered_at_unix_ms: current_unix_time_ms(),
        };
        if self.outbox.append_delivery(&receipt).is_err() {
            state.delivery_degraded = true;
            return false;
        }
        true
    }

    fn terminal_was_delivered(&self) -> Result<bool> {
        self.state
            .lock()
            .map(|state| state.terminal_delivered)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
    }

    fn delivery_is_degraded(&self) -> Result<bool> {
        self.state
            .lock()
            .map(|state| state.delivery_degraded)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
    }

    fn mark_delivery_degraded(&self) -> Result<()> {
        self.state
            .lock()
            .map(|mut state| {
                state.delivery_degraded = true;
                state.live_delivery_prepared = true;
            })
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
    }

    fn mark_live_delivery_prepared(&self) -> Result<()> {
        self.state
            .lock()
            .map(|mut state| state.live_delivery_prepared = true)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
    }
}

fn application_public_event_id(session_id: &str, run_id: &str, sequence: u64) -> String {
    format!("application-public:{session_id}:{run_id}:{sequence}")
}

/// Replays the exact pending public events for one application run to one adapter in durable
/// order.
///
/// This is a transport-recovery operation only: it reads the already committed outbox entry,
/// forwards its original id/sequence/payload to the adapter, and appends that adapter's receipt
/// only after acceptance. It never appends or derives a new domain terminal.
///
/// # Errors
///
/// Returns an error when the durable outbox cannot be rebuilt, the adapter rejects an exact
/// pending event, or the corresponding delivery receipt cannot be persisted. Replay stops at the
/// first failure so later entries can never overtake an unresolved predecessor.
pub fn replay_pending_application_outbox<H>(
    session_log_path: &Path,
    run_id: &str,
    handler: &mut H,
) -> Result<usize>
where
    H: ApplicationRunEventHandler,
{
    let store = JsonlSessionStore::new(session_log_path)?;
    let records = store.read_event_records_writer()?;
    let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
    let adapter = handler.public_event_adapter_id();
    let pending_ids = projection
        .pending_for_adapter(adapter)
        .into_iter()
        .filter(|entry| entry.run_id == run_id)
        .map(|entry| entry.public_event_id.as_str())
        .collect::<BTreeSet<_>>();
    let recorder = PublicEventOutboxRecorder::new(store);
    let mut replayed = 0;
    for entry in projection
        .events_in_order()
        .into_iter()
        .filter(|entry| pending_ids.contains(entry.public_event_id.as_str()))
    {
        handler
            .handle_public_event(entry.event.clone())
            .with_context(|| {
                format!(
                    "adapter {adapter} rejected pending application public event {}",
                    entry.public_event_id
                )
            })?;
        recorder.append_delivery(&PublicEventDeliveryReceiptV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: entry.public_event_id.clone(),
            adapter: adapter.to_owned(),
            delivered_at_unix_ms: current_unix_time_ms(),
        })?;
        replayed += 1;
    }
    Ok(replayed)
}

/// Root-conversation terminal classification. A revision Waiting event intentionally shares the
/// public `RunAwaitingUserInput` kind, but is admitted only through its separate atomic
/// plan-review Waiting attempt/outbox bundle rather than this root terminal path.
fn is_terminal_public_run_event(event: &PublicRunEventKind) -> bool {
    matches!(
        event,
        PublicRunEventKind::RunFinished { .. }
            | PublicRunEventKind::RunFailed { .. }
            | PublicRunEventKind::RunBlocked { .. }
            | PublicRunEventKind::RunPaused { .. }
            | PublicRunEventKind::RunInterrupted { .. }
            | PublicRunEventKind::RunCancelled
            | PublicRunEventKind::RunAwaitingUserInput { .. }
    )
}

/// One application-surface revision execution and its optional durable terminal outbox.
///
/// A waiting research-input suspension is not a revision finalizer: the same attempt can resume,
/// so it deliberately has no terminal outbox in this result.
#[derive(Debug, Clone)]
pub struct PlanReviewRevisionExecution {
    /// The exact domain outcome reconstructed from the current execution or durable terminal.
    pub outcome: crate::PlanReviewRunOutcome,
    /// The only terminal payload an adapter may publish. `None` is only valid for a resumable
    /// waiting-input outcome.
    pub terminal_outbox: Option<PublicEventOutboxEntryV1>,
    /// The exact durable suspension event for a resumable research question. It is not a terminal
    /// finalizer; a later answer resumes this same child logical run from the paired Waiting
    /// attempt and outbox record.
    pub waiting_public_event: Option<PublicRunEvent>,
}

/// Restores a Plan after its owner confirms an accepted revision never started.
///
/// The adapter must exclude an active dispatch for this exact request before calling.
///
/// # Errors
///
/// Rejects another session, stale guidance, and any already-started revision.
pub fn record_application_plan_revision_dispatch_failure(
    session_log_path: &Path,
    expected_scope: &str,
    request: &crate::PlanReviewRunRequest,
    reason: &str,
) -> Result<()> {
    let mut session = load_application_control_session(session_log_path, expected_scope)?;
    crate::PlanReviewCoordinator::record_unstarted_plan_revision_failure(
        &mut session,
        request,
        reason,
        current_unix_time_ms(),
    )?;
    Ok(())
}

/// Executes one prepared plan review revision on an application-surface session.
///
/// HTTP/Desktop use this so `Revise` actually runs the new read-only plan review instead of
/// leaving a dangling `Started` attempt. The run is fail-closed: only the frozen read-only tool
/// surface is exposed and the permission mode is read-only. Every revision finalizer is committed
/// with its exact public outbox before this function returns it to an adapter; an uncertain
/// append is reconciled from the writer before any generic failure is considered.
pub async fn execute_plan_review_revision_with_managed_execution<H>(
    root_config: &RootConfig,
    workspace_root: &Path,
    session_log_path: &Path,
    request: &crate::PlanReviewRunRequest,
    handler: &mut H,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
    managed_command_execution: Option<
        Arc<crate::managed_resource_adapters::RuntimeManagedCommandExecutionRouteV1>,
    >,
    managed_tool_authority: Option<Arc<sigil_kernel::tool_authority::KernelToolAuthorityV1>>,
    child_resource_provisioner: Option<
        Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    >,
) -> Result<PlanReviewRevisionExecution>
where
    H: ApplicationRunEventHandler + Send,
{
    let cancellation_handle = cancellation.unwrap_or_else(|| RunCancellationOwner::new().handle());
    let mut session =
        load_application_control_session(session_log_path, &request.source_turn.session_scope_id)?;
    if !cancellation_handle.is_cancel_requested()
        && let Some(provisioner) = child_resource_provisioner.as_deref()
    {
        crate::PlanReviewCoordinator::recover_managed_plan_review_drafts(
            &mut session,
            provisioner,
            current_unix_time_ms(),
        )?;
    }
    if let Some(outbox) =
        session.reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
    {
        let outcome = crate::PlanReviewCoordinator::revision_outcome_from_terminal(
            &session, request, &outbox,
        )?;
        return Ok(PlanReviewRevisionExecution {
            outcome,
            terminal_outbox: Some(outbox),
            waiting_public_event: None,
        });
    }
    let managed_research_input_allows_resume = child_resource_provisioner
        .as_deref()
        .map(|provisioner| {
            crate::PlanReviewCoordinator::managed_plan_review_research_input_allows_resume(
                &session,
                request,
                provisioner,
            )
        })
        .transpose()?
        .unwrap_or(false);
    if let Some(outbox) =
        session.reconcile_plan_review_revision_waiting(&request.child_logical_run_id())?
        && !managed_research_input_allows_resume
    {
        let outcome = crate::PlanReviewCoordinator::revision_waiting_outcome_from_outbox(
            &session, request, &outbox,
        )?;
        return Ok(PlanReviewRevisionExecution {
            outcome,
            terminal_outbox: None,
            waiting_public_event: Some(outbox.event),
        });
    }
    crate::PlanReviewCoordinator::ensure_revision_attempt_started(
        &mut session,
        request,
        current_unix_time_ms(),
    )?;
    let mut bridge = PublicApplicationEventBridge::new(
        ApplicationRunEventSequence::with_outbox(
            session.session_scope_id().to_owned(),
            request.child_logical_run_id(),
            JsonlSessionStore::new(session_log_path)?,
        )?,
        handler,
    )?;
    // A revision Waiting attempt is resumable, not terminal. Its next execution must append a
    // fresh durable start so application projections clear the old waiting presentation only
    // after an actual resumed run has been admitted.
    bridge.emit(PublicRunEventKind::RunStarted {
        prompt: "plan review revision".to_owned(),
    })?;
    let execution = (async {
        // Route and provider readiness belong to this admitted execution so a removed
        // connection restores the base Plan through the same durable terminal path.
        let preparation = (async {
            let route = session.resolved_model_route().context(
                "plan review session has no model route; select a model before retrying",
            )?;
            crate::provider_connections::validate_persisted_model_route(root_config, route)?;
            crate::build_provider_for_model_ref_async(root_config, &route.model_ref).await
        }).await;
        let provider = match preparation {
            Ok(provider) => provider,
            Err(error) => {
                let redactor = secret_redactor_for_root_config(root_config);
                let reason = sigil_kernel::safe_persistence_text(&redactor.redact_text(
                    &format!("plan review revision could not start: {error:#}"),
                ));
                return crate::plan_review_coordinator::complete_plan_review_run(
                    &cancellation_handle,
                    crate::PlanReviewRunOutcome::Failed(reason),
                );
            }
        };
        let mut base_registry = sigil_kernel::ToolRegistry::new();
        let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
        let builtin_paths = sigil_tools_builtin::BuiltinToolPaths {
            changesets_root: paths.changesets_root.clone(),
            changesets_label_root: PathBuf::from("state/artifacts/changesets"),
            terminal_tasks_root: paths.terminal_tasks_root.clone(),
            terminal_tasks_label_root: PathBuf::from("state/artifacts/tasks"),
            scratch_root: paths.scratch_root.clone(),
            scratch_label: "cache/tmp".to_owned(),
            scratch_quota: sigil_tools_builtin::ScratchQuota::default(),
        };
        let managed_command_execution = match managed_command_execution {
            Some(route) => Some(route),
            None => {
                #[cfg(test)]
                {
                    sigil_tools_builtin::register_builtin_tools_with_unavailable_managed_execution(
                        &mut base_registry,
                        builtin_paths.clone(),
                    );
                    None
                }
                #[cfg(not(test))]
                {
                    bail!(
                        "current-schema plan review requires the managed command execution route"
                    );
                }
            }
        };
        if let Some(managed_command_execution) = managed_command_execution {
            let managed_executor: Arc<dyn sigil_tools_builtin::ManagedCommandExecutionPortV1> =
                managed_command_execution.clone();
            let managed_terminal: Arc<dyn sigil_tools_builtin::ManagedTerminalExecutionPortV1> =
                managed_command_execution;
            sigil_tools_builtin::register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
                &mut base_registry,
                builtin_paths,
                managed_executor,
                sigil_tools_builtin::TerminalExecutionConfig::from_execution_config(
                    &root_config.execution,
                ),
                None,
                None,
                managed_terminal,
            );
        }
        crate::register_agent_tools(&mut base_registry, root_config)?;
        let tool_registry =
            crate::build_plan_review_tool_registry(&base_registry, root_config).into_registry();
        let mut options = crate::build_run_options(
            root_config,
            workspace_root.to_path_buf(),
            sigil_kernel::InteractionMode::Headless,
            None,
        );
        if let Some(tool_authority) = managed_tool_authority {
            options = options.with_tool_authority(tool_authority);
        }
        let agent = crate::configured_agent(root_config, provider, base_registry)?;
        let outcome = match child_resource_provisioner {
            Some(provisioner) => {
                crate::PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
                    &mut session,
                    request,
                    &agent,
                    options,
                    tool_registry,
                    &mut bridge,
                    &mut sigil_kernel::AutoApproveHandler,
                    cancellation_handle,
                    provisioner,
                )
                .await
            }
            None => {
                #[cfg(test)]
                {
                    crate::PlanReviewCoordinator::run_plan_review(
                        &mut session,
                        request,
                        &agent,
                        options,
                        tool_registry,
                        &mut bridge,
                        &mut sigil_kernel::AutoApproveHandler,
                        cancellation_handle,
                    )
                    .await
                }
                #[cfg(not(test))]
                {
                    bail!(
                        "current-schema plan review revision requires the composed child resource bundle"
                    );
                }
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return Err(error),
        };
        Ok(outcome)
    })
    .await;
    let outcome = match execution {
        Ok(outcome) => outcome,
        Err(error) if is_application_public_outbox_append_error(&error) => {
            return Err(error).context(
                "plan review revision public outbox append was not confirmed; durable recovery must decide the next terminal",
            );
        }
        Err(error) => {
            if let Some(outbox) =
                session.reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
            {
                let outcome = crate::PlanReviewCoordinator::revision_outcome_from_terminal(
                    &session, request, &outbox,
                )?;
                bridge.mark_plan_review_revision_terminal_committed(&outbox.event)?;
                return Ok(PlanReviewRevisionExecution {
                    outcome,
                    terminal_outbox: Some(outbox),
                    waiting_public_event: None,
                });
            }
            // The execution future has actually ended, and writer recovery just established
            // that no earlier finalizer won. This is a confirmed interruption, not a Failed
            // fallback. If either the new bundle or its recovery fails below, return that error
            // unchanged and retain Started for a later strict recovery attempt.
            let interrupted = crate::PlanReviewRunOutcome::Interrupted(
                "plan review revision execution ended before a terminal outcome".to_owned(),
            );
            let public_kind =
                crate::PlanReviewCoordinator::revision_terminal_public_event(&interrupted)
                    .expect("Interrupted is a revision terminal public event");
            let public_event = bridge.prepare_plan_review_revision_terminal(public_kind)?;
            let outbox = match crate::PlanReviewCoordinator::commit_revision_terminal_with_outbox(
                &mut session,
                request,
                &interrupted,
                public_event,
                current_unix_time_ms(),
            ) {
                Ok(outbox) => outbox,
                Err(commit_error) => match session
                    .reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
                {
                    Some(outbox) => outbox,
                    None => return Err(commit_error.context(format!(
                        "plan review revision execution failed ({error:#}) and its interrupted terminal bundle was not confirmed"
                    ))),
                },
            };
            let durable_outcome = crate::PlanReviewCoordinator::revision_outcome_from_terminal(
                &session, request, &outbox,
            )?;
            bridge.mark_plan_review_revision_terminal_committed(&outbox.event)?;
            return Ok(PlanReviewRevisionExecution {
                outcome: durable_outcome,
                terminal_outbox: Some(outbox),
                waiting_public_event: None,
            });
        }
    };
    if matches!(
        &outcome,
        crate::PlanReviewRunOutcome::AwaitingUserInput { .. }
    ) {
        let crate::PlanReviewRunOutcome::AwaitingUserInput { request: pending } = &outcome else {
            unreachable!("AwaitingUserInput was matched above");
        };
        let waiting_event = bridge.prepare_plan_review_revision_waiting(
            PublicRunEventKind::RunAwaitingUserInput {
                request_id: pending.identity.request_id.as_str().to_owned(),
                generation: pending.identity.generation,
                request_hash: pending.request_hash.clone(),
            },
        )?;
        let waiting_outbox = crate::PlanReviewCoordinator::commit_revision_waiting_with_outbox(
            &mut session,
            request,
            pending,
            waiting_event,
            current_unix_time_ms(),
        )?;
        bridge.mark_plan_review_revision_waiting_committed(&waiting_outbox.event)?;
        bridge.deliver_plan_review_revision_waiting(
            waiting_outbox.event.clone(),
            &waiting_outbox.public_event_id,
        )?;
        return Ok(PlanReviewRevisionExecution {
            outcome,
            terminal_outbox: None,
            waiting_public_event: Some(waiting_outbox.event),
        });
    }
    let public_kind = crate::PlanReviewCoordinator::revision_terminal_public_event(&outcome)
        .context("revision final outcome did not produce a public terminal payload")?;
    let public_event = bridge.prepare_plan_review_revision_terminal(public_kind)?;
    let outbox = match crate::PlanReviewCoordinator::commit_revision_terminal_with_outbox(
        &mut session,
        request,
        &outcome,
        public_event,
        current_unix_time_ms(),
    ) {
        Ok(outbox) => outbox,
        Err(error) => match session
            .reconcile_plan_review_revision_terminal(&request.child_logical_run_id())?
        {
            Some(outbox) => outbox,
            None => return Err(error),
        },
    };
    let durable_outcome =
        crate::PlanReviewCoordinator::revision_outcome_from_terminal(&session, request, &outbox)?;
    bridge.mark_plan_review_revision_terminal_committed(&outbox.event)?;
    Ok(PlanReviewRevisionExecution {
        outcome: durable_outcome,
        terminal_outbox: Some(outbox),
        waiting_public_event: None,
    })
}

/// Test-only compatibility wrapper for plan-review fixtures that do not compose authority.
#[cfg(test)]
pub async fn execute_plan_review_revision<H>(
    root_config: &RootConfig,
    workspace_root: &Path,
    session_log_path: &Path,
    request: &crate::PlanReviewRunRequest,
    handler: &mut H,
    cancellation: Option<sigil_kernel::RunCancellationHandle>,
) -> Result<crate::PlanReviewRunOutcome>
where
    H: ApplicationRunEventHandler + Send,
{
    execute_plan_review_revision_with_managed_execution(
        root_config,
        workspace_root,
        session_log_path,
        request,
        handler,
        cancellation,
        None,
        None,
        None,
    )
    .await
    .map(|execution| execution.outcome)
}

fn task_projector_from_records(
    records: &[sigil_kernel::SessionStreamRecord],
) -> Result<PublicTaskEventProjector> {
    let mut task_events = PublicTaskEventProjector::default();
    for record in records {
        if let Some(SessionLogEntry::Control(control)) = record.session_log_entry()? {
            task_events.project_control(&control)?;
        }
    }
    Ok(task_events)
}

struct PublicApplicationEventBridge<'a, H> {
    events: ApplicationRunEventSequence,
    task_events: PublicTaskEventProjector,
    handler: &'a mut H,
}

impl<'a, H> PublicApplicationEventBridge<'a, H>
where
    H: ApplicationRunEventHandler,
{
    fn new(events: ApplicationRunEventSequence, handler: &'a mut H) -> Result<Self> {
        handler.bind_live_preview_source(events.live_preview.clone())?;
        events.replay_pending_before_live(handler)?;
        let task_events =
            task_projector_from_records(&events.outbox_store.read_event_records_writer()?)?;
        Ok(Self {
            events,
            task_events,
            handler,
        })
    }

    fn emit(&mut self, event: PublicRunEventKind) -> Result<()> {
        self.events.emit(self.handler, event)
    }

    fn emit_conversation_terminal(
        &mut self,
        lifecycle: &ConversationRunLifecycleRecorder,
        run_id: &str,
        terminal_status: ApplicationRunTerminalStatus,
        final_message_id: Option<String>,
        summary: Option<&str>,
        redactor: &sigil_kernel::SecretRedactor,
        event: PublicRunEventKind,
    ) -> Result<()> {
        emit_application_conversation_terminal(
            lifecycle,
            &self.events,
            self.handler,
            run_id,
            terminal_status,
            final_message_id,
            summary,
            redactor,
            event,
        )
    }

    fn conversation_terminal_committed(&self) -> Result<bool> {
        self.events
            .state
            .lock()
            .map(|state| state.terminal)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
    }

    fn prepare_plan_review_revision_terminal(
        &self,
        event: PublicRunEventKind,
    ) -> Result<PublicRunEvent> {
        self.events.prepare_terminal_public_event(event)
    }

    fn mark_plan_review_revision_terminal_committed(&self, event: &PublicRunEvent) -> Result<()> {
        self.events.mark_terminal_committed(event)
    }

    fn prepare_plan_review_revision_waiting(
        &self,
        event: PublicRunEventKind,
    ) -> Result<PublicRunEvent> {
        self.events.prepare_plan_review_revision_waiting(event)
    }

    fn mark_plan_review_revision_waiting_committed(&self, event: &PublicRunEvent) -> Result<()> {
        self.events
            .mark_plan_review_revision_waiting_committed(event)
    }

    fn deliver_plan_review_revision_waiting(
        &mut self,
        event: PublicRunEvent,
        public_event_id: &str,
    ) -> Result<()> {
        self.events
            .deliver_plan_review_revision_waiting(self.handler, event, public_event_id)
    }

    fn commit_plan_review_terminal_bundle(
        &mut self,
        session: &mut Session,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
        terminal: ConversationRunFinalizedEntryV1,
        terminal_event: PublicRunEventKind,
    ) -> Result<bool> {
        self.events
            .ensure_pending_replayed_before_live(self.handler)?;
        if session.session_scope_id() != self.events.session_id {
            bail!("application plan-review terminal belongs to another durable session");
        }
        if session.store_path() != Some(self.events.outbox_store.path()) {
            bail!("application plan-review terminal uses a different durable session store");
        }
        let mut state = self
            .events
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        let next_sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        let mut staged_task_events = self.task_events.clone();
        for entry in &entries {
            if let SessionLogEntry::Control(control) = entry {
                staged_task_events
                    .project_control(control)
                    .map_err(|source| {
                        anyhow::Error::new(ApplicationPublicOutboxAppendError { source })
                    })?;
            }
        }
        let (_events, outbox) = session
            .append_session_entries_with_terminal_outbox(
                entries,
                publications,
                terminal,
                terminal_event,
                &self.events.run_id,
                next_sequence,
            )
            .map_err(|source| anyhow::Error::new(ApplicationPublicOutboxAppendError { source }))?;
        self.task_events = staged_task_events;
        let terminal_id = outbox
            .last()
            .map(|entry| entry.public_event_id.clone())
            .context("terminal plan-review bundle has no public terminal")?;
        if let Some(last) = outbox.last() {
            state.sequence = last.sequence;
        }
        state.terminal = true;
        for entry in outbox {
            let delivered = self.events.deliver_committed(
                &mut state,
                self.handler,
                entry.event,
                &entry.public_event_id,
            );
            if entry.public_event_id == terminal_id {
                state.terminal_delivered = delivered;
            }
        }
        Ok(true)
    }
}

impl<H> EventHandler for PublicApplicationEventBridge<'_, H>
where
    H: ApplicationRunEventHandler,
{
    fn diagnostic_run_id(&self) -> Option<&str> {
        Some(&self.events.run_id)
    }

    fn begin_live_attempt(&mut self, physical_attempt_id: &str) -> Result<()> {
        self.events.live_preview.begin_attempt(physical_attempt_id)
    }

    fn handle(&mut self, event: RunEvent) -> Result<()> {
        let RunEvent::Control(control) = event else {
            return self.emit(event.into());
        };
        let task_events = self
            .task_events
            .project_control(&control)
            .map_err(|source| anyhow::Error::new(ApplicationPublicOutboxAppendError { source }))?;
        if task_events.is_empty() {
            return self.emit(PublicRunEventKind::Control {
                control: control.into(),
            });
        }
        for event in task_events {
            self.emit(event)?;
        }
        Ok(())
    }

    fn commit_controls(
        &mut self,
        session: &mut Session,
        controls: Vec<ControlEntry>,
    ) -> Result<Vec<sigil_kernel::StoredEvent>> {
        self.events
            .ensure_pending_replayed_before_live(self.handler)?;
        if session.session_scope_id() != self.events.session_id {
            bail!("application public control belongs to another durable session");
        }
        if session.store_path() != Some(self.events.outbox_store.path()) {
            bail!("application public control uses a different durable session store");
        }
        let mut state = self
            .events
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        let next_sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        let mut staged_task_events = self.task_events.clone();
        controls
            .iter()
            .try_for_each(|control| staged_task_events.project_control(control).map(|_| ()))
            .map_err(|source| anyhow::Error::new(ApplicationPublicOutboxAppendError { source }))?;
        let (events, outbox) = session
            .append_controls_with_public_outbox(controls, &self.events.run_id, next_sequence)
            .map_err(|source| anyhow::Error::new(ApplicationPublicOutboxAppendError { source }))?;
        self.task_events = staged_task_events;
        if let Some(last) = outbox.last() {
            state.sequence = last.sequence;
        }
        for entry in outbox {
            self.events.deliver_committed(
                &mut state,
                self.handler,
                entry.event,
                &entry.public_event_id,
            );
        }
        Ok(events)
    }

    fn commit_session_publications(
        &mut self,
        session: &mut Session,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
    ) -> Result<Vec<sigil_kernel::StoredEvent>> {
        self.events
            .ensure_pending_replayed_before_live(self.handler)?;
        if session.session_scope_id() != self.events.session_id {
            bail!("application public session publication belongs to another durable session");
        }
        if session.store_path() != Some(self.events.outbox_store.path()) {
            bail!("application public session publication uses a different durable session store");
        }

        // This mixed-bundle API only accepts explicitly projected provider-visible entries plus
        // private companion controls. A control with its own public projection must use
        // `commit_controls`; accepting it here would recreate a split domain/outbox append.
        let mut staged_task_events = self.task_events.clone();
        for entry in &entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            let projected = staged_task_events
                .project_control(control)
                .map_err(|source| {
                    anyhow::Error::new(ApplicationPublicOutboxAppendError { source })
                })?;
            if !projected.is_empty() {
                bail!(
                    "public controls must be committed through the atomic control publication boundary"
                );
            }
        }

        let mut state = self
            .events
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        let next_sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        let (events, outbox) = session
            .append_session_entries_with_public_outbox(
                entries,
                publications,
                &self.events.run_id,
                next_sequence,
            )
            .map_err(|source| anyhow::Error::new(ApplicationPublicOutboxAppendError { source }))?;
        self.task_events = staged_task_events;
        if let Some(last) = outbox.last() {
            state.sequence = last.sequence;
        }
        for entry in outbox {
            self.events.deliver_committed(
                &mut state,
                self.handler,
                entry.event,
                &entry.public_event_id,
            );
        }
        Ok(events)
    }

    fn prepare_provider_output_publication(
        &mut self,
        session: &Session,
        control: &ControlEntry,
    ) -> Result<Option<ProviderOutputPublicationIntentV1>> {
        self.events
            .ensure_pending_replayed_before_live(self.handler)?;
        if session.session_scope_id() != self.events.session_id {
            bail!("application provider output belongs to another durable session");
        }
        if session.store_path() != Some(self.events.outbox_store.path()) {
            bail!("application provider output uses a different durable session store");
        }
        match control {
            ControlEntry::UsageSnapshot(usage) => {
                let state = self
                    .events
                    .state
                    .lock()
                    .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
                if state.terminal {
                    bail!("application run event stream is already terminal");
                }
                let next_sequence = state
                    .sequence
                    .checked_add(1)
                    .context("application run event sequence exhausted")?;
                Ok(Some(ProviderOutputPublicationIntentV1::public(
                    self.events.run_id.clone(),
                    next_sequence,
                    SessionPublicEventProjectionV1::usage_snapshot(0, usage.clone()),
                )?))
            }
            ControlEntry::ResponseHandleTracked(_) | ControlEntry::BackgroundTaskTracked(_) => {
                Ok(Some(ProviderOutputPublicationIntentV1::private()))
            }
            _ => bail!("unsupported provider-attempt output control"),
        }
    }

    fn complete_provider_output_publication(
        &mut self,
        intent: Option<ProviderOutputPublicationIntentV1>,
        committed: Vec<PublicEventOutboxEntryV1>,
        event: RunEvent,
    ) -> Result<()> {
        let Some(intent) = intent else {
            return self.handle(event);
        };
        if !intent.is_public() {
            if !committed.is_empty() {
                bail!("private provider output unexpectedly created a public outbox entry");
            }
            return Ok(());
        }
        if committed.len() != 1 {
            bail!("public provider output did not commit exactly one public outbox entry");
        }
        let entry = committed.into_iter().next().expect("length checked");
        let mut state = self
            .events
            .state
            .lock()
            .map_err(|_| anyhow!("application run event sequence is unavailable"))?;
        if state.terminal {
            bail!("application run event stream is already terminal");
        }
        let expected = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        if entry.run_id != self.events.run_id || entry.sequence != expected {
            bail!("committed provider output does not match the application public frontier");
        }
        state.sequence = entry.sequence;
        self.events.deliver_committed(
            &mut state,
            self.handler,
            entry.event,
            &entry.public_event_id,
        );
        Ok(())
    }
}

impl<H> ApplicationRunEventHandler for PublicApplicationEventBridge<'_, H>
where
    H: ApplicationRunEventHandler,
{
    fn bind_live_preview_source(&mut self, source: RuntimeLivePreviewSource) -> Result<()> {
        self.handler.bind_live_preview_source(source)
    }

    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        self.handler.handle_public_event(event)
    }

    fn handle_live_update(&mut self, update: sigil_application::LiveRunUpdate) -> Result<()> {
        self.handler.handle_live_update(update)
    }

    fn public_event_adapter_id(&self) -> &'static str {
        self.handler.public_event_adapter_id()
    }

    fn commit_plan_review_terminal(
        &mut self,
        session: &mut Session,
        entries: Vec<SessionLogEntry>,
        publications: Vec<SessionPublicEventProjectionV1>,
        terminal: ConversationRunFinalizedEntryV1,
        terminal_event: PublicRunEventKind,
    ) -> Result<bool> {
        self.commit_plan_review_terminal_bundle(
            session,
            entries,
            publications,
            terminal,
            terminal_event,
        )
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
    match &output.disposition {
        AgentRunDisposition::FinalAnswer => (
            ApplicationRunTerminalStatus::Succeeded,
            PublicRunEventKind::RunFinished {
                final_text: output.result.final_text.clone(),
            },
        ),
        AgentRunDisposition::AwaitingUserInput(request) => (
            ApplicationRunTerminalStatus::AwaitingUserInput,
            PublicRunEventKind::RunAwaitingUserInput {
                request_id: request.identity.request_id.as_str().to_owned(),
                generation: request.identity.generation,
                request_hash: request.request_hash.clone(),
            },
        ),
        AgentRunDisposition::Interrupted => (
            ApplicationRunTerminalStatus::Interrupted,
            PublicRunEventKind::RunInterrupted {
                reason: "run interrupted after reaching the configured turn limit".to_owned(),
            },
        ),
        AgentRunDisposition::Blocked => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: match output.outcome.terminal_reason {
                    AgentRunTerminalReason::DelegationUnsatisfied => {
                        "run blocked because its required delegation was not satisfied"
                    }
                    AgentRunTerminalReason::TaskHandoff => {
                        "run is waiting for its durable task to complete"
                    }
                    AgentRunTerminalReason::FinalAnswerBlocked => {
                        "run final answer was blocked by completion requirements"
                    }
                    AgentRunTerminalReason::RepairReplanRequired => {
                        "run requires repair before completion"
                    }
                    _ => "run is blocked from completing",
                }
                .to_owned(),
            },
        ),
        AgentRunDisposition::StartDurableTask(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: "run requested a durable task handoff, but this application surface has not attached the task executor"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::ContinueDurableTask(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: "run requested a durable task continuation, but this application surface has not attached the task executor"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::RunPendingPlan(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: "run requested pending plan execution, but this application surface has not attached the task executor"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::StartPlanReview(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: "run requested a plan review, but this application surface has not attached the plan review coordinator"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::PlanReviewDraftSubmitted(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: "plan review draft submitted outside an attached plan review coordinator".to_owned(),
            },
        ),
    }
}

fn application_provider_recovery_terminal(
    recovery: &sigil_kernel::ProviderTurnRecoveryTerminalError,
    summary: &str,
) -> (ApplicationRunTerminalStatus, PublicRunEventKind) {
    match recovery.disposition {
        sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Blocked => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunBlocked {
                reason: summary.to_owned(),
            },
        ),
        sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Paused => (
            ApplicationRunTerminalStatus::Paused,
            PublicRunEventKind::RunPaused {
                reason: summary.to_owned(),
            },
        ),
        sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Cancelled => (
            ApplicationRunTerminalStatus::Cancelled,
            PublicRunEventKind::RunCancelled,
        ),
        sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Irrecoverable => (
            ApplicationRunTerminalStatus::Failed,
            PublicRunEventKind::RunFailed {
                error: summary.to_owned(),
            },
        ),
    }
}

fn emit_application_conversation_terminal<H>(
    recorder: &ConversationRunLifecycleRecorder,
    events: &ApplicationRunEventSequence,
    handler: &mut H,
    run_id: &str,
    terminal_status: ApplicationRunTerminalStatus,
    final_message_id: Option<String>,
    summary: Option<&str>,
    redactor: &sigil_kernel::SecretRedactor,
    event: PublicRunEventKind,
) -> Result<()>
where
    H: ApplicationRunEventHandler,
{
    let terminal = ConversationRunFinalizedEntryV1::new(
        run_id,
        terminal_status,
        final_message_id,
        summary,
        current_unix_time_ms(),
        redactor,
    )?;
    events.emit_terminal(recorder, handler, &terminal, event)
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

#[cfg(test)]
#[path = "tests/application_task_terminal_tests.rs"]
mod application_task_terminal_tests;
