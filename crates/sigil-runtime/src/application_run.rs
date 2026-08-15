use std::{
    collections::{BTreeMap, BTreeSet},
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
    ConversationRunLifecycleRecorder, ConversationRunStartedEntryV1,
    ConversationRunTerminalStatusV1, EgressDisclosurePresenter, EventHandler,
    FrozenProviderRequestMaterial, InteractionMode, JsonlSessionStore, McpServerStartup,
    MessageRole, ModelMessage, ModelRef, MutationEventRecorder, NoopEventHandler, PermissionMode,
    PublicRunEvent, PublicRunEventKind, PublicTaskEventProjector, ReasoningEffort,
    ResolvedModelRoute, RootConfig, RunCancellationFinalizedEntry, RunCancellationHandle,
    RunCancellationOwner, RunCancellationRecorder, RunCancellationRequestedEntry,
    RunCancellationTarget, RunCancellationTerminalOutcome, RunEvent, RunQuiescenceOutcome,
    RunTaskGuard, SecretString, Session, SessionLogEntry, SessionRef, TaskId, TaskPauseRequest,
    TaskRunStatus, TaskVerificationRerunRequest, ToolRegistryScope, VerificationProductView,
    WorkspaceTrust, conversation_route_routing_contract_material, rerun_task_verification_check,
    resolve_workspace_root, safe_persistence_text, verification_product_view,
    workspace_trust_from_entries,
};

use crate::{
    activate_eager_remote_mcp_server,
    application_queue::{ApplicationQueuedRunPrepareError, PreparedApplicationQueuedRunInput},
    attach_remote_mcp_activation_presenter, attach_session_url_capability_store,
    context_candidates_from_safe_sources, current_unix_time_ms,
    product_view::{ApplicationAgentActivityView, agent_activity_product_view_from_entries},
    resolve_sigil_paths, secret_redactor_for_root_config, unsupported_mcp_elicitation_handler,
    unsupported_mcp_runtime_event_handler,
};

mod integration_control;
mod task_control;
mod user_input;

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
};

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
    /// Source used to resolve the effective context window.
    pub context_window_source: crate::ContextWindowSource,
    /// Bounded command, skill, and agent metadata for application clients.
    pub extension_catalog: crate::ApplicationExtensionCatalogView,
    /// Exact-bound route recovery state; transcript and catalog reads remain available.
    pub route_recovery: Option<ApplicationSessionRouteRecoveryView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSessionRouteRecoveryCode {
    SessionRouteConfirmationRequired,
    SessionRouteSelectionRequired,
    ModelRouteNotConfigured,
    ConnectionConfigInvalid,
    ProviderUnavailable,
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
    /// Resolved Sigil config path.
    pub config_path: PathBuf,
    /// Process launch working directory.
    pub launch_cwd: PathBuf,
    /// User prompt.
    pub prompt: String,
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
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            prompt: prompt.into(),
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
                    "supplied session attachment belongs to another durable session"
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

/// Shared dependencies used while preparing application runs.
#[derive(Clone)]
pub struct ApplicationRunServices {
    disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
    session_leases: Arc<ApplicationSessionLeaseManager>,
    supervisor_instance_id: Arc<str>,
    task_role_provider_builder:
        Option<Arc<dyn crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder>>,
    terminal_lifecycle_handler: Option<Arc<dyn crate::ApplicationTerminalLifecycleHandler>>,
    /// RFC-0062 14.1: process-scoped scratch lease registry shared by every run surface so
    /// tool/terminal leases, session-delete cleanup and TTL GC observe the same authority.
    scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
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
        owner: sigil_tools_builtin::TerminalTaskControlHandle,
        session_lease: &ApplicationSessionLease,
        session_scope_id: &str,
    ) -> Result<Self> {
        let route_execution_owner = session_lease
            .route_mutation_authority(session_scope_id)?
            .acquire_execution_owner()
            .map_err(anyhow::Error::new)?;
        Ok(Self {
            workspace_root,
            owner,
            _session_attachment: Arc::clone(&session_lease.attachment),
            _route_execution_owner: Arc::new(route_execution_owner),
        })
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
            disclosure_presenter,
            session_leases: Arc::new(ApplicationSessionLeaseManager::new()),
            supervisor_instance_id: Arc::from(format!("runtime-{}", uuid::Uuid::new_v4())),
            task_role_provider_builder: None,
            terminal_lifecycle_handler: None,
            scratch_control: None,
        }
    }

    /// Creates shared run services with an injected session lease manager.
    #[must_use]
    pub fn with_session_leases(
        disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
        session_leases: Arc<ApplicationSessionLeaseManager>,
    ) -> Self {
        Self {
            disclosure_presenter,
            session_leases,
            supervisor_instance_id: Arc::from(format!("runtime-{}", uuid::Uuid::new_v4())),
            task_role_provider_builder: None,
            terminal_lifecycle_handler: None,
            scratch_control: None,
        }
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
}

/// Sink for ordered provider-neutral application events.
pub trait ApplicationRunEventHandler {
    /// Handles one public event.
    ///
    /// # Errors
    ///
    /// Returns an error when the adapter cannot accept the event and execution should stop.
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()>;
}

/// Prepared application run and its root cancellation authority.
pub struct PreparedApplicationRun {
    execution: ApplicationRunExecution,
    control: ApplicationRunControl,
    terminal_control: ApplicationTerminalTaskControl,
}

impl PreparedApplicationRun {
    /// Returns the typed persistent-terminal owner retained beyond the foreground model turn.
    #[must_use]
    pub fn terminal_control(&self) -> ApplicationTerminalTaskControl {
        self.terminal_control.clone()
    }

