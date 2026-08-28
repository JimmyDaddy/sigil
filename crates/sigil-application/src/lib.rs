//! Transport-neutral application protocol for the post-R71 product surfaces.
//!
//! This crate owns the application boundary, not physical resources.  It deliberately depends on
//! the kernel's recovery surface instead of copying its schema, and it contains no TUI, runtime,
//! filesystem, sandbox, provider, or transport types.  A runtime host implements [`ApplicationPort`]
//! and binds it to one authenticated scope; callers can submit typed commands but cannot choose
//! their lane, resource authority, or presentation-completion authority.

use std::{
    collections::BTreeMap,
    fmt,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use sigil_kernel::resource_recovery_surface::{
    PublicRecoveryBlockerV2, RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION, ResourceEffectReceiptViewV1,
    ResourceRecoveryActionEnvelopeV1, ResourceRecoveryActionV1, ResourceRecoveryDomainV1,
    ResourceRecoveryReasonCodeV1, ResourceRecoveryRetryDispositionV1,
    ResourceRecoverySurfaceContractV1,
};

pub const APPLICATION_CONTRACT_SCHEMA_VERSION: u16 = 1;
pub const MAX_SAFE_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_PAGE_ITEMS: usize = 100;
pub const MAX_PAGE_BYTES: usize = 512 * 1024;

macro_rules! bounded_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ApplicationError> {
                let value = value.into();
                if value.is_empty() || value.len() > 256 {
                    return Err(ApplicationError::InvalidRequest(
                        concat!(stringify!($name), " must be non-empty and bounded").to_owned(),
                    ));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

bounded_id!(ApplicationInstanceId);
bounded_id!(ApplicationCommandId);
bounded_id!(CorrelationId);
bounded_id!(AuthenticatedSubject);
bounded_id!(HostConnectionInstanceId);
bounded_id!(SessionScopeId);
bounded_id!(WorkspaceScopeId);
bounded_id!(SessionItemId);
bounded_id!(PageRequestId);
bounded_id!(StablePageCursor);
bounded_id!(PageQueryFingerprint);
bounded_id!(PresentationMarkerId);
bounded_id!(PresenterSessionId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeText(String);

impl SafeText {
    pub fn new(value: impl Into<String>) -> Result<Self, ApplicationError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SAFE_TEXT_BYTES {
            return Err(ApplicationError::InvalidRequest(
                "safe text is empty or exceeds the application bound".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationScope {
    pub application_instance: ApplicationInstanceId,
    pub authenticated_subject: AuthenticatedSubject,
    pub workspace: Option<WorkspaceScopeId>,
    pub session: Option<SessionScopeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationFrontier {
    pub schema_version: u16,
    pub scope: ApplicationScope,
    pub writer_generation: u64,
    pub stream_generation: u64,
    pub through_sequence: u64,
    pub durable_cursor: String,
}

impl ApplicationFrontier {
    pub fn same_cut(&self, other: &Self) -> bool {
        self.schema_version == other.schema_version
            && self.scope == other.scope
            && self.writer_generation == other.writer_generation
            && self.stream_generation == other.stream_generation
            && self.through_sequence == other.through_sequence
            && self.durable_cursor == other.durable_cursor
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFrontier {
    pub scope: ApplicationScope,
    pub writer_generation: u64,
    pub through_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandLane {
    Urgent,
    Interactive,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectSettlementClass {
    AtomicDurableMutation,
    MonotonicControl,
    IdempotentWithKey,
    ExternalOrWorkspaceEffect,
    NonRepeatable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPolicy {
    pub lane: CommandLane,
    pub settlement: EffectSettlementClass,
    pub requires_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationCommand {
    SubmitPrompt { prompt: SafeText },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunCommand {
    Start,
    Cancel { binding: String },
    Pause { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalCommand {
    Resolve { binding: String, accepted: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanTaskCommand {
    Accept { binding: String },
    Reject { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentCommand {
    Start { binding: String },
    Stop { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserInputCommand {
    Resolve { binding: String, value: SafeText },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionCommand {
    Create,
    Switch { binding: String },
    Close { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigurationCommand {
    Save { binding: String, patch: SafeText },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderCommand {
    SelectRoute { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum McpCommand {
    Activate { binding: String },
    Refresh { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaintenanceCommand {
    Reconcile { binding: String },
    Compact { binding: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ApplicationCommand {
    Conversation(ConversationCommand),
    Run(RunCommand),
    Approval(ApprovalCommand),
    PlanTask(PlanTaskCommand),
    Agent(AgentCommand),
    UserInput(UserInputCommand),
    Session(SessionCommand),
    Configuration(ConfigurationCommand),
    Provider(ProviderCommand),
    Mcp(McpCommand),
    Maintenance(MaintenanceCommand),
}

impl ApplicationCommand {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Conversation(_) => "conversation",
            Self::Run(_) => "run",
            Self::Approval(_) => "approval",
            Self::PlanTask(_) => "plan_task",
            Self::Agent(_) => "agent",
            Self::UserInput(_) => "user_input",
            Self::Session(_) => "session",
            Self::Configuration(_) => "configuration",
            Self::Provider(_) => "provider",
            Self::Mcp(_) => "mcp",
            Self::Maintenance(_) => "maintenance",
        }
    }

    pub fn policy(&self) -> CommandPolicy {
        match self {
            Self::Conversation(ConversationCommand::SubmitPrompt { .. }) => CommandPolicy {
                lane: CommandLane::Interactive,
                settlement: EffectSettlementClass::ExternalOrWorkspaceEffect,
                requires_session: true,
            },
            Self::Run(RunCommand::Cancel { .. } | RunCommand::Pause { .. })
            | Self::Approval(_)
            | Self::UserInput(_) => CommandPolicy {
                lane: CommandLane::Urgent,
                settlement: EffectSettlementClass::MonotonicControl,
                requires_session: true,
            },
            Self::Run(RunCommand::Start) | Self::PlanTask(_) | Self::Agent(_) => CommandPolicy {
                lane: CommandLane::Interactive,
                settlement: EffectSettlementClass::IdempotentWithKey,
                requires_session: true,
            },
            Self::Session(_) | Self::Configuration(_) | Self::Provider(_) => CommandPolicy {
                lane: CommandLane::Interactive,
                settlement: EffectSettlementClass::AtomicDurableMutation,
                requires_session: false,
            },
            Self::Mcp(_) | Self::Maintenance(_) => CommandPolicy {
                lane: CommandLane::Background,
                settlement: EffectSettlementClass::IdempotentWithKey,
                requires_session: false,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationCommandEnvelope {
    pub schema_version: u16,
    pub command_id: ApplicationCommandId,
    pub correlation_id: Option<CorrelationId>,
    pub expected_frontier: ExpectedFrontier,
    pub command: ApplicationCommand,
}

impl ApplicationCommandEnvelope {
    pub fn validate(&self) -> Result<(), ApplicationError> {
        if self.schema_version != APPLICATION_CONTRACT_SCHEMA_VERSION {
            return Err(ApplicationError::UnknownSchema(self.schema_version));
        }
        if self.command.policy().requires_session && self.expected_frontier.scope.session.is_none()
        {
            return Err(ApplicationError::ScopeRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationCommandRequest {
    pub envelope: ApplicationCommandEnvelope,
    pub admission: CommandAdmissionContext,
}

/// Identity and scope facts injected by a trusted transport/composition root.
///
/// The ordinary application command payload does not carry these values. A transport adapter
/// binds the request to its authenticated principal, durable client epoch, and current
/// application scope before handing it to an [`ApplicationPort`]. The connection instance is
/// intentionally not part of the durable reservation key; it identifies only the live transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAdmissionContext {
    pub principal: AuthenticatedSubject,
    pub client_epoch: u64,
    pub connection_instance: HostConnectionInstanceId,
    pub scope: ApplicationScope,
}

impl CommandAdmissionContext {
    pub fn host_bound(
        principal: AuthenticatedSubject,
        client_epoch: u64,
        connection_instance: HostConnectionInstanceId,
        scope: ApplicationScope,
    ) -> Result<Self, ApplicationError> {
        if client_epoch == 0 {
            return Err(ApplicationError::InvalidRequest(
                "client epoch must be non-zero".to_owned(),
            ));
        }
        if principal != scope.authenticated_subject {
            return Err(ApplicationError::ScopeMismatch);
        }
        Ok(Self {
            principal,
            client_epoch,
            connection_instance,
            scope,
        })
    }

    pub fn reservation_key(&self, command_id: &ApplicationCommandId) -> CommandReservationKey {
        CommandReservationKey {
            application_instance: self.scope.application_instance.clone(),
            principal: self.principal.clone(),
            client_epoch: self.client_epoch,
            command_id: command_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CommandReservationKey {
    pub application_instance: ApplicationInstanceId,
    pub principal: AuthenticatedSubject,
    pub client_epoch: u64,
    pub command_id: ApplicationCommandId,
}

impl ApplicationCommandRequest {
    pub fn validate(&self) -> Result<(), ApplicationError> {
        self.envelope.validate()?;
        if self.admission.scope != self.envelope.expected_frontier.scope {
            return Err(ApplicationError::ScopeMismatch);
        }
        if self.admission.principal != self.admission.scope.authenticated_subject {
            return Err(ApplicationError::ScopeMismatch);
        }
        if self.admission.client_epoch == 0 {
            return Err(ApplicationError::InvalidRequest(
                "client epoch must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Computes the canonical command fingerprint used by application adapters.
pub fn command_fingerprint(
    request: &ApplicationCommandRequest,
) -> Result<String, ApplicationError> {
    request.validate()?;
    #[derive(Serialize)]
    struct FingerprintInput<'a> {
        envelope: &'a ApplicationCommandEnvelope,
        principal: &'a AuthenticatedSubject,
        client_epoch: u64,
    }
    let bytes = serde_json::to_vec(&FingerprintInput {
        envelope: &request.envelope,
        principal: &request.admission.principal,
        client_epoch: request.admission.client_epoch,
    })
    .map_err(|_| {
        ApplicationError::InvalidRequest("command could not be canonicalized".to_owned())
    })?;
    Ok(hex_digest(&bytes))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationDomainReceipt {
    pub command_id: ApplicationCommandId,
    pub command_kind: String,
    pub frontier: ApplicationFrontier,
    pub settlement: EffectSettlementClass,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncertainCommandReceipt {
    pub command_id: ApplicationCommandId,
    pub command_kind: String,
    pub reservation_fingerprint: String,
    pub recovery_binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplicationCommandReceipt {
    Settled(ApplicationDomainReceipt),
    Replayed(ApplicationDomainReceipt),
    Rejected(CommandRejection),
    PayloadConflict(CommandConflict),
    InFlight(ApplicationInFlightReceipt),
    Uncertain(UncertainCommandReceipt),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationInFlightReceipt {
    pub command_id: ApplicationCommandId,
    pub command_kind: String,
    pub reservation_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRejection {
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandConflict {
    pub command_id: ApplicationCommandId,
    pub original_fingerprint: String,
    pub received_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSurfaceProjection {
    pub session_id: Option<SessionScopeId>,
    pub title: SafeText,
    pub status: SafeText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSurfaceProjection {
    pub message_count: u64,
    pub latest_message: Option<SafeText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSurfaceProjection {
    pub status: SafeText,
    pub active_binding: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTaskSurfaceProjection {
    pub status: SafeText,
    pub action_binding: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSurfaceProjection {
    pub active_count: u16,
    pub summary: Vec<SafeText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalSurfaceProjection {
    pub pending: bool,
    pub binding: Option<String>,
    pub summary: Option<SafeText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputSurfaceProjection {
    pub pending: bool,
    pub binding: Option<String>,
    pub prompt: Option<SafeText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySurfaceProjection {
    pub can_submit: bool,
    pub can_cancel: bool,
    pub can_configure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationSurfaceProjection {
    pub persisted_revision: u64,
    pub selected_route: Option<SafeText>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionSurfaceProjection {
    pub last_notice: Option<SafeText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationProjection {
    pub schema_version: u16,
    pub scope: ApplicationScope,
    pub writer_generation: u64,
    pub stream_generation: u64,
    pub observer_generation: u64,
    pub frontier: ApplicationFrontier,
    pub resource_recovery: ResourceRecoverySurfaceContractV1,
    pub session: SessionSurfaceProjection,
    pub conversation: ConversationSurfaceProjection,
    pub run: RunSurfaceProjection,
    pub plan_task: PlanTaskSurfaceProjection,
    pub agents: AgentSurfaceProjection,
    pub approval: ApprovalSurfaceProjection,
    pub user_input: UserInputSurfaceProjection,
    pub capabilities: CapabilitySurfaceProjection,
    pub configuration: ConfigurationSurfaceProjection,
    pub attention: AttentionSurfaceProjection,
}

impl ApplicationProjection {
    pub fn validate(&self) -> Result<(), ApplicationError> {
        if self.schema_version != APPLICATION_CONTRACT_SCHEMA_VERSION {
            return Err(ApplicationError::UnknownSchema(self.schema_version));
        }
        if self.frontier.scope != self.scope
            || self.frontier.writer_generation != self.writer_generation
            || self.frontier.stream_generation != self.stream_generation
        {
            return Err(ApplicationError::CorruptProjection(
                "projection and frontier binding disagree".to_owned(),
            ));
        }
        self.resource_recovery.validate_schema().map_err(|_| {
            ApplicationError::CorruptProjection("invalid resource recovery schema".to_owned())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionSnapshotEnvelope {
    pub schema_version: u16,
    pub scope: ApplicationScope,
    pub writer_generation: u64,
    pub stream_generation: u64,
    pub observer_generation: u64,
    pub cut: ApplicationFrontier,
    pub projection: ApplicationProjection,
}

impl ProjectionSnapshotEnvelope {
    pub fn validate(&self) -> Result<(), ApplicationError> {
        if self.schema_version != APPLICATION_CONTRACT_SCHEMA_VERSION
            || self.scope != self.cut.scope
            || self.scope != self.projection.scope
            || self.writer_generation != self.cut.writer_generation
            || self.writer_generation != self.projection.writer_generation
            || self.stream_generation != self.cut.stream_generation
            || self.stream_generation != self.projection.stream_generation
            || self.observer_generation != self.projection.observer_generation
            || !self.projection.frontier.same_cut(&self.cut)
        {
            return Err(ApplicationError::CorruptProjection(
                "snapshot envelope has inconsistent scope or frontier".to_owned(),
            ));
        }
        self.projection.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApplicationEvent {
    ProjectionReplaced(Box<ApplicationProjection>),
    Notice { text: SafeText },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationEventEnvelope {
    pub schema_version: u16,
    pub scope: ApplicationScope,
    pub writer_generation: u64,
    pub stream_generation: u64,
    pub observer_generation: u64,
    pub event_id: String,
    pub base_frontier: ApplicationFrontier,
    pub next_frontier: ApplicationFrontier,
    pub payload_digest: String,
    pub payload: ApplicationEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionFeedItem {
    Event(Box<ApplicationEventEnvelope>),
    Gap { expected: u64, observed: u64 },
    ResetRequired { reason: &'static str },
    ScopeMismatch,
    Ahead,
    Expired,
    Closed(ApplicationFrontier),
    UnexpectedEof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSnapshot {
    pub envelope: ProjectionSnapshotEnvelope,
    pub feed: Vec<ProjectionFeedItem>,
}

/// Delivery acknowledgement emitted only after an adapter has committed an event to its local
/// projection. It is not a command receipt and it never implies presentation completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionDeliveryAck {
    pub scope: ApplicationScope,
    pub observer_generation: u64,
    pub event_id: String,
    pub frontier: ApplicationFrontier,
}

impl ProjectionDeliveryAck {
    pub fn validate(&self) -> Result<(), ApplicationError> {
        if self.scope != self.frontier.scope {
            return Err(ApplicationError::ScopeMismatch);
        }
        if self.event_id.is_empty() || self.event_id.len() > 256 {
            return Err(ApplicationError::InvalidRequest(
                "delivery acknowledgement event id is empty or unbounded".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionReducer {
    projection: ApplicationProjection,
    frontier: ApplicationFrontier,
    observer_generation: u64,
    seen_events: BTreeMap<String, String>,
}

impl ProjectionReducer {
    pub fn open(snapshot: ProjectionSnapshotEnvelope) -> Result<Self, ApplicationError> {
        snapshot.validate()?;
        Ok(Self {
            projection: snapshot.projection,
            frontier: snapshot.cut,
            observer_generation: snapshot.observer_generation,
            seen_events: BTreeMap::new(),
        })
    }

    pub fn projection(&self) -> &ApplicationProjection {
        &self.projection
    }

    pub fn frontier(&self) -> &ApplicationFrontier {
        &self.frontier
    }

    pub fn apply(&mut self, item: ProjectionFeedItem) -> Result<(), ApplicationError> {
        let ProjectionFeedItem::Event(event) = item else {
            return Err(ApplicationError::ResetRequired);
        };
        let event = *event;
        if event.schema_version != APPLICATION_CONTRACT_SCHEMA_VERSION {
            return Err(ApplicationError::UnknownSchema(event.schema_version));
        }
        if event.observer_generation != self.observer_generation
            || event.scope != self.frontier.scope
            || event.writer_generation != self.frontier.writer_generation
            || event.stream_generation != self.frontier.stream_generation
            || event.base_frontier.scope != event.scope
            || event.next_frontier.scope != event.scope
            || event.base_frontier.writer_generation != event.writer_generation
            || event.next_frontier.writer_generation != event.writer_generation
            || event.base_frontier.stream_generation != event.stream_generation
            || event.next_frontier.stream_generation != event.stream_generation
        {
            return Err(ApplicationError::ResetRequired);
        }
        let digest = digest_event(&event.payload)?;
        if digest != event.payload_digest {
            return Err(ApplicationError::CorruptProjection(
                "event payload digest mismatch".to_owned(),
            ));
        }
        if let Some(previous) = self.seen_events.get(&event.event_id) {
            return if previous == &event.payload_digest {
                Ok(())
            } else {
                Err(ApplicationError::CorruptProjection(
                    "event identity was reused with a different payload".to_owned(),
                ))
            };
        }
        if !event.base_frontier.same_cut(&self.frontier)
            || event.next_frontier.through_sequence != self.frontier.through_sequence + 1
        {
            return Err(ApplicationError::ResetRequired);
        }
        if let ApplicationEvent::ProjectionReplaced(projection) = event.payload {
            let projection = *projection;
            projection.validate()?;
            if !projection.frontier.same_cut(&event.next_frontier) {
                return Err(ApplicationError::CorruptProjection(
                    "event projection does not match next frontier".to_owned(),
                ));
            }
            self.projection = projection;
        }
        self.frontier = event.next_frontier;
        self.seen_events
            .insert(event.event_id, event.payload_digest);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageAnchor {
    pub item_id: Option<SessionItemId>,
    pub intra_item_row: u32,
    pub cursor: Option<StablePageCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageDirection {
    Older,
    Newer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionPageRequest {
    pub request_id: PageRequestId,
    pub scope: ApplicationScope,
    pub source_generation: u64,
    pub at_frontier: ApplicationFrontier,
    pub query: PageQueryFingerprint,
    pub anchor: PageAnchor,
    pub direction: PageDirection,
    pub limit: NonZeroUsize,
    pub width_bucket: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererSafeItem {
    pub item_id: SessionItemId,
    pub ordinal: u64,
    pub role: String,
    pub text: Option<SafeText>,
    pub estimated_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionPage {
    pub request_id: PageRequestId,
    pub scope: ApplicationScope,
    pub source_generation: u64,
    pub at_frontier: ApplicationFrontier,
    pub query: PageQueryFingerprint,
    pub before: Option<StablePageCursor>,
    pub after: Option<StablePageCursor>,
    pub total: u64,
    pub items: Vec<RendererSafeItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageCancellationReceipt {
    CancelledBeforeLoad,
    TooLate,
    Completed,
    UnknownRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenProjectionRequest {
    pub scope: ApplicationScope,
    pub observer_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplicationError {
    UnknownSchema(u16),
    InvalidRequest(String),
    ScopeRequired,
    ScopeMismatch,
    ResetRequired,
    CorruptProjection(String),
    NotFound,
    Unavailable,
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSchema(version) => {
                write!(formatter, "unknown application schema {version}")
            }
            Self::InvalidRequest(message) | Self::CorruptProjection(message) => {
                formatter.write_str(message)
            }
            Self::ScopeRequired => formatter.write_str("application session scope is required"),
            Self::ScopeMismatch => formatter.write_str("application scope mismatch"),
            Self::ResetRequired => formatter.write_str("projection reset is required"),
            Self::NotFound => formatter.write_str("application item was not found"),
            Self::Unavailable => formatter.write_str("application service is unavailable"),
        }
    }
}

impl std::error::Error for ApplicationError {}

pub trait ApplicationPort: Send + Sync {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>>;
    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>>;
    fn cancel_page(&self, request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt>;
    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;
    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reservation {
    fingerprint: String,
    receipt: ApplicationCommandReceipt,
}

#[derive(Debug, Default)]
struct FakeState {
    reservations: BTreeMap<CommandReservationKey, Reservation>,
    projection: Option<ProjectionSnapshotEnvelope>,
    pages: BTreeMap<PageRequestId, ProjectionPage>,
}

/// Deterministic in-process application for contract and adapter tests.
///
/// It models the durable identity rules (reservation key/fingerprint separation, exact replay,
/// payload conflict, scoped snapshot cut, and stale page rejection) without pretending to be the
/// runtime implementation or a resource authority.
#[derive(Debug, Clone, Default)]
pub struct FakeApplication {
    state: Arc<Mutex<FakeState>>,
}

impl FakeApplication {
    pub fn new(snapshot: ProjectionSnapshotEnvelope) -> Result<Self, ApplicationError> {
        snapshot.validate()?;
        Ok(Self {
            state: Arc::new(Mutex::new(FakeState {
                projection: Some(snapshot),
                ..FakeState::default()
            })),
        })
    }

    fn fingerprint(request: &ApplicationCommandRequest) -> Result<String, ApplicationError> {
        command_fingerprint(request)
    }
}

impl ApplicationPort for FakeApplication {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
            let envelope = state
                .projection
                .clone()
                .ok_or(ApplicationError::Unavailable)?;
            if envelope.scope != request.scope
                || envelope.observer_generation != request.observer_generation
            {
                return Err(ApplicationError::ScopeMismatch);
            }
            Ok(ProjectionSnapshot {
                envelope,
                feed: Vec::new(),
            })
        })
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            if request.limit.get() > MAX_PAGE_ITEMS {
                return Err(ApplicationError::InvalidRequest(
                    "page limit exceeds bound".to_owned(),
                ));
            }
            let mut state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
            if let Some(page) = state.pages.get(&request.request_id) {
                if page.scope != request.scope
                    || page.source_generation != request.source_generation
                {
                    return Err(ApplicationError::ScopeMismatch);
                }
                return Ok(page.clone());
            }
            let projection = state
                .projection
                .as_ref()
                .ok_or(ApplicationError::Unavailable)?;
            if projection.scope != request.scope || !projection.cut.same_cut(&request.at_frontier) {
                return Err(ApplicationError::ResetRequired);
            }
            let page = ProjectionPage {
                request_id: request.request_id.clone(),
                scope: request.scope,
                source_generation: request.source_generation,
                at_frontier: request.at_frontier,
                query: request.query,
                before: None,
                after: None,
                total: projection.projection.conversation.message_count,
                items: Vec::new(),
            };
            state.pages.insert(request.request_id, page.clone());
            Ok(page)
        })
    }

    fn cancel_page(&self, request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let Ok(mut state) = state.lock() else {
                return PageCancellationReceipt::UnknownRequest;
            };
            if state.pages.remove(&request).is_some() {
                PageCancellationReceipt::TooLate
            } else {
                PageCancellationReceipt::CancelledBeforeLoad
            }
        })
    }

    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async move { acknowledgement.validate() })
    }

    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let fingerprint = Self::fingerprint(&request)?;
            let reservation_key = request
                .admission
                .reservation_key(&request.envelope.command_id);
            let mut state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
            if let Some(previous) = state.reservations.get(&reservation_key) {
                if previous.fingerprint == fingerprint {
                    return Ok(match &previous.receipt {
                        ApplicationCommandReceipt::Settled(receipt) => {
                            ApplicationCommandReceipt::Replayed(receipt.clone())
                        }
                        receipt => receipt.clone(),
                    });
                }
                return Ok(ApplicationCommandReceipt::PayloadConflict(
                    CommandConflict {
                        command_id: request.envelope.command_id,
                        original_fingerprint: previous.fingerprint.clone(),
                        received_fingerprint: fingerprint,
                    },
                ));
            }
            let projection = state
                .projection
                .as_ref()
                .ok_or(ApplicationError::Unavailable)?;
            if projection.cut.scope != request.envelope.expected_frontier.scope
                || projection.cut.writer_generation
                    != request.envelope.expected_frontier.writer_generation
                || projection.cut.through_sequence
                    != request.envelope.expected_frontier.through_sequence
            {
                return Ok(ApplicationCommandReceipt::Rejected(CommandRejection {
                    kind: "frontier_conflict".to_owned(),
                    reason: "expected frontier is stale".to_owned(),
                }));
            }
            let mut frontier = projection.cut.clone();
            frontier.through_sequence = frontier.through_sequence.saturating_add(1);
            let receipt = ApplicationCommandReceipt::Settled(ApplicationDomainReceipt {
                command_id: request.envelope.command_id.clone(),
                command_kind: request.envelope.command.kind().to_owned(),
                frontier,
                settlement: request.envelope.command.policy().settlement,
                summary: "fake application command committed".to_owned(),
            });
            state.reservations.insert(
                reservation_key.clone(),
                Reservation {
                    fingerprint,
                    receipt: receipt.clone(),
                },
            );
            Ok(receipt)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererNeutralPresentationObservation {
    pub marker_id: PresentationMarkerId,
    pub content_revision: u64,
    pub frame_nonce: u64,
    pub terminal_epoch: u64,
    pub sink_completion_nonce: u64,
}

#[derive(PartialEq, Eq)]
pub struct TrustedPresenterSession {
    id: PresenterSessionId,
    secret: u128,
}

#[derive(PartialEq, Eq)]
pub struct TrustedPresentationCapability {
    session_id: PresenterSessionId,
    marker_id: PresentationMarkerId,
    content_revision: u64,
    terminal_epoch: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PresenterAttestation {
    session_id: PresenterSessionId,
    observation: RendererNeutralPresentationObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumedPresentationReceipt {
    pub marker_id: PresentationMarkerId,
    pub frame_nonce: u64,
}

#[derive(Default)]
pub struct PresenterBroker {
    sessions: Mutex<BTreeMap<PresenterSessionId, u128>>,
    armed: Mutex<BTreeMap<PresentationMarkerId, PresentationBinding>>,
    consumed: Mutex<BTreeMap<PresentationMarkerId, u64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PresentationBinding {
    session_id: PresenterSessionId,
    content_revision: u64,
    terminal_epoch: u64,
}

impl fmt::Debug for TrustedPresenterSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedPresenterSession")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for TrustedPresentationCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedPresentationCapability")
            .field("session_id", &self.session_id)
            .field("marker_id", &self.marker_id)
            .field("content_revision", &self.content_revision)
            .field("terminal_epoch", &self.terminal_epoch)
            .finish()
    }
}

impl fmt::Debug for PresenterBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PresenterBroker")
            .field("sessions", &"<redacted>")
            .field("armed", &"<redacted>")
            .field("consumed", &"<redacted>")
            .finish()
    }
}

impl PresenterBroker {
    pub fn register(
        &self,
        session_id: PresenterSessionId,
        secret: u128,
    ) -> Result<TrustedPresenterSession, ApplicationError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if sessions.insert(session_id.clone(), secret).is_some() {
            return Err(ApplicationError::InvalidRequest(
                "presenter session already exists".to_owned(),
            ));
        }
        Ok(TrustedPresenterSession {
            id: session_id,
            secret,
        })
    }

    pub fn arm(
        &self,
        session: &TrustedPresenterSession,
        marker_id: PresentationMarkerId,
        content_revision: u64,
        terminal_epoch: u64,
    ) -> Result<TrustedPresentationCapability, ApplicationError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if sessions.get(&session.id) != Some(&session.secret) {
            return Err(ApplicationError::ScopeMismatch);
        }
        drop(sessions);
        let mut armed = self
            .armed
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if armed.contains_key(&marker_id) {
            return Err(ApplicationError::InvalidRequest(
                "presentation marker is already armed".to_owned(),
            ));
        }
        armed.insert(
            marker_id.clone(),
            PresentationBinding {
                session_id: session.id.clone(),
                content_revision,
                terminal_epoch,
            },
        );
        Ok(TrustedPresentationCapability {
            session_id: session.id.clone(),
            marker_id,
            content_revision,
            terminal_epoch,
        })
    }

    pub fn attest(
        &self,
        session: &TrustedPresenterSession,
        capability: &TrustedPresentationCapability,
        observation: RendererNeutralPresentationObservation,
    ) -> Result<PresenterAttestation, ApplicationError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if sessions.get(&session.id) != Some(&session.secret) {
            return Err(ApplicationError::ScopeMismatch);
        }
        if capability.session_id != session.id
            || observation.marker_id != capability.marker_id
            || observation.content_revision != capability.content_revision
            || observation.terminal_epoch != capability.terminal_epoch
            || observation.frame_nonce == 0
            || observation.sink_completion_nonce == 0
        {
            return Err(ApplicationError::InvalidRequest(
                "presentation observation does not match its armed capability".to_owned(),
            ));
        }
        let armed = self
            .armed
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let Some(binding) = armed.get(&capability.marker_id) else {
            return Err(ApplicationError::InvalidRequest(
                "presentation capability was already consumed or revoked".to_owned(),
            ));
        };
        if binding.session_id != session.id
            || binding.content_revision != observation.content_revision
            || binding.terminal_epoch != observation.terminal_epoch
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        Ok(PresenterAttestation {
            session_id: session.id.clone(),
            observation,
        })
    }

    pub fn complete(
        &self,
        attestation: PresenterAttestation,
    ) -> Result<ConsumedPresentationReceipt, ApplicationError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if !sessions.contains_key(&attestation.session_id) {
            return Err(ApplicationError::ScopeMismatch);
        }
        drop(sessions);
        let marker = attestation.observation.marker_id.clone();
        let mut armed = self
            .armed
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let binding = armed.remove(&marker).ok_or_else(|| {
            ApplicationError::InvalidRequest("presentation is not armed".to_owned())
        })?;
        if binding.session_id != attestation.session_id
            || binding.content_revision != attestation.observation.content_revision
            || binding.terminal_epoch != attestation.observation.terminal_epoch
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        drop(armed);
        let mut consumed = self
            .consumed
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if consumed
            .insert(marker.clone(), attestation.observation.frame_nonce)
            .is_some()
        {
            return Err(ApplicationError::InvalidRequest(
                "presentation marker was already consumed".to_owned(),
            ));
        }
        Ok(ConsumedPresentationReceipt {
            marker_id: marker,
            frame_nonce: attestation.observation.frame_nonce,
        })
    }

    pub fn revoke(&self, session: &TrustedPresenterSession) -> Result<(), ApplicationError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if sessions.remove(&session.id) != Some(session.secret) {
            return Err(ApplicationError::ScopeMismatch);
        }
        drop(sessions);
        let mut armed = self
            .armed
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        armed.retain(|_, binding| binding.session_id != session.id);
        Ok(())
    }
}

fn digest_event(event: &ApplicationEvent) -> Result<String, ApplicationError> {
    let bytes = serde_json::to_vec(event).map_err(|_| {
        ApplicationError::CorruptProjection("event could not be encoded".to_owned())
    })?;
    Ok(hex_digest(&bytes))
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> ApplicationScope {
        ApplicationScope {
            application_instance: ApplicationInstanceId::new("app").expect("valid id"),
            authenticated_subject: AuthenticatedSubject::new("subject").expect("valid id"),
            workspace: Some(WorkspaceScopeId::new("workspace").expect("valid id")),
            session: Some(SessionScopeId::new("session").expect("valid id")),
        }
    }

    fn snapshot() -> ProjectionSnapshotEnvelope {
        let scope = scope();
        let frontier = ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: scope.clone(),
            writer_generation: 1,
            stream_generation: 1,
            through_sequence: 0,
            durable_cursor: "cursor-0".to_owned(),
        };
        let recovery = ResourceRecoverySurfaceContractV1 {
            schema_version: RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
            blocker: None,
            resource_effects: Vec::new(),
            action_envelope: None,
        };
        let projection = ApplicationProjection {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: scope.clone(),
            writer_generation: 1,
            stream_generation: 1,
            observer_generation: 9,
            frontier: frontier.clone(),
            resource_recovery: recovery,
            session: SessionSurfaceProjection {
                session_id: scope.session.clone(),
                title: SafeText::new("test").expect("valid text"),
                status: SafeText::new("idle").expect("valid text"),
            },
            conversation: ConversationSurfaceProjection {
                message_count: 0,
                latest_message: None,
            },
            run: RunSurfaceProjection {
                status: SafeText::new("idle").expect("valid text"),
                active_binding: None,
            },
            plan_task: PlanTaskSurfaceProjection {
                status: SafeText::new("none").expect("valid text"),
                action_binding: None,
            },
            agents: AgentSurfaceProjection {
                active_count: 0,
                summary: Vec::new(),
            },
            approval: ApprovalSurfaceProjection {
                pending: false,
                binding: None,
                summary: None,
            },
            user_input: UserInputSurfaceProjection {
                pending: false,
                binding: None,
                prompt: None,
            },
            capabilities: CapabilitySurfaceProjection {
                can_submit: true,
                can_cancel: false,
                can_configure: true,
            },
            configuration: ConfigurationSurfaceProjection {
                persisted_revision: 1,
                selected_route: None,
                dirty: false,
            },
            attention: AttentionSurfaceProjection { last_notice: None },
        };
        ProjectionSnapshotEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope,
            writer_generation: 1,
            stream_generation: 1,
            observer_generation: 9,
            cut: frontier,
            projection,
        }
    }

    #[test]
    fn fake_application_replays_exact_receipt_and_rejects_payload_conflict() {
        let app = FakeApplication::new(snapshot()).expect("valid snapshot");
        let expected = snapshot().cut;
        let command_id = ApplicationCommandId::new("command").expect("valid id");
        let request = || ApplicationCommandRequest {
            envelope: ApplicationCommandEnvelope {
                schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
                command_id: command_id.clone(),
                correlation_id: None,
                expected_frontier: ExpectedFrontier {
                    scope: expected.scope.clone(),
                    writer_generation: expected.writer_generation,
                    through_sequence: expected.through_sequence,
                },
                command: ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                    prompt: SafeText::new("hello").expect("valid text"),
                }),
            },
            admission: CommandAdmissionContext::host_bound(
                expected.scope.authenticated_subject.clone(),
                1,
                HostConnectionInstanceId::new("connection").expect("valid id"),
                expected.scope.clone(),
            )
            .expect("valid admission"),
        };
        let first = futures::executor::block_on(app.execute(request())).expect("execute");
        assert!(matches!(first, ApplicationCommandReceipt::Settled(_)));
        let replay = futures::executor::block_on(app.execute(request())).expect("replay");
        assert!(matches!(replay, ApplicationCommandReceipt::Replayed(_)));
        let mut conflicting = request();
        conflicting.envelope.command =
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: SafeText::new("different").expect("valid text"),
            });
        assert!(matches!(
            futures::executor::block_on(app.execute(conflicting)).expect("conflict"),
            ApplicationCommandReceipt::PayloadConflict(_)
        ));
    }

    #[test]
    fn reducer_rejects_gap_and_accepts_exact_event_chain() {
        let envelope = snapshot();
        let mut reducer = ProjectionReducer::open(envelope.clone()).expect("valid snapshot");
        let mut next = envelope.projection.clone();
        next.frontier.through_sequence = 1;
        next.frontier.durable_cursor = "cursor-1".to_owned();
        let payload = ApplicationEvent::ProjectionReplaced(Box::new(next.clone()));
        let event = ApplicationEventEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: envelope.scope.clone(),
            writer_generation: 1,
            stream_generation: 1,
            observer_generation: 9,
            event_id: "event-1".to_owned(),
            base_frontier: envelope.cut.clone(),
            next_frontier: next.frontier.clone(),
            payload_digest: digest_event(&payload).expect("digest"),
            payload,
        };
        reducer
            .apply(ProjectionFeedItem::Event(Box::new(event)))
            .expect("exact event");
        assert_eq!(reducer.frontier().through_sequence, 1);
        assert_eq!(
            reducer.apply(ProjectionFeedItem::Gap {
                expected: 2,
                observed: 4
            }),
            Err(ApplicationError::ResetRequired)
        );
    }

    #[test]
    fn presenter_capability_is_one_shot() {
        let broker = PresenterBroker::default();
        let session_id = PresenterSessionId::new("tui").expect("valid id");
        let session = broker.register(session_id, 42).expect("register");
        let observation = RendererNeutralPresentationObservation {
            marker_id: PresentationMarkerId::new("marker").expect("valid id"),
            content_revision: 1,
            frame_nonce: 2,
            terminal_epoch: 3,
            sink_completion_nonce: 4,
        };
        let capability = broker
            .arm(&session, observation.marker_id.clone(), 1, 3)
            .expect("arm");
        let attestation = broker
            .attest(&session, &capability, observation.clone())
            .expect("attest");
        broker.complete(attestation).expect("complete");
        assert!(broker.attest(&session, &capability, observation).is_err());
        assert!(!format!("{session:?}").contains("42"));
        assert!(!format!("{broker:?}").contains("42"));
    }
}