    /// Separates the execution payload from its root cancellation authority.
    ///
    /// The caller must keep `control` alive until the execution reaches a terminal state.
    #[must_use]
    pub fn into_parts(self) -> (ApplicationRunExecution, ApplicationRunControl) {
        (self.execution, self.control)
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

        self.execution
            .conversation_coordinator
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
        let queued_input = self
            .execution
            .conversation_coordinator
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
                ApplicationQueuedRunPrepareError::promotion_commit("task_handoff_binding", source)
            })?;
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
            let _ = self
                .owner
                .wait_for_quiescence(ticket.remaining_timeout())
                .await;
            let task_stop = self.append_related_task_stop_state(
                crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
                "application Task cancellation request could not be durably audited",
            );
            let conversation_terminal = if conversation_start.is_ok() {
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    self.conversation_start.run_id(),
                    ConversationRunTerminalStatusV1::Interrupted,
                    None,
                    Some("cancellation request could not be durably audited"),
                    &sigil_kernel::SecretRedactor::empty(),
                )
            } else {
                Ok(())
            };
            if let Ok(Some(task_stop)) = task_stop.as_ref() {
                self.emit_task_stop_state(handler, task_stop)?;
            }
            self.events.emit(
                handler,
                PublicRunEventKind::RunFailed {
                    error: "run interrupted because its cancellation request could not be audited"
                        .to_owned(),
                },
            )?;
            task_stop?;
            conversation_terminal?;
            conversation_start?;
            bail!("application cancellation request was not durably recorded");
        }
        let outcome = self
            .finalize_recorded_cancellation(ticket, execution_joined, conversation_start)
            .await?;
        let task_stop = self.append_related_task_stop_state(
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
        let (conversation_status, conversation_summary) = match outcome {
            RunCancellationTerminalOutcome::Cancelled => (
                ConversationRunTerminalStatusV1::Cancelled,
                "cancellation quiescence confirmed",
            ),
            RunCancellationTerminalOutcome::Interrupted => (
                ConversationRunTerminalStatusV1::Interrupted,
                "cancellation cleanup could not be confirmed",
            ),
        };
        append_application_conversation_terminal(
            &self.conversation_lifecycle,
            self.conversation_start.run_id(),
            conversation_status,
            None,
            Some(conversation_summary),
            &sigil_kernel::SecretRedactor::empty(),
        )?;
        if let Some(task_stop) = task_stop.as_ref() {
            self.emit_task_stop_state(handler, task_stop)?;
        }
        let terminal = match outcome {
            RunCancellationTerminalOutcome::Cancelled => PublicRunEventKind::RunCancelled,
            RunCancellationTerminalOutcome::Interrupted => PublicRunEventKind::RunFailed {
                error: "run interrupted before cancellation cleanup could be confirmed".to_owned(),
            },
        };
        self.events.emit(handler, terminal)?;
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
            let _ = self
                .owner
                .wait_for_quiescence(cancellation.remaining_timeout())
                .await;
            let task_stop = self.load_control_session().and_then(|mut session| {
                crate::agent_supervisor::task_execution::append_task_stop_state(
                    &mut session,
                    Some(&request.task_id),
                    crate::agent_supervisor::task_execution::TaskStopDisposition::Interrupted,
                    "application Task pause request could not be durably audited",
                )
                .map_err(Into::into)
            });
            let conversation_terminal = if conversation_start.is_ok() {
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    self.conversation_start.run_id(),
                    ConversationRunTerminalStatusV1::Interrupted,
                    None,
                    Some("Task pause request could not be durably audited"),
                    &sigil_kernel::SecretRedactor::empty(),
                )
            } else {
                Ok(())
            };
            if let Ok(Some(task_stop)) = task_stop.as_ref() {
                self.emit_task_stop_state(handler, task_stop)?;
            }
            self.events.emit(
                handler,
                PublicRunEventKind::RunFailed {
                    error: "Task interrupted because its pause request could not be audited"
                        .to_owned(),
                },
            )?;
            task_stop?;
            conversation_terminal?;
            conversation_start?;
            bail!("application Task pause request was not durably recorded");
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
        let task_stop = crate::agent_supervisor::task_execution::append_task_stop_state(
            &mut session,
            Some(&request.task_id),
            disposition,
            &reason,
        )?
        .context("exact application Task was not available during pause finalization")?;
        let task_status = task_stop.status();
        let (conversation_status, terminal_summary, terminal) = match task_status {
            TaskRunStatus::Paused => (
                ConversationRunTerminalStatusV1::Blocked,
                "Task paused",
                PublicRunEventKind::RunCancelled,
            ),
            TaskRunStatus::Interrupted => (
                ConversationRunTerminalStatusV1::Interrupted,
                "Task pause could not be confirmed",
                PublicRunEventKind::RunFailed {
                    error: "Task interrupted before pause cleanup could be confirmed".to_owned(),
                },
            ),
            _ => bail!("application Task pause wrote an invalid terminal status"),
        };
        append_application_conversation_terminal(
            &self.conversation_lifecycle,
            self.conversation_start.run_id(),
            conversation_status,
            None,
            Some(terminal_summary),
            &sigil_kernel::SecretRedactor::empty(),
        )?;
        self.emit_task_stop_state(handler, &task_stop)?;
        self.events.emit(handler, terminal)?;
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
        Ok(outcome)
    }

    fn append_related_task_stop_state(
        &self,
        disposition: crate::agent_supervisor::task_execution::TaskStopDisposition,
        reason: &str,
    ) -> Result<Option<crate::agent_supervisor::task_execution::AppendedTaskStopState>> {
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
        crate::agent_supervisor::task_execution::append_task_stop_state(
            &mut session,
            Some(&task_id),
            disposition,
            reason,
        )
        .map_err(Into::into)
    }

    fn load_control_session(&self) -> Result<Session> {
        let entries =
            application_bound_session_entries(&self._session_lease.path, &self.events.session_id)?;
        let (provider_name, model_name) = application_session_identity(&entries)
            .context("application control session has no durable provider/model identity")?;
        let session = Session::load_from_store(
            provider_name,
            model_name,
            JsonlSessionStore::new(&self._session_lease.path)?,
        )?;
        if session.session_scope_id() != self.events.session_id {
            bail!("application control session identity changed");
        }
        Ok(session)
    }

    fn emit_task_stop_state<H>(
        &self,
        handler: &mut H,
        task_stop: &crate::agent_supervisor::task_execution::AppendedTaskStopState,
    ) -> Result<()>
    where
        H: ApplicationRunEventHandler,
    {
        let mut projector = PublicTaskEventProjector::default();
        for control in task_stop.controls() {
            for event in projector.project_control(control) {
                self.events.emit(handler, event)?;
            }
        }
        self.events.emit(
            handler,
            PublicRunEventKind::TaskRunFinished {
                task_id: task_stop.task_id().as_str().to_owned(),
                status: application_task_run_status_label(task_stop.status()).to_owned(),
            },
        )
    }
}

/// Durable cancellation request retained until cleanup reaches a terminal observation.
#[derive(Debug)]
pub struct ApplicationCancellationTicket {
    request: RunCancellationRequestedEntry,
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
    conversation_coordinator: crate::ConversationCoordinator,
    parent_session_ref: SessionRef,
    pending_session_title: Option<ApplicationSessionTitleRequest>,
    pending_user_input_continuation: Option<user_input::ApplicationUserInputContinuationContext>,
    route_transition: crate::provider_connections::SessionRouteTransitionView,
    _session_lease: Arc<ApplicationSessionLease>,
}

#[derive(Debug, Clone)]
struct ApplicationSessionTitleRequest {
    root_config: RootConfig,
    workspace_root: PathBuf,
    model_ref: ModelRef,
    session_log_path: PathBuf,
    session_id: String,
    prompt: String,
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
            }),
        }
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
            )
            .await?;
        }
        Ok(())
    }
}

struct ApplicationTaskExecutionRuntime {
    root_config: RootConfig,
    options: AgentRunOptions,
    base_registry: sigil_kernel::ToolRegistry,
    agent_supervisor: crate::AgentSupervisor,
    role_provider_builder:
        Arc<dyn crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder>,
}

/// Runtime facts used to execute a read-only plan review after an automatic route decision.
struct ApplicationPlanReviewRuntime {
    options: AgentRunOptions,
    agent: Box<Agent<Box<dyn sigil_kernel::Provider>>>,
    tool_registry: sigil_kernel::ToolRegistry,
    workspace_snapshot_id: Option<String>,
}

enum ApplicationRunExecutionKind {
    Main {
        agent: Box<Agent<Box<dyn sigil_kernel::Provider>>>,
        input: Box<AgentRunInput>,
    },
    AgentProfile {
        runtime: Box<crate::AgentToolRuntime>,
        profile_id: AgentProfileId,
    },
    ExplicitPlanReview {
        request: Box<crate::PlanReviewRunRequest>,
    },
}

/// Provider-neutral terminal classification for one completed application run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationRunTerminalStatus {
    /// A final assistant answer was accepted.
    Succeeded,
    /// The configured turn bound was reached without a final answer.
    Interrupted,
    /// A required delegation contract was not satisfied.
    Blocked,
    /// The physical run stopped safely after durably requesting user input.
    AwaitingUserInput,
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
        validate_execution_contract(self.interaction, approval_handler, false)?;
        self.execute_inner(handler, approval_handler).await
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
        validate_execution_contract(self.interaction, &approval_handler, true)?;
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            runtime.block_on(self.execute_inner(&mut handler, &mut approval_handler))
        })
        .await
        .context("application run owned blocking worker failed")?
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
        let mut bridge = PublicApplicationEventBridge::new(self.events.clone(), handler);
        if let Err(error) = bridge.emit(PublicRunEventKind::RunStarted {
            prompt: self.prompt.clone(),
        }) {
            let safe_error = self.redactor.redact_text(&format!("{error:#}"));
            append_application_conversation_terminal(
                &self.conversation_lifecycle,
                &self.run_id,
                ConversationRunTerminalStatusV1::Failed,
                None,
                Some(&safe_error),
                &self.redactor,
            )?;
            return Err(error).context("application run start event delivery failed");
        }
        bridge.emit(PublicRunEventKind::RouteTransition {
            transition: application_public_route_transition(&self.route_transition),
        })?;
        for warning in std::mem::take(&mut self.warnings) {
            if let Err(error) = bridge.emit(PublicRunEventKind::Notice { message: warning }) {
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ConversationRunTerminalStatusV1::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
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
                    append_application_conversation_terminal(
                        &self.conversation_lifecycle,
                        &self.run_id,
                        ConversationRunTerminalStatusV1::Failed,
                        None,
                        Some(&safe_error),
                        &self.redactor,
                    )?;
                    bridge.emit(PublicRunEventKind::RunFailed { error: safe_error })?;
                    return Err(error).context("failed to start user-input continuation");
                }
            };
            if let Err(error) =
                bridge.emit(user_input::application_user_input_changed_event(request))
            {
                let resolution = user_input::reconcile_failed_application_user_input_continuation(
                    &mut self.session,
                    context,
                );
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ConversationRunTerminalStatusV1::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
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
            ApplicationRunExecutionKind::Main { agent, input } => {
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
        match run {
            Ok(agent_output) => {
                let (terminal_status, terminal_event) =
                    application_terminal_projection(&agent_output);
                let durable_status = match terminal_status {
                    ApplicationRunTerminalStatus::Succeeded => {
                        ConversationRunTerminalStatusV1::Succeeded
                    }
                    ApplicationRunTerminalStatus::Interrupted => {
                        ConversationRunTerminalStatusV1::Interrupted
                    }
                    ApplicationRunTerminalStatus::Blocked => {
                        ConversationRunTerminalStatusV1::Blocked
                    }
                    ApplicationRunTerminalStatus::AwaitingUserInput => {
                        ConversationRunTerminalStatusV1::AwaitingUserInput
                    }
                };
                let summary = match &terminal_event {
                    PublicRunEventKind::RunFailed { error } => Some(error.as_str()),
                    _ => None,
                };
                let final_message_id = (terminal_status == ApplicationRunTerminalStatus::Succeeded)
                    .then(|| agent_output.result.final_message_id.clone())
                    .flatten();
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    durable_status,
                    final_message_id,
                    summary,
                    &self.redactor,
                )?;
                bridge.emit(terminal_event)?;
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
            Err(error) if self.cancellation_handle.is_cancel_requested() => Err(error)
                .context("application run cancellation is pending terminal cleanup confirmation"),
            Err(error) => {
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ConversationRunTerminalStatusV1::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
                )?;
                bridge.emit(PublicRunEventKind::RunFailed { error: safe_error })?;
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
    if let AgentRunDisposition::ContinueDurableTask(action) = output.disposition.clone() {
        return continue_application_existing_task(
            session,
            output,
            *action,
            task_execution,
            handler,
            approval_handler,
            cancellation_handle,
        )
        .await;
    }
    let AgentRunDisposition::StartDurableTask(action) = output.disposition.clone() else {
        return Ok(output);
    };
    let Some(task_execution) = task_execution else {
        return Ok(output);
    };
    let task = session
        .task_state_projection()
        .tasks
        .get(&action.task_id)
        .cloned();
    let Some(task) = task else {
        if !cancellation_handle.is_naturally_finalized()
            && !cancellation_handle.try_finalize_naturally()
        {
            bail!("run cancellation won the missing-task terminal-state race");
        }
        bail!(
            "accepted task handoff {} is missing its durable task",
            action.task_id.as_str()
        );
    };
    let ApplicationTaskExecutionRuntime {
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
    } = task_execution;
    let status = crate::agent_supervisor::task_execution::run_admitted_task_to_root_terminal(
        session,
        crate::agent_supervisor::task_execution::AdmittedTaskExecution {
            task_id: action.task_id.clone(),
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
        },
        approval_handler,
    )
    .await?;
    application_task_terminal_output(session, &action.task_id, status, output)
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
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
    } = task_execution;
    let result = crate::agent_supervisor::task_execution::bind_task_run_cancellation_scope(
        session,
        &action.task_id,
        cancellation_handle,
    );
    let continuation_entry_frontier = session.entries().len();
    let result = match result {
        Ok(()) => {
            crate::agent_supervisor::task_execution::continue_task_execution(
                session,
                crate::agent_supervisor::task_execution::ContinuedTaskExecution {
                    requested_task_id: Some(action.task_id.clone()),
                    guidance: Some(action.guidance.expose_secret().to_owned()),
                    guidance_promotion: None,
                    continuation_guidance_receipt: Some(action.guidance_receipt),
                    root_config,
                    options,
                    base_registry,
                    agent_supervisor,
                    role_provider_builder: role_provider_builder.as_ref(),
                    handler,
                    cancellation_handle: cancellation_handle.clone(),
                    tool_artifact_read_budget: None,
                },
                approval_handler,
            )
            .await
        }
        Err(error) => Err(error),
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
) -> Result<AgentRunOutput>
where
    H: EventHandler + Send,
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
) -> Result<AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let ApplicationPlanReviewRuntime {
        options,
        agent,
        tool_registry,
        ..
    } = runtime;
    emit_current_plan_review_attempt(session, &request, handler)?;
    let outcome = match crate::PlanReviewCoordinator::run_plan_review(
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
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let close = crate::PlanReviewCoordinator::close_plan_review_run_if_open(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Failed(
                    "plan review run failed before an outcome".to_owned(),
                ),
                current_unix_time_ms(),
            );
            if let Err(close_error) = close {
                bail!(
                    "plan review run failed ({error:#}) and its terminal closure also failed ({close_error:#})"
                );
            }
            emit_current_plan_review_attempt(session, &request, handler)?;
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
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
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
            crate::PlanReviewCoordinator::commit_draft_from_child(
                session,
                &draft,
                &request,
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
            let final_text = format!("Plan ready: {}", draft.summary);
            let final_message_id =
                append_application_final_answer(session, handler, final_text.clone())?;
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
            crate::PlanReviewCoordinator::complete_without_draft(
                session,
                &request,
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
            let final_text =
                "Plan review closed without a draft; no task was created. Send a more specific request or use /plan with explicit steps."
                    .to_owned();
            let final_message_id =
                append_application_final_answer(session, handler, final_text.clone())?;
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
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
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
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
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
        crate::PlanReviewRunOutcome::Failed(error) => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::Failed(error.clone()),
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
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
        crate::PlanReviewRunOutcome::SubmitOnlyProtocolViolation(error) => {
            crate::PlanReviewCoordinator::close_plan_review_run(
                session,
                &request,
                &crate::PlanReviewRunOutcome::SubmitOnlyProtocolViolation(error.clone()),
                current_unix_time_ms(),
            )?;
            emit_current_plan_review_attempt(session, &request, handler)?;
            if !cancellation_handle.is_naturally_finalized()
                && !cancellation_handle.try_finalize_naturally()
            {
                bail!("run cancellation won the plan review protocol terminal-state race");
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
            .context(format!(
                "plan review submit-only protocol violation: {error}"
            ))
        }
    }
}

fn emit_current_plan_review_attempt(
    session: &Session,
    request: &crate::PlanReviewRunRequest,
    handler: &mut (impl EventHandler + ?Sized),
) -> Result<()> {
    let attempt = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.plan_review_id == request.plan_review_id
                    && attempt.attempt_id == request.attempt_id =>
            {
                Some(attempt.clone())
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("plan review transition has no durable attempt"))?;
    handler.handle(RunEvent::Control(ControlEntry::PlanReviewAttempt(attempt)))
}

fn append_application_final_answer(
    session: &mut Session,
    handler: &mut (impl EventHandler + ?Sized),
    text: String,
) -> Result<String> {
    let mut message = ModelMessage::assistant(Some(safe_persistence_text(&text)), Vec::new());
    message.assistant_kind = Some(AssistantMessageKind::FinalAnswer);
    let final_message_id = message.id.clone();
    session.append_assistant_message(message.clone())?;
    handler.handle(RunEvent::AssistantMessage(message))?;
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
    let committed = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .and_then(|task| task.final_answer.clone())
        .ok_or_else(|| anyhow!("completed application task has no committed final answer"))?;
    let message = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Assistant(message) if message.id == committed.message_id => {
                Some(message)
            }
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
    Ok(ApplicationTaskFinalAnswer {
        message_id: committed.message_id,
        text,
    })
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
        TaskRunStatus::Started
        | TaskRunStatus::Running
        | TaskRunStatus::Paused
        | TaskRunStatus::Failed => {
            output.result.final_text.clear();
            output.result.final_message_id = None;
            output.disposition = AgentRunDisposition::Blocked;
            output.outcome.terminal_reason = AgentRunTerminalReason::DelegationUnsatisfied;
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
    session.append_assistant_message(message.clone())?;
    handler.handle(RunEvent::AssistantMessage(message))?;
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
    tool_scope: Option<&ToolRegistryScope>,
    terminal_lifecycle_sink: Arc<dyn sigil_kernel::TerminalLifecycleSink>,
) -> Result<(crate::RuntimeToolSurface, Vec<String>)> {
    let surface = crate::mcp_registry::build_tool_surface_with_terminal_lifecycle(
        root_config,
        provider_capabilities,
        workspace_root.to_path_buf(),
        mutation_recorder,
        workspace_trust,
        sigil_kernel::ExtensionProcessNetworkAdmission::new(
            options.permission_context.network_policy,
            false,
        ),
        terminal_lifecycle_sink,
        services.scratch_control().cloned(),
    )
    .await?;
    // RFC-0062 14.1: one TTL sweep over the workspace scratch namespaces per application run
    // assembly. Leases are in-memory only, so this fresh process cannot hold one; the sweep
    // reclaims namespaces abandoned by crashed or deleted sessions and never races a live tool.
    {
        let scratch_root =
            resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root)
                .scratch_root;
        let scratch_control = surface.scratch_control.clone();
        tokio::task::spawn_blocking(move || {
            match sigil_tools_builtin::gc_scratch_namespaces(
                &scratch_root,
                &scratch_control,
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
    let elicitation_handler = unsupported_mcp_elicitation_handler();
    let runtime_event_handler = unsupported_mcp_runtime_event_handler();
    attach_remote_mcp_activation_presenter(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root.to_path_buf(),
        Arc::clone(&elicitation_handler),
        runtime_event_handler,
        Arc::clone(&services.disclosure_presenter),
    );
    let eager_remote_servers = root_config
        .mcp_servers
        .iter()
        .filter(|server| {
            server.startup == McpServerStartup::Eager && server.streamable_http().is_some()
        })
        .map(|server| (server.name.clone(), server.required))
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();
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
    if request.prompt.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "prompt must not be empty".to_owned(),
        });
    }
    if request.run_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "run id must not be empty".to_owned(),
        });
    }
    let conversation_start =
        ConversationRunStartedEntryV1::new(request.run_id.clone(), current_unix_time_ms())
            .map_err(|error| ApplicationRunPrepareError::InvalidInvocation {
                message: safe_persistence_text(&error.to_string()),
            })?;
    let session_leases = Arc::clone(&services.session_leases);
    let task_executor_attached = services.task_role_provider_builder.is_some();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_application_run_blocking(request, session_leases, task_executor_attached)
    })
    .await
    .map_err(|error| ApplicationRunPrepareError::Internal {
        source: anyhow!(error).context("application run blocking preparation task failed"),
    })??;
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
        target_max_tokens,
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
    } = prepared;
    let provider = crate::build_provider_for_model_ref_async(&root_config, &model_ref)
        .await
        .map_err(ApplicationRunPrepareError::provider_unavailable)?;
    let orchestration_route_guard = crate::OrchestrationRouteGuard::new(
        session.provider_name(),
        session.model_name(),
        crate::ORCHESTRATION_RUNTIME_BUILD_ID,
    );
    if queued_first_request.is_none() {
        orchestration_route_guard
            .enforce(&mut session, current_unix_time_ms())
            .map_err(ApplicationRunPrepareError::execution)?;
    }
    orchestration_route_guard.apply_effective_task_config(&session, &mut root_config.task);
    let terminal_lifecycle_sink = Arc::new(crate::ApplicationTerminalLifecycleRouter::new(
        mutation_recorder.clone(),
        session.session_scope_id(),
        &run_id,
        services.terminal_lifecycle_handler.clone(),
    )) as Arc<dyn sigil_kernel::TerminalLifecycleSink>;
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
        tool_scope.as_ref(),
        terminal_lifecycle_sink,
    )
    .await
    .map_err(ApplicationRunPrepareError::execution)?;
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
    input = input.with_runtime_context(runtime_context.clone());
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
    let task_execution = task_agent_registry
        .zip(services.task_role_provider_builder.as_ref())
        .map(
            |(profile_registry, role_provider_builder)| ApplicationTaskExecutionRuntime {
                root_config: root_config.clone(),
                options: options.clone(),
                base_registry: registry.clone(),
                agent_supervisor: crate::AgentSupervisor::new(
                    profile_registry,
                    crate::AgentBudgetPolicy::from_root_config(&root_config),
                    provider.capabilities(),
                ),
                role_provider_builder: Arc::clone(role_provider_builder),
            },
        );
    let conversation_coordinator = crate::ConversationCoordinator::new(
        root_config.task.enabled,
        root_config.task.routing_policy,
    )
    .with_writable_memory_routing(writable_memory_available)
    .with_orchestration_route_guard(orchestration_route_guard)
    .with_route_capability_evidence(crate::RouteCapabilityEvidence {
        provider_supports_routing_tools: provider.capabilities().supports_tool_stream,
        // DirectTask additionally requires an attached task executor; without one the route
        // stays at the ReviewFirst baseline so plan review remains usable.
        route_qualified: crate::route_qualification_evidence(&root_config)
            && task_execution.is_some(),
    });
    if queued_first_request.is_none() && agent_invocation.is_none() {
        conversation_coordinator
            .enforce_orchestration_route_kill_switch(&mut session, current_unix_time_ms())
            .map_err(ApplicationRunPrepareError::execution)?;
        input = conversation_coordinator
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
            let route_capability = conversation_coordinator.resolve_route_capability(&session);
            let automatic_routing = route_capability.routes_automatically();
            let tool_specs = if automatic_routing {
                conversation_coordinator.route_tool_specs_for_session(&session, route_capability)
            } else {
                registry.specs()
            };
            let mut transient_messages = vec![exact_user_message];
            if automatic_routing {
                transient_messages.insert(
                    0,
                    ModelMessage::system(conversation_route_routing_contract_material()),
                );
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
                .with_pending_input_provider(Arc::new(
                    crate::pending_input::DurableQueuePendingInputProvider,
                ));
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
    let session_id = session.session_scope_id().to_owned();
    let plan_review_workspace_snapshot_id =
        crate::plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
            .ok()
            .flatten();
    let explicit_plan_review = agent_invocation
        .as_ref()
        .is_some_and(|(_, profile_id)| profile_id.as_str() == "plan");
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
    let pending_session_title =
        (queued_first_request.is_none() && generate_session_title).then(|| {
            ApplicationSessionTitleRequest {
                root_config: root_config.clone(),
                workspace_root: workspace_root.clone(),
                model_ref: model_ref.clone(),
                session_log_path: session_path.clone(),
                session_id: session_id.clone(),
                prompt: prompt.clone(),
            }
        });
    let conversation_lifecycle = session
        .conversation_run_lifecycle_recorder()
        .map_err(ApplicationRunPrepareError::execution)?;
    let events = ApplicationRunEventSequence::new(session_id.clone(), run_id.clone());
    let kind = if let Some(request) = explicit_plan_review_request {
        ApplicationRunExecutionKind::ExplicitPlanReview {
            request: Box::new(request),
        }
    } else if let Some((registry_snapshot, profile_id)) = agent_invocation {
        let supervisor = crate::AgentSupervisor::new(
            registry_snapshot,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            provider.capabilities().clone(),
        );
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
        ApplicationRunExecutionKind::Main {
            agent: Box::new(Agent::new(provider, registry.clone())),
            input: Box::new(input),
        }
    };
    let prepared = PreparedApplicationRun {
        execution: ApplicationRunExecution {
            plan_review_runtime: Some(ApplicationPlanReviewRuntime {
                options: options.clone(),
                workspace_snapshot_id: plan_review_workspace_snapshot_id,
                agent: Box::new(Agent::new(
                    crate::build_provider_for_model_ref_async(&root_config, &model_ref)
                        .await
                        .map_err(ApplicationRunPrepareError::provider_unavailable)?,
                    crate::build_plan_review_tool_registry(&registry, &root_config).into_registry(),
                )),
                tool_registry: crate::build_plan_review_tool_registry(&registry, &root_config)
                    .into_registry(),
            }),
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
    let root_config = load_application_root_config(config_path)?;
    let (_, selected_route) =
        application_selected_model_route(&root_config, connection_id, model_name)?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    if connection_id.is_some()
        && !application_model_ref_is_selectable(
            &root_config,
            &selected_route.model_ref,
            &sigil_paths.cache_root,
        )
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: format!(
                "model {}/{} is not admitted by the exact connection catalog",
                selected_route.model_ref.connection_id, selected_route.model_ref.model_id
            ),
        });
    }
    let requested_path = session_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_application_session_path(&sigil_paths.session_log_dir));
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
    let outcome = crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment_transition(
            &root_config,
            &selected_route,
            store,
            None,
            None,
            Some(attachment.as_ref()),
        )
        .map_err(application_route_load_prepare_error)?;
    Ok((
        ApplicationSessionBinding {
            session_scope_id: outcome.session.session_scope_id().to_owned(),
            session_log_path: canonical_path,
            route_transition: outcome.transition,
        },
        attachment,
    ))
}

fn application_model_ref_is_selectable(
    root_config: &RootConfig,
    requested: &ModelRef,
    cache_root: &Path,
) -> bool {
    let loaded = crate::provider_connections::load_provider_connections(root_config);
    let Some(connection) = loaded.connections.get(&requested.connection_id) else {
        return false;
    };
    if let Some(cached) = crate::provider_connections::fresh_cached_model_entries_native(
        cache_root,
        root_config,
        &requested.connection_id,
    ) {
        return cached.iter().any(|entry| {
            entry.model_ref == *requested
                && entry.availability
                    != crate::provider_connections::ModelAvailability::ConfiguredUnavailable
        });
    }
    crate::provider_connections::bundled_model_entries(&connection.config)
        .iter()
        .any(|entry| entry.model_ref == *requested)
        || loaded.default_model.as_ref() == Some(requested)
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
        let cache_proved_absence = cached.is_some();
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
                    availability: if cache_proved_absence {
                        crate::provider_connections::ModelAvailability::ConfiguredUnavailable
                    } else {
                        crate::provider_connections::ModelAvailability::Unverified
                    },
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

fn application_model_selection_binding(
    current_model: &ModelRef,
    model_options: &[ApplicationModelOptionView],
) -> String {
    let mut material = format!(
        "sigil-application-model-selection-v3\n{}/{}\n",
        current_model.connection_id, current_model.model_id,
    );
    for option in model_options {
        use std::fmt::Write as _;
        let _ = writeln!(
            material,
            "{}/{}|{:?}|{:?}|{:?}",
            option.model_ref.connection_id,
            option.model_ref.model_id,
            option.availability,
            option.recommendation,
            option.provenance,
        );
    }
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
    let records = JsonlSessionStore::read_event_records(&canonical_path)
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
        .map_err(ApplicationRunPrepareError::execution)?;
    let outcome = crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment_transition(
        &root_config,
        &fallback_route,
        store,
        None,
        None,
        Some(attachment),
    )
    .map_err(application_route_load_prepare_error)?;
    Ok(ApplicationSessionBinding {
        session_scope_id: outcome.session.session_scope_id().to_owned(),
        session_log_path: read_binding.session_log_path,
        route_transition: outcome.transition,
    })
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
    RootConfig::load(config_path).map_err(ApplicationRunPrepareError::connection_config_invalid)
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
    crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
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
    if expected_session_scope_id.is_empty() {
        bail!("expected run-context session scope must not be empty");
    }
    let root_config = RootConfig::load(config_path)?;
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    let route = application_session_route(&entries).ok_or_else(|| {
        anyhow!(
            "session_route_missing: restore the referenced connection or fork with current route"
        )
    })?;
    let config_snapshot =
        crate::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(&root_config);
    let plan = crate::provider_connections::plan_session_route_resume(
        &config_snapshot,
        &crate::provider_connections::SessionRouteResumeInput {
            route: route.clone(),
            egress_trust_binding: application_session_route_trust_binding(&entries),
        },
    );
    let route_frontier_binding =
        crate::provider_connections::session_route_frontier_binding(&entries);
    let route_authority_generation_binding =
        crate::provider_connections::session_route_authority_generation_binding(&entries);
    let recovery_binding = config_snapshot.recovery_binding(
        expected_session_scope_id,
        &route,
        &route_frontier_binding,
        &route_authority_generation_binding,
    );
    let (provider_name, effective_route, route_recovery) = match plan {
        crate::provider_connections::SessionRouteResumePlan::Exact {
            provider_name,
            route,
        }
        | crate::provider_connections::SessionRouteResumePlan::RebindCurrentModel {
            provider_name,
            target_route: route,
            ..
        } => (provider_name, route, None),
        crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation {
            provider_name,
            target_route,
            ..
        } => (
            provider_name,
            target_route,
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::ConfirmCurrentRoute,
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::SelectReplacement,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding,
                retryable: true,
            }),
        ),
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement {
            reason: crate::provider_connections::SessionRouteUnavailableReason::ConnectionNotFound,
            ..
        } => (
            application_session_identity(&entries)
                .map(|(provider, _)| provider)
                .unwrap_or_else(|| "unavailable".to_owned()),
            route.clone(),
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::SessionRouteSelectionRequired,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::SelectReplacement,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding,
                retryable: true,
            }),
        ),
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement {
            reason:
                crate::provider_connections::SessionRouteUnavailableReason::ConnectionConfigInvalid,
            ..
        }
        | crate::provider_connections::SessionRouteResumePlan::NeedsSetup {
            reason: crate::provider_connections::ModelRouteSetupReason::ConfigurationInvalid,
        } => (
            application_session_identity(&entries)
                .map(|(provider, _)| provider)
                .unwrap_or_else(|| "unavailable".to_owned()),
            route.clone(),
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::ConnectionConfigInvalid,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::SelectReplacement,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding,
                retryable: false,
            }),
        ),
        crate::provider_connections::SessionRouteResumePlan::NeedsSetup {
            reason: crate::provider_connections::ModelRouteSetupReason::RouteNotConfigured,
        } => (
            application_session_identity(&entries)
                .map(|(provider, _)| provider)
                .unwrap_or_else(|| "unavailable".to_owned()),
            route.clone(),
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::ModelRouteNotConfigured,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding,
                retryable: false,
            }),
        ),
    };
    let model_name = effective_route.model_ref.model_id.clone();
    let resolved = crate::resolve_model_context_window_tokens(
        &root_config,
        &effective_route.model_ref,
        &provider_name,
    );
    let last_prompt_tokens = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) => {
                Some(usage.prompt_tokens)
            }
            _ => None,
        })
        .next_back();
    let available_reasoning_efforts = if route_recovery.as_ref().is_some_and(|recovery| {
        recovery.code != ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired
    }) {
        Vec::new()
    } else {
        crate::reasoning_effort::supported_reasoning_efforts(&provider_name, &model_name)
    };
    let mut identity_config = root_config.clone();
    identity_config.agent.runtime_provider = provider_name.clone();
    identity_config.agent.model = model_name.clone();
    let default_reasoning_effort =
        crate::reasoning_effort::configured_default_reasoning_effort(&identity_config);
    let reasoning_effort_binding = crate::reasoning_effort::reasoning_effort_binding(
        &provider_name,
        &model_name,
        &available_reasoning_efforts,
    );
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let extension_catalog =
        crate::application_extension_catalog_view(&root_config, &workspace_root, &entries)?;
    let catalog_entries =
        application_model_catalog_entries(&root_config, &route.model_ref, &sigil_paths.cache_root);
    let model_options = application_model_option_views(&root_config, catalog_entries);
    let model_selection_binding =
        application_model_selection_binding(&route.model_ref, &model_options);
    Ok(ApplicationRunContextView {
        model_ref: effective_route.model_ref,
        provider_name,
        model_name,
        model_options,
        model_selection_binding,
        default_permission_mode: root_config.permission.mode,
        available_reasoning_efforts,
        default_reasoning_effort,
        reasoning_effort_binding,
        context_window_tokens: resolved.tokens,
        last_prompt_tokens,
        context_window_source: resolved.source,
        extension_catalog,
        route_recovery,
    })
}

fn application_session_route_trust_binding(
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

fn application_session_route(entries: &[SessionLogEntry]) -> Option<ResolvedModelRoute> {
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
    sigil_kernel::ConversationQueueDurableProjection::from_records(&records)?;
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
    let records = JsonlSessionStore::read_event_records(session_path)?;
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
    Ok(records)
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
    if expected_session_scope_id.is_empty() {
        bail!("expected transcript session scope must not be empty");
    }
    if !(1..=MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE).contains(&limit) {
        bail!("transcript page size must be between 1 and {MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE}");
    }
    if before == Some(0) {
        bail!("transcript before ordinal must be positive");
    }

    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    let mut tool_names = BTreeMap::new();
    let mut projected = Vec::new();
    for entry in entries {
        if let SessionLogEntry::Control(control) = &entry {
            if let Some(trace) = application_transcript_reasoning_trace(control) {
                let safe_content = safe_persistence_text(trace);
                let original_content_bytes = safe_content.len();
                let truncated = original_content_bytes > MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES;
                let content = truncate_application_transcript_text(
                    &safe_content,
                    MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES,
                );
                let ordinal = u64::try_from(projected.len())
                    .map_err(|_| anyhow!("transcript message count exceeds supported range"))?
                    .saturating_add(1);
                projected.push(ApplicationTranscriptMessage {
                    ordinal,
                    message_id: safe_application_transcript_message_id(&format!(
                        "reasoning-trace:{ordinal}"
                    )),
                    role: ApplicationTranscriptRole::Assistant,
                    content: Some(content),
                    assistant_kind: Some(AssistantMessageKind::ReasoningTrace),
                    tool_name: None,
                    image_attachment_count: 0,
                    truncated,
                    original_content_bytes: u64::try_from(original_content_bytes)
                        .map_err(|_| anyhow!("transcript content size exceeds supported range"))?,
                });
            }
            continue;
        }
        let (message, role, expected_role) = match entry {
            SessionLogEntry::User(message) => {
                (message, ApplicationTranscriptRole::User, MessageRole::User)
            }
            SessionLogEntry::Assistant(message) => {
                for call in &message.tool_calls {
                    tool_names.insert(
                        call.id.clone(),
                        truncate_application_transcript_text(
                            &safe_persistence_text(&call.name),
                            128,
                        ),
                    );
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
            SessionLogEntry::Control(_) => unreachable!("control entries are handled above"),
        };
        if message.role != expected_role {
            bail!("durable transcript entry role does not match its entry class");
        }
        let ordinal = u64::try_from(projected.len())
            .map_err(|_| anyhow!("transcript message count exceeds supported range"))?
            .saturating_add(1);
        let safe_content = message.content.as_deref().map(safe_persistence_text);
        let original_content_bytes = safe_content.as_ref().map_or(0, String::len);
        let truncated = original_content_bytes > MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES;
        let content = safe_content.map(|content| {
            truncate_application_transcript_text(&content, MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES)
        });
        let tool_name = message
            .tool_call_id
            .as_ref()
            .and_then(|call_id| tool_names.get(call_id))
            .cloned();
        projected.push(ApplicationTranscriptMessage {
            ordinal,
            message_id: safe_application_transcript_message_id(&message.id),
            role,
            content,
            assistant_kind: if role == ApplicationTranscriptRole::Assistant {
                message.assistant_kind
            } else {
                None
            },
            tool_name,
            image_attachment_count: u64::try_from(message.image_attachments.len())
                .map_err(|_| anyhow!("transcript attachment count exceeds supported range"))?,
            truncated,
            original_content_bytes: u64::try_from(original_content_bytes)
                .map_err(|_| anyhow!("transcript content size exceeds supported range"))?,
        });
    }

    let total_messages = u64::try_from(projected.len())
        .map_err(|_| anyhow!("transcript message count exceeds supported range"))?;
    let eligible_end = before.map_or(projected.len(), |before| {
        projected.partition_point(|message| message.ordinal < before)
    });
    let mut page_bytes = 0_usize;
    let mut messages = Vec::with_capacity(limit.min(eligible_end));
    for message in projected[..eligible_end].iter().rev() {
        if messages.len() == limit {
            break;
        }
        let message_bytes = message.content.as_ref().map_or(0, String::len);
        if !messages.is_empty()
            && page_bytes.saturating_add(message_bytes) > MAX_APPLICATION_TRANSCRIPT_PAGE_BYTES
        {
            break;
        }
        page_bytes = page_bytes.saturating_add(message_bytes);
        messages.push(message.clone());
    }
    messages.reverse();
    let next_before = messages
        .first()
        .filter(|message| message.ordinal > 1)
        .map(|message| message.ordinal);

    Ok(ApplicationTranscriptPage {
        session_scope_id: expected_session_scope_id.to_owned(),
        total_messages,
        messages,
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

fn safe_application_transcript_message_id(value: &str) -> String {
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
        let root_config = RootConfig::load(&config_path)?;
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
        let execution_backend = crate::build_configured_execution_backend(&root_config)?;
        Ok::<_, anyhow::Error>((
            session,
            session_lease,
            workspace_root,
            execution_backend,
            request,
        ))
    })
    .await
    .map_err(|_| anyhow!("verification rerun preparation worker failed"))??;
    let (mut session, _session_lease, workspace_root, execution_backend, request) = preparation;
    let mut handler = NoopEventHandler;
    rerun_task_verification_check(
        &mut session,
        &mut handler,
        execution_backend.as_ref(),
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

/// Builds provider input with safe repository context candidates.
#[must_use]
pub fn application_run_input(workspace_root: &Path, prompt: String) -> AgentRunInput {
    let runtime_context =
        context_candidates_from_safe_sources(workspace_root, &prompt, None).unwrap_or_default();
    AgentRunInput::user(prompt)
        .with_runtime_context(runtime_context)
        .with_pending_input_provider(Arc::new(
            crate::pending_input::DurableQueuePendingInputProvider,
        ))
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
}

fn prepare_application_run_blocking(
    request: ApplicationRunRequest,
    session_leases: Arc<ApplicationSessionLeaseManager>,
    task_executor_attached: bool,
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
    let mutation_recorder = MutationEventRecorder::new(session_store.clone());
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
    let selected_model = admit_application_model_selection(
        &request,
        &root_config,
        &session,
        &sigil_paths.cache_root,
    )?;
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
    root_config.agent.runtime_provider = runtime_provider_name;
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
    let generate_session_title = request.constraints.is_none()
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
    let route_transition = crate::provider_connections::SessionRouteTransitionView {
        kind: route_transition_kind,
        connection_id: Some(session_route.model_ref.connection_id.as_str().to_owned()),
        model_id: Some(session_route.model_ref.model_id.clone()),
        remote_context_reset: route_remote_context_reset,
    };
    attach_session_url_capability_store(&mut session)
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
    if let Some(permission_mode) = request.permission_mode {
        options.permission_config.mode = permission_mode;
    }
    if let Some(reasoning_effort) = request.reasoning_effort {
        options.reasoning_effort = Some(reasoning_effort);
    }
    if let Some(constraints) = request.constraints.as_ref() {
        options.max_turns = Some(constraints.max_turns);
    }
    let mut input = AgentRunInput::user(request.prompt.clone())
        .with_logical_run_id(request.run_id.clone())
        .with_cancellation(cancellation_handle.clone())
        .with_pending_input_provider(Arc::new(
            crate::pending_input::DurableQueuePendingInputProvider,
        ));
    if let Some(loaded_skill) = loaded_skill.as_ref() {
        input
            .transient_context
            .push(loaded_skill.transient_context.clone());
    }
    if let Some(constraints) = request.constraints.as_ref() {
        input = input.with_max_output_tokens(constraints.max_output_tokens);
    }
    let target_max_tokens = request
        .constraints
        .as_ref()
        .map(|constraints| constraints.max_output_tokens);
    let redactor = secret_redactor_for_root_config(&root_config);
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

fn admit_application_skill_binding(
    request: &ApplicationRunRequest,
    root_config: &RootConfig,
    workspace_root: &Path,
    session: &mut Session,
) -> std::result::Result<Option<crate::LoadedSkillContext>, ApplicationRunPrepareError> {
    let Some(binding) = request.skill_binding.as_ref() else {
        return Ok(None);
    };
    let user_config_dir = sigil_kernel::default_user_config_dir().ok();
    let report = crate::discover_skill_index_with_user_dir(
        workspace_root,
        user_config_dir.as_deref(),
        &root_config.skills,
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
    cache_root: &Path,
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
            if !application_model_ref_is_selectable(root_config, &model_ref, cache_root) {
                return Err(ApplicationRunPrepareError::InvalidInvocation {
                    message: format!(
                        "model {}/{} is not admitted by the exact connection catalog",
                        model_ref.connection_id, model_ref.model_id
                    ),
                });
            }
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
            let catalog_entries = application_model_catalog_entries(
                root_config,
                &current_route.model_ref,
                cache_root,
            );
            let available_models = application_model_option_views(root_config, catalog_entries);
            let expected_binding =
                application_model_selection_binding(&current_route.model_ref, &available_models);
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
            if !available_models.iter().any(|option| {
                option.model_ref == model_ref
                    && option.availability
                        != crate::provider_connections::ModelAvailability::ConfiguredUnavailable
            }) {
                return Err(ApplicationRunPrepareError::InvalidInvocation {
                    message: format!(
                        "model {}/{} is not available for the configured provider connection",
                        model_ref.connection_id, model_ref.model_id
                    ),
                });
            }
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
struct ApplicationRunEventSequence {
    session_id: String,
    run_id: String,
    state: Arc<Mutex<ApplicationRunEventState>>,
}

#[derive(Debug, Default)]
struct ApplicationRunEventState {
    sequence: u64,
    terminal: bool,
}

impl ApplicationRunEventSequence {
    fn new(session_id: String, run_id: String) -> Self {
        Self {
            session_id,
            run_id,
            state: Arc::new(Mutex::new(ApplicationRunEventState::default())),
        }
    }

    fn emit<H>(&self, handler: &mut H, event: PublicRunEventKind) -> Result<()>
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
        let sequence = state
            .sequence
            .checked_add(1)
            .context("application run event sequence exhausted")?;
        let terminal = is_terminal_public_run_event(&event);
        handler.handle_public_event(PublicRunEvent::new(
            self.session_id.clone(),
            self.run_id.clone(),
            sequence,
            event,
        ))?;
        state.sequence = sequence;
        if terminal {
            state.terminal = true;
        }
        Ok(())
    }

    fn terminal_was_delivered(&self) -> Result<bool> {
        self.state
            .lock()
            .map(|state| state.terminal)
            .map_err(|_| anyhow!("application run event sequence is unavailable"))
    }
}

fn is_terminal_public_run_event(event: &PublicRunEventKind) -> bool {
    matches!(
        event,
        PublicRunEventKind::RunFinished { .. }
            | PublicRunEventKind::RunFailed { .. }
            | PublicRunEventKind::RunCancelled
    )
}

fn application_task_run_status_label(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Started => "started",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Completed => "completed",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Interrupted => "interrupted",
    }
}

/// Executes one prepared plan review revision on an application-surface session.
///
/// HTTP/Desktop use this so `Revise` actually runs the new read-only plan review instead of
/// leaving a dangling `Started` attempt. The run is fail-closed: only the frozen read-only tool
/// surface is exposed and the permission mode is read-only; every terminal outcome (draft
/// committed, no-draft closure, cancelled, failed) is written durably through
/// [`PlanReviewCoordinator::close_plan_review_run`].
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
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    let model_ref = session
        .resolved_model_route()
        .map(|route| route.model_ref.clone())
        .unwrap_or_else(|| fallback_route.model_ref.clone());
    crate::PlanReviewCoordinator::ensure_attempt_started(
        &mut session,
        request,
        current_unix_time_ms(),
    )?;
    let cancellation_handle = cancellation.unwrap_or_else(|| RunCancellationOwner::new().handle());
    let outcome = (async {
        let provider = crate::build_provider_for_model_ref_async(root_config, &model_ref).await?;
        let mut base_registry = sigil_kernel::ToolRegistry::new();
        sigil_tools_builtin::register_builtin_tools(&mut base_registry);
        crate::register_agent_tools(&mut base_registry, root_config)?;
        let tool_registry =
            crate::build_plan_review_tool_registry(&base_registry, root_config).into_registry();
        let options = crate::build_run_options(
            root_config,
            workspace_root.to_path_buf(),
            sigil_kernel::InteractionMode::Headless,
            None,
        );
        let agent = sigil_kernel::Agent::new(provider, base_registry);
        let mut bridge = PublicApplicationEventBridge::new(
            ApplicationRunEventSequence::new(
                session.session_scope_id().to_owned(),
                request.child_logical_run_id(),
            ),
            handler,
        );
        let outcome = match crate::PlanReviewCoordinator::run_plan_review(
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
        {
            Ok(outcome) => outcome,
            Err(error) => {
                let close = crate::PlanReviewCoordinator::close_plan_review_run_if_open(
                    &mut session,
                    request,
                    &crate::PlanReviewRunOutcome::Failed(
                        "plan review run failed before an outcome".to_owned(),
                    ),
                    current_unix_time_ms(),
                );
                if let Err(close_error) = close {
                    bail!(
                        "plan review run failed ({error:#}) and its terminal closure also failed ({close_error:#})"
                    );
                }
                return Err(error);
            }
        };
        match &outcome {
            crate::PlanReviewRunOutcome::AwaitingUserInput { .. } => {
                crate::PlanReviewCoordinator::close_plan_review_run(
                    &mut session,
                    request,
                    &outcome,
                    current_unix_time_ms(),
                )?;
            }
            crate::PlanReviewRunOutcome::DraftReady { draft } => {
                crate::PlanReviewCoordinator::commit_draft_from_child(
                    &mut session,
                    draft,
                    request,
                    current_unix_time_ms(),
                )?;
            }
            crate::PlanReviewRunOutcome::CompletedWithoutDraft => {
                crate::PlanReviewCoordinator::complete_without_draft(
                    &mut session,
                    request,
                    current_unix_time_ms(),
                )?;
            }
            crate::PlanReviewRunOutcome::Cancelled
            | crate::PlanReviewRunOutcome::Interrupted(_)
            | crate::PlanReviewRunOutcome::Failed(_)
            | crate::PlanReviewRunOutcome::SubmitOnlyProtocolViolation(_) => {
                crate::PlanReviewCoordinator::close_plan_review_run(
                    &mut session,
                    request,
                    &outcome,
                    current_unix_time_ms(),
                )?;
            }
        }
        Ok(outcome)
    })
    .await
    .map_err(|error| {
        let close = crate::PlanReviewCoordinator::close_plan_review_run_if_open(
            &mut session,
            request,
            &crate::PlanReviewRunOutcome::Failed(
                "plan review revision failed after the attempt started".to_owned(),
            ),
            current_unix_time_ms(),
        );
        match close {
            Ok(()) => error,
            Err(close_error) => anyhow::anyhow!(
                "plan review revision failed ({error:#}) and its terminal closure also failed ({close_error:#})"
            ),
        }
    })?;
    Ok(outcome)
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
    fn new(events: ApplicationRunEventSequence, handler: &'a mut H) -> Self {
        Self {
            events,
            task_events: PublicTaskEventProjector::default(),
            handler,
        }
    }

    fn emit(&mut self, event: PublicRunEventKind) -> Result<()> {
        self.events.emit(self.handler, event)
    }
}

impl<H> EventHandler for PublicApplicationEventBridge<'_, H>
where
    H: ApplicationRunEventHandler,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        let RunEvent::Control(control) = event else {
            return self.emit(event.into());
        };
        let task_events = self.task_events.project_control(&control);
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
            PublicRunEventKind::RunFailed {
                error: "run interrupted after reaching the configured turn limit".to_owned(),
            },
        ),
        AgentRunDisposition::Blocked => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "run blocked because its required delegation was not satisfied".to_owned(),
            },
        ),
        AgentRunDisposition::StartDurableTask(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "run requested a durable task handoff, but this application surface has not attached the task executor"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::ContinueDurableTask(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "run requested a durable task continuation, but this application surface has not attached the task executor"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::StartPlanReview(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "run requested a plan review, but this application surface has not attached the plan review coordinator"
                    .to_owned(),
            },
        ),
        AgentRunDisposition::PlanReviewDraftSubmitted(_) => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "plan review draft submitted outside an attached plan review coordinator".to_owned(),
            },
        ),
        AgentRunDisposition::TaskPlanAccepted => (
            ApplicationRunTerminalStatus::Blocked,
            PublicRunEventKind::RunFailed {
                error: "task planning completed outside an attached task executor".to_owned(),
            },
        ),
    }
}

fn append_application_conversation_terminal(
    recorder: &ConversationRunLifecycleRecorder,
    run_id: &str,
    status: ConversationRunTerminalStatusV1,
    final_message_id: Option<String>,
    summary: Option<&str>,
    redactor: &sigil_kernel::SecretRedactor,
) -> Result<()> {
    let terminal = ConversationRunFinalizedEntryV1::new(
        run_id,
        status,
        final_message_id,
        summary,
        current_unix_time_ms(),
        redactor,
    )?;
    recorder
        .append_finalized(&terminal)
        .context("failed to persist application conversation run terminal")?;
    Ok(())
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
