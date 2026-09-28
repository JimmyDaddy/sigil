//! Durable-session adapter for the transport-neutral application projection.

use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use futures::future::BoxFuture;
use sigil_application::{
    APPLICATION_CONTRACT_SCHEMA_VERSION, AgentSurfaceProjection, ApplicationError,
    ApplicationFrontier, ApplicationInstanceId, ApplicationProjection, ApplicationQueueItemKind,
    ApplicationQueueItemProjection, ApplicationQueueSurfaceProjection, ApplicationQueueTarget,
    ApplicationScope, ApplicationTerminalTaskProjection, AttentionSurfaceProjection,
    CapabilitySurfaceProjection, ConfigurationSurfaceProjection, ConversationSurfaceProjection,
    OpenProjectionRequest, PageDirection, PlanTaskSurfaceProjection, ProjectionFeedItem,
    ProjectionPage, ProjectionPageRequest, ProjectionSnapshot, ProjectionSnapshotEnvelope,
    ResourceRecoverySurfaceContractV1, RunSurfaceProjection, SafeText, SessionItemId,
    SessionScopeId, SessionSurfaceProjection, StablePageCursor, TerminalSurfaceProjection,
    UserInputSurfaceProjection,
};
use sigil_kernel::{
    ControlEntry, ConversationQueueDurableProjection, DurableEventType, JsonlSessionStore,
    PUBLIC_EVENT_DELIVERY_BATCH_MAX_RECORDS, PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
    PublicEventDeliveryReceiptV1, PublicEventOutboxEntryV1, PublicEventOutboxRecorder,
    PublicRunEventKind, SessionLogEntry, SessionRecordReadHandle, SessionStreamRecord,
    TerminalReadinessStatus, TerminalTaskProjection,
};

const MAX_PROJECTION_RANGE_BYTES: usize = 4 * 1024 * 1024;

mod message_content;
mod read_model;
pub use read_model::ProjectionReadMetrics;
use read_model::{ProjectionRecordCache, corrupt, unavailable};

/// Canonical display inputs; all durable reads remain bound to the session owner.
pub struct ConversationDisplayQuery<'a> {
    pub expected_session_scope_id: &'a str,
    pub cursor: Option<&'a str>,
    pub limit: usize,
    pub current_workspace_snapshot_id: Option<&'a str>,
    pub artifact_store: Option<&'a sigil_kernel::ToolArtifactStore>,
}

/// One attachment owns one incremental state and a single catch-up boundary. Clones share it.
#[derive(Debug, Clone)]
pub struct RuntimeSessionProjectionOwner {
    reader: SessionRecordReadHandle,
    delivery_recorder: Option<PublicEventOutboxRecorder>,
    cache: Arc<Mutex<ProjectionRecordCache>>,
    observations: Arc<std::sync::atomic::AtomicUsize>,
}

impl RuntimeSessionProjectionOwner {
    #[must_use]
    pub fn from_run_recorder_source(source: &sigil_kernel::SessionRunRecorderSource) -> Self {
        let mut owner = Self::from_read_handle(source.read_handle());
        owner.delivery_recorder = Some(source.public_outbox_recorder());
        owner
    }

    #[must_use]
    pub fn from_store(store: &JsonlSessionStore) -> Self {
        let mut owner = Self::from_read_handle(store.read_handle());
        owner.delivery_recorder = Some(PublicEventOutboxRecorder::new(store.clone()));
        owner
    }
    /// Creates a read-only attachment. This handle cannot issue ACKs or recover a writer.
    #[must_use]
    pub fn from_read_handle(reader: SessionRecordReadHandle) -> Self {
        Self {
            reader,
            delivery_recorder: None,
            cache: Arc::new(Mutex::new(ProjectionRecordCache::default())),
            observations: Arc::default(),
        }
    }
    #[must_use]
    pub fn pending_observations(&self) -> usize {
        self.observations.load(std::sync::atomic::Ordering::Acquire)
    }
    #[must_use]
    pub fn read_handle(&self) -> SessionRecordReadHandle {
        self.reader.clone()
    }
    pub fn metrics(&self) -> Result<ProjectionReadMetrics, ApplicationError> {
        Ok(self.cache.lock().map_err(unavailable)?.metrics)
    }
    /// Reads one renderer-safe page through this session owner's shared incremental index.
    pub fn transcript_page(
        &self,
        expected_session_scope_id: &str,
        before: Option<u64>,
        limit: usize,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<crate::application_run::ApplicationTranscriptPage, ApplicationError> {
        if limit == 0
            || limit > crate::application_run::MAX_APPLICATION_TRANSCRIPT_PAGE_SIZE
            || before == Some(0)
        {
            return Err(ApplicationError::InvalidRequest(
                "invalid transcript page bounds".into(),
            ));
        }
        let mut state = self.state(budget)?;
        if state.session_id.as_deref() != Some(expected_session_scope_id) {
            return Err(ApplicationError::ScopeMismatch);
        }
        let sequence = state.sequence;
        state.transcript_page(self, sequence, before, limit, budget)
    }
    /// Reads the formal Desktop display, including Plan/Task/input summaries, from the same index.
    pub fn conversation_display_page(
        &self,
        request: ConversationDisplayQuery<'_>,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<
        crate::conversation_display::ConversationDisplayPageV1,
        crate::conversation_display::ConversationDisplayProjectionError,
    > {
        let mut state = self.state(budget).map_err(
            crate::conversation_display::ConversationDisplayProjectionError::from_application,
        )?;
        if state.session_id.as_deref() != Some(request.expected_session_scope_id) {
            return Err(anyhow::anyhow!("conversation display session scope mismatch").into());
        }
        state.display_page(self, request, budget)
    }
    fn state(
        &self,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<std::sync::MutexGuard<'_, ProjectionRecordCache>, ApplicationError> {
        loop {
            budget.check().map_err(unavailable)?;
            match self.cache.try_lock() {
                Ok(mut state) => {
                    if let Some(error) = &state.permanent_error {
                        return Err(error.clone());
                    }
                    if let Err(error) = state.synchronize(self, budget) {
                        if matches!(
                            error,
                            ApplicationError::ResetRequired
                                | ApplicationError::ScopeMismatch
                                | ApplicationError::CorruptProjection(_)
                        ) {
                            let metrics = state.metrics;
                            *state = ProjectionRecordCache::default();
                            state.metrics = metrics;
                            state.permanent_error = Some(error.clone());
                        }
                        return Err(error);
                    }
                    return Ok(state);
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(ApplicationError::Unavailable);
                }
            }
        }
    }
}

/// Runtime-owned binding used to construct an application projection without exposing its durable
/// paths to the application contract or renderer.
#[derive(Debug, Clone)]
pub struct RuntimeSessionProjectionBinding {
    config_path: PathBuf,
    _launch_cwd: PathBuf,
    #[cfg_attr(not(test), allow(dead_code))]
    session_path: PathBuf,
    expected_session_scope_id: String,
    scope: ApplicationScope,
    writer_generation: u64,
    stream_generation: u64,
    observer_generation: u64,
    source_generation: u64,
    owner: Arc<Mutex<Option<RuntimeSessionProjectionOwner>>>,
    configuration: Arc<Mutex<Option<QueryConfiguration>>>,
}

#[derive(Debug)]
struct QueryConfiguration {
    source: read_model::SourceIdentity,
    modified: Option<SystemTime>,
    length: u64,
    route_revision: u64,
    model_name: String,
    recovery_required: bool,
}

impl RuntimeSessionProjectionBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config_path: PathBuf,
        launch_cwd: PathBuf,
        session_path: PathBuf,
        expected_session_scope_id: String,
        application_instance: ApplicationInstanceId,
        authenticated_subject: sigil_application::AuthenticatedSubject,
        workspace: Option<sigil_application::WorkspaceScopeId>,
        writer_generation: u64,
        stream_generation: u64,
        observer_generation: u64,
        source_generation: u64,
    ) -> Result<Self, ApplicationError> {
        let session = SessionScopeId::new(expected_session_scope_id.clone())?;
        let scope = ApplicationScope {
            application_instance,
            authenticated_subject,
            workspace,
            session: Some(session),
        };
        if writer_generation == 0
            || stream_generation == 0
            || observer_generation == 0
            || source_generation == 0
        {
            return Err(ApplicationError::InvalidRequest(
                "runtime application projection generations must be non-zero".to_owned(),
            ));
        }
        Ok(Self {
            config_path,
            _launch_cwd: launch_cwd,
            session_path,
            expected_session_scope_id,
            scope,
            writer_generation,
            stream_generation,
            observer_generation,
            source_generation,
            owner: Arc::new(Mutex::new(None)),
            configuration: Arc::new(Mutex::new(None)),
        })
    }

    /// Attaches capabilities explicitly transferred by this session's host owner.
    ///
    /// Every snapshot still validates its durable session identity. A missing or failed owned
    /// read never falls back to a path-based observer, and detached bindings cannot append ACKs.
    #[must_use]
    pub fn with_owner(mut self, owner: RuntimeSessionProjectionOwner) -> Self {
        self.owner = Arc::new(Mutex::new(Some(owner)));
        self
    }

    pub fn scope(&self) -> &ApplicationScope {
        &self.scope
    }

    /// Queries only the owned durable stream, without configuration or provider inventory.
    pub async fn durable_frontier(&self) -> Result<ApplicationFrontier, ApplicationError> {
        let binding = self.clone();
        observation(binding.observation_counter(), move |budget| {
            let owner = binding.owner()?;
            let state = owner.state(&budget)?;
            if state.session_id.as_deref() != Some(&binding.expected_session_scope_id) {
                return Err(ApplicationError::ScopeMismatch);
            }
            Ok(binding.frontier(state.sequence))
        })
        .await
    }

    fn owner(&self) -> Result<RuntimeSessionProjectionOwner, ApplicationError> {
        self.owner
            .lock()
            .map_err(unavailable)?
            .clone()
            .ok_or(ApplicationError::Unavailable)
    }

    /// Replaces a retired worker's projection capability for this same durable session.
    /// The host must drain observations before replacement; a new scope requires a new binding.
    pub fn replace_owner(
        &self,
        owner: RuntimeSessionProjectionOwner,
    ) -> Result<(), ApplicationError> {
        {
            let state = owner.state(&sigil_kernel::SessionReadBudget::default())?;
            if state.session_id.as_deref() != Some(&self.expected_session_scope_id) {
                return Err(ApplicationError::ScopeMismatch);
            }
        }
        let mut current = self.owner.lock().map_err(unavailable)?;
        if current
            .as_ref()
            .is_some_and(|owner| owner.pending_observations() != 0)
        {
            return Err(ApplicationError::Unavailable);
        }
        *current = Some(owner);
        Ok(())
    }

    pub fn read_handle(&self) -> Result<SessionRecordReadHandle, ApplicationError> {
        Ok(self.owner()?.read_handle())
    }
    #[must_use]
    pub fn pending_observations(&self) -> usize {
        self.owner().map_or(0, |owner| owner.pending_observations())
    }
    fn observation_counter(&self) -> Arc<std::sync::atomic::AtomicUsize> {
        self.owner()
            .map(|owner| Arc::clone(&owner.observations))
            .unwrap_or_default()
    }
    fn frontier(&self, sequence: u64) -> ApplicationFrontier {
        ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: self.scope.clone(),
            writer_generation: self.writer_generation,
            stream_generation: self.stream_generation,
            through_sequence: sequence,
            durable_cursor: format!("session-stream:{sequence}"),
        }
    }
    fn validate_frontier(
        &self,
        frontier: &ApplicationFrontier,
        sequence: u64,
    ) -> Result<(), ApplicationError> {
        if frontier.scope != self.scope
            || frontier.writer_generation != self.writer_generation
            || frontier.stream_generation != self.stream_generation
            || frontier.schema_version != APPLICATION_CONTRACT_SCHEMA_VERSION
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        if frontier.through_sequence == 0
            || frontier.through_sequence > sequence
            || frontier.durable_cursor != format!("session-stream:{}", frontier.through_sequence)
        {
            return Err(ApplicationError::ResetRequired);
        }
        Ok(())
    }
    fn configuration(
        &self,
        state: &ProjectionRecordCache,
    ) -> Result<(String, bool), ApplicationError> {
        let metadata = std::fs::metadata(&self.config_path).map_err(unavailable)?;
        let source = read_model::SourceIdentity::from_metadata(&metadata);
        let mut cached = self.configuration.lock().map_err(unavailable)?;
        if let Some(config) = cached.as_ref()
            && config.source == source
            && config.modified == metadata.modified().ok()
            && config.length == metadata.len()
            && config.route_revision == state.route_revision
        {
            return Ok((config.model_name.clone(), config.recovery_required));
        }
        let root = sigil_kernel::RootConfig::load(&self.config_path)
            .map_err(unavailable)?
            .with_effective_composition()
            .map_err(unavailable)?;
        let route = crate::application_run::application_session_route(&state.route_entries)
            .ok_or(ApplicationError::Unavailable)?;
        let config =
            crate::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(&root);
        let planned = crate::provider_connections::plan_session_route_resume(
            &config,
            &crate::provider_connections::SessionRouteResumeInput {
                route: route.clone(),
                egress_trust_binding:
                    crate::application_run::application_session_route_trust_binding(
                        &state.route_entries,
                    ),
            },
        );
        let (model_name, route_recovery) = match planned {
            crate::provider_connections::SessionRouteResumePlan::Exact { route, .. } => {
                (route.model_ref.model_id, false)
            }
            crate::provider_connections::SessionRouteResumePlan::RebindCurrentModel {
                target_route,
                ..
            } => (target_route.model_ref.model_id, false),
            crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation {
                target_route,
                ..
            } => (target_route.model_ref.model_id, true),
            _ => (route.model_ref.model_id, true),
        };
        *cached = Some(QueryConfiguration {
            source,
            modified: metadata.modified().ok(),
            length: metadata.len(),
            route_revision: state.route_revision,
            model_name: model_name.clone(),
            recovery_required: route_recovery,
        });
        Ok((model_name, route_recovery))
    }
    fn envelope(
        &self,
        state: &ProjectionRecordCache,
    ) -> Result<ProjectionSnapshotEnvelope, ApplicationError> {
        if state.session_id.as_deref() != Some(&self.expected_session_scope_id) {
            return Err(ApplicationError::ScopeMismatch);
        }
        let frontier = self.frontier(state.sequence);
        let (model_name, route_recovery) = self.configuration(state)?;
        let event_state = state.event_state.clone();
        let status = if route_recovery {
            "recovery-required"
        } else {
            event_state.run_status
        };
        let latest_message = state.latest_message.as_deref().unwrap_or("No messages yet");
        let queue = &state.queue;
        let terminal = terminal_surface_from_projection(&state.terminal)?;
        let queue_revision = queue.current_revision();
        let queue_next_dispatchable = queue.queue.next_dispatchable.clone();
        let queue_paused = queue.queue.paused;
        let queue_items = queue.queue.items.clone();
        let queue = ApplicationQueueSurfaceProjection {
            generation: sigil_application::queue_generation(
                queue_revision.stream_sequence,
                &queue_revision.event_id,
            ),
            paused: queue_paused,
            items: queue_items
                .into_iter()
                .map(|item| {
                    Ok(ApplicationQueueItemProjection {
                        entry_id: safe_text(item.queued.queue_id.as_str())?,
                        target: application_queue_target(&item.queued.target)?,
                        kind: application_queue_item_kind(item.queued.kind),
                        status: safe_text(application_queue_item_status(item.status))?,
                        dispatchable: queue_next_dispatchable
                            .as_ref()
                            .is_some_and(|queue_id| queue_id == &item.queued.queue_id),
                    })
                })
                .collect::<Result<Vec<_>, ApplicationError>>()?,
        };
        let projection = ApplicationProjection {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: self.scope.clone(),
            writer_generation: self.writer_generation,
            stream_generation: self.stream_generation,
            observer_generation: self.observer_generation,
            frontier: frontier.clone(),
            resource_recovery: ResourceRecoverySurfaceContractV1 {
                schema_version: sigil_application::RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
                blocker: None,
                resource_effects: Vec::new(),
                action_envelope: None,
            },
            session: SessionSurfaceProjection {
                session_id: self.scope.session.clone(),
                title: safe_text(&format!("Session {}", self.expected_session_scope_id))?,
                status: safe_text(status)?,
            },
            conversation: ConversationSurfaceProjection {
                message_count: state.transcript.len() as u64,
                latest_message: Some(safe_text(latest_message)?),
            },
            run: RunSurfaceProjection {
                status: safe_text(status)?,
                active_binding: event_state.run_binding,
            },
            plan_task: PlanTaskSurfaceProjection {
                status: safe_text(event_state.plan_status)?,
                action_binding: event_state.plan_binding,
            },
            agents: AgentSurfaceProjection {
                active_count: event_state.active_agents,
                summary: event_state.agent_summary,
            },
            approval: sigil_application::ApprovalSurfaceProjection {
                pending: event_state.approval_pending,
                binding: event_state.approval_binding,
                summary: event_state.approval_summary,
            },
            user_input: UserInputSurfaceProjection {
                pending: event_state.user_input_pending,
                binding: event_state.user_input_binding,
                prompt: event_state.user_input_prompt,
            },
            capabilities: CapabilitySurfaceProjection {
                can_submit: !route_recovery && !event_state.run_active,
                can_cancel: event_state.run_active,
                can_configure: true,
            },
            configuration: ConfigurationSurfaceProjection {
                persisted_revision: 0,
                selected_route: Some(safe_text(&model_name)?),
                dirty: false,
            },
            attention: AttentionSurfaceProjection {
                last_notice: event_state.last_notice,
            },
            queue,
            terminal,
        };
        let envelope = ProjectionSnapshotEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: self.scope.clone(),
            writer_generation: self.writer_generation,
            stream_generation: self.stream_generation,
            observer_generation: self.observer_generation,
            cut: frontier,
            projection,
        };
        Ok(envelope)
    }
    #[cfg(test)]
    fn build_snapshot(
        &self,
        resume: Option<ApplicationFrontier>,
    ) -> Result<ProjectionSnapshot, ApplicationError> {
        self.snapshot_with_budget(resume, &sigil_kernel::SessionReadBudget::default())
    }
    fn snapshot_with_budget(
        &self,
        resume: Option<ApplicationFrontier>,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<ProjectionSnapshot, ApplicationError> {
        let owner = self.owner()?;
        let state = owner.state(budget)?;
        if let Some(frontier) = &resume {
            self.validate_frontier(frontier, state.sequence)?;
        }
        let envelope = self.envelope(&state)?;
        Ok(ProjectionSnapshot {
            envelope,
            feed: if resume.is_some() {
                vec![ProjectionFeedItem::CurrentState]
            } else {
                Vec::new()
            },
        })
    }
    #[cfg(test)]
    fn page_sync(
        &self,
        request: ProjectionPageRequest,
    ) -> Result<ProjectionPage, ApplicationError> {
        self.page_with_budget(request, &sigil_kernel::SessionReadBudget::default())
    }
    fn page_with_budget(
        &self,
        request: ProjectionPageRequest,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<ProjectionPage, ApplicationError> {
        if request.scope != self.scope || request.source_generation != self.source_generation {
            return Err(ApplicationError::ScopeMismatch);
        }
        if request.limit.get() > sigil_application::MAX_PAGE_ITEMS {
            return Err(ApplicationError::InvalidRequest(
                "page limit exceeds bound".into(),
            ));
        }
        let owner = self.owner()?;
        let mut state = owner.state(budget)?;
        self.validate_frontier(&request.at_frontier, state.sequence)?;
        if state.session_id.as_deref() != Some(&self.expected_session_scope_id) {
            return Err(ApplicationError::ScopeMismatch);
        }
        if request.direction != PageDirection::Older {
            return Err(ApplicationError::ResetRequired);
        }
        let before = request
            .anchor
            .cursor
            .as_ref()
            .map(parse_before_cursor)
            .transpose()?;
        let page = state.transcript_page(
            &owner,
            request.at_frontier.through_sequence,
            before,
            request.limit.get(),
            budget,
        )?;
        let items = page
            .messages
            .into_iter()
            .map(|message| {
                Ok(sigil_application::RendererSafeItem {
                    item_id: SessionItemId::new(message.message_id)?,
                    ordinal: message.ordinal,
                    role: match message.role {
                        crate::application_run::ApplicationTranscriptRole::User => "user",
                        crate::application_run::ApplicationTranscriptRole::Assistant => "assistant",
                        crate::application_run::ApplicationTranscriptRole::Tool => "tool",
                    }
                    .into(),
                    text: message
                        .content
                        .as_deref()
                        .filter(|content| !content.is_empty())
                        .map(safe_text)
                        .transpose()?,
                    estimated_height: 1,
                })
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        let before = page
            .next_before
            .map(|ordinal| StablePageCursor::new(format!("before:{ordinal}")))
            .transpose()?;
        Ok(ProjectionPage {
            request_id: request.request_id,
            scope: request.scope,
            source_generation: request.source_generation,
            at_frontier: request.at_frontier,
            query: request.query,
            before,
            after: None,
            total: page.total_messages,
            items,
        })
    }
    fn acknowledge_with_budget(
        &self,
        event_ids: &[String],
        frontier: &ApplicationFrontier,
        budget: &sigil_kernel::SessionReadBudget,
    ) -> Result<usize, ApplicationError> {
        let owner = self.owner()?;
        let recorder = owner
            .delivery_recorder
            .as_ref()
            .ok_or(ApplicationError::Unavailable)?;
        let ids = {
            let state = owner.state(budget)?;
            self.validate_frontier(frontier, state.sequence)?;
            if state.session_id.as_deref() != Some(&self.expected_session_scope_id) {
                return Err(ApplicationError::ScopeMismatch);
            }
            let ids = event_ids.to_owned();
            for id in &ids {
                let index = *state
                    .public_positions
                    .get(id)
                    .ok_or(ApplicationError::ResetRequired)?;
                if state.public[index].1.sequence > frontier.through_sequence {
                    return Err(ApplicationError::ResetRequired);
                }
            }
            ids.into_iter()
                .filter(|id| !state.delivery.was_delivered(id, "tui"))
                .collect::<Vec<_>>()
        };
        let mut acknowledged = 0;
        for batch in ids.chunks(PUBLIC_EVENT_DELIVERY_BATCH_MAX_RECORDS) {
            budget.check().map_err(unavailable)?;
            let receipts = batch
                .iter()
                .map(|id| PublicEventDeliveryReceiptV1 {
                    schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                    public_event_id: id.clone(),
                    adapter: "tui".into(),
                    delivered_at_unix_ms: crate::current_unix_time_ms(),
                })
                .collect::<Vec<_>>();
            acknowledged += recorder
                .append_delivery_batch_with_budget(&receipts, budget)
                .map_err(unavailable)?;
        }
        Ok(acknowledged)
    }
    pub async fn acknowledge_tui_public_events(
        &self,
        event_ids: &[String],
        frontier: &ApplicationFrontier,
    ) -> Result<usize, ApplicationError> {
        let binding = self.clone();
        let ids = event_ids.to_owned();
        let frontier = frontier.clone();
        observation(binding.observation_counter(), move |budget| {
            binding.acknowledge_with_budget(&ids, &frontier, &budget)
        })
        .await
    }
}

struct ObservationGuard(sigil_kernel::SessionReadBudget);
impl Drop for ObservationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct PendingObservation(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for PendingObservation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
async fn observation<T: Send + 'static>(
    counter: Arc<std::sync::atomic::AtomicUsize>,
    work: impl FnOnce(sigil_kernel::SessionReadBudget) -> Result<T, ApplicationError> + Send + 'static,
) -> Result<T, ApplicationError> {
    let guard = ObservationGuard(sigil_kernel::SessionReadBudget::default());
    let budget = guard.0.clone();
    counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let pending = PendingObservation(counter);
    let result = tokio::task::spawn_blocking(move || {
        let _pending = pending;
        work(budget)
    })
    .await
    .map_err(unavailable)?;
    drop(guard);
    result
}

#[cfg(test)]
fn terminal_surface_projection(
    records: &[sigil_kernel::SessionStreamRecord],
) -> Result<TerminalSurfaceProjection, ApplicationError> {
    let mut entries = Vec::new();
    for record in records {
        let Some(SessionLogEntry::Control(ControlEntry::TerminalTask(entry))) =
            record.session_log_entry().map_err(|_| {
                ApplicationError::CorruptProjection(
                    "invalid terminal task session entry".to_owned(),
                )
            })?
        else {
            continue;
        };
        entries.push(SessionLogEntry::Control(ControlEntry::TerminalTask(entry)));
    }

    let projection = TerminalTaskProjection::from_entries(&entries);
    terminal_surface_from_projection(&projection)
}

fn terminal_surface_from_projection(
    projection: &TerminalTaskProjection,
) -> Result<TerminalSurfaceProjection, ApplicationError> {
    if projection.tasks.len() > sigil_application::MAX_TERMINAL_TASKS {
        return Err(ApplicationError::CorruptProjection(
            "terminal task projection exceeds application bound".to_owned(),
        ));
    }
    let tasks = projection
        .tasks
        .values()
        .map(|task| {
            Ok(ApplicationTerminalTaskProjection {
                task_id: safe_text(task.handle.task_id.as_str())?,
                generation: task.generation,
                status: safe_text(task.status.as_str())?,
                readiness: safe_text(terminal_readiness_label(&task.readiness))?,
                output_total_bytes: task.output_total_bytes,
                output_truncated: task.output_truncated,
                output_hash: task.output_hash.as_deref().map(safe_text).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, ApplicationError>>()?;
    let active_task_count = u16::try_from(projection.active_task_ids.len()).map_err(|_| {
        ApplicationError::CorruptProjection("active terminal task count exceeds bound".to_owned())
    })?;
    let latest_task_id = projection
        .latest_task_id
        .as_ref()
        .map(|task_id| safe_text(task_id.as_str()))
        .transpose()?;
    Ok(TerminalSurfaceProjection {
        tasks,
        active_task_count,
        latest_task_id,
    })
}

fn terminal_readiness_label(readiness: &TerminalReadinessStatus) -> &'static str {
    match readiness {
        TerminalReadinessStatus::None => "none",
        TerminalReadinessStatus::Waiting { .. } => "waiting",
        TerminalReadinessStatus::Ready { .. } => "ready",
        TerminalReadinessStatus::Failed { .. } => "failed",
        TerminalReadinessStatus::TimedOut { .. } => "timed_out",
    }
}

#[derive(Clone)]
struct ProjectionEventState {
    run_status: &'static str,
    run_active: bool,
    run_binding: Option<String>,
    plan_status: &'static str,
    plan_binding: Option<String>,
    active_agents: u16,
    agent_summary: Vec<SafeText>,
    approval_pending: bool,
    approval_binding: Option<String>,
    approval_summary: Option<SafeText>,
    user_input_pending: bool,
    user_input_binding: Option<String>,
    user_input_prompt: Option<SafeText>,
    revision_waiting_run_id: Option<String>,
    last_notice: Option<SafeText>,
}

impl ProjectionEventState {
    fn new() -> Self {
        Self {
            run_status: "idle",
            run_active: false,
            run_binding: None,
            plan_status: "none",
            plan_binding: None,
            active_agents: 0,
            agent_summary: Vec::new(),
            approval_pending: false,
            approval_binding: None,
            approval_summary: None,
            user_input_pending: false,
            user_input_binding: None,
            user_input_prompt: None,
            revision_waiting_run_id: None,
            last_notice: None,
        }
    }
    #[cfg(test)]
    fn from_events(events: &[&PublicEventOutboxEntryV1], waiting: &BTreeSet<String>) -> Self {
        let mut state = Self::new();
        for entry in events {
            state.apply_event(entry, waiting.contains(&entry.public_event_id));
        }
        state
    }
    fn apply_event(&mut self, entry: &PublicEventOutboxEntryV1, revision_waiting: bool) {
        let state = self;
        match &entry.event.event {
            PublicRunEventKind::RunStarted { .. }
            | PublicRunEventKind::TaskRunStarted { .. }
            | PublicRunEventKind::TaskPhaseChanged { .. }
            | PublicRunEventKind::TaskExecutionAdmitted { .. } => {
                state.run_status = "running";
                state.run_active = true;
                state.run_binding = Some(entry.run_id.clone());
                if state.revision_waiting_run_id.as_deref() == Some(&entry.run_id) {
                    state.user_input_pending = false;
                    state.user_input_binding = None;
                    state.user_input_prompt = None;
                    state.plan_status = "started";
                    state.revision_waiting_run_id = None;
                }
            }
            PublicRunEventKind::RunFinished { .. }
            | PublicRunEventKind::TaskRunFinished { .. }
            | PublicRunEventKind::RunCancelled
            | PublicRunEventKind::RunPaused { .. }
            | PublicRunEventKind::RunInterrupted { .. }
            | PublicRunEventKind::RunFailed { .. }
            | PublicRunEventKind::RunBlocked { .. } => {
                state.run_status = match &entry.event.event {
                    PublicRunEventKind::RunFinished { .. }
                    | PublicRunEventKind::TaskRunFinished { .. } => "finished",
                    PublicRunEventKind::RunCancelled => "cancelled",
                    PublicRunEventKind::RunPaused { .. } => "paused",
                    PublicRunEventKind::RunInterrupted { .. } => "interrupted",
                    PublicRunEventKind::RunFailed { .. } => "failed",
                    PublicRunEventKind::RunBlocked { .. } => "blocked",
                    _ => unreachable!("terminal event classification is exhaustive"),
                };
                state.run_active = false;
                state.run_binding = None;
            }
            PublicRunEventKind::ApprovalRequested {
                approval_identity,
                safe_summary,
                ..
            } => {
                state.approval_pending = true;
                state.approval_binding = Some(format!(
                    "{}:{}:{}",
                    approval_identity.run_id,
                    approval_identity.call_id,
                    approval_identity.approval_request_id
                ));
                state.approval_summary = safe_text(&safe_summary.title).ok();
            }
            PublicRunEventKind::ApprovalResolved { .. } => {
                state.approval_pending = false;
                state.approval_binding = None;
                state.approval_summary = None;
            }
            PublicRunEventKind::RunAwaitingUserInput {
                request_id,
                generation,
                request_hash,
            } => {
                // A root AwaitingUserInput is terminal, while a revision Waiting pair is a
                // resumable attempt suspension. The exact PlanReviewAttempt/outbox pairing
                // selects the latter; the public event kind alone never decides it.
                state.run_status = "awaiting-user-input";
                state.run_active = false;
                state.run_binding = None;
                state.user_input_pending = true;
                state.user_input_binding =
                    Some(format!("{request_id}:{generation}:{request_hash}"));
                if revision_waiting {
                    state.plan_status = "waiting-for-input";
                    state.revision_waiting_run_id = Some(entry.run_id.clone());
                }
            }
            PublicRunEventKind::UserInputChanged {
                request_id,
                generation,
                request_hash,
                status,
                request,
            } => {
                state.user_input_pending =
                    !matches!(status, sigil_kernel::UserInputStatusV1::Resolved);
                state.user_input_binding = state
                    .user_input_pending
                    .then(|| format!("{request_id}:{generation}:{request_hash}"));
                state.user_input_prompt = state
                    .user_input_pending
                    .then(|| safe_text(&request.prompt).ok())
                    .flatten();
            }
            PublicRunEventKind::Notice { message: text } => {
                state.last_notice = safe_text(text).ok();
            }
            PublicRunEventKind::TaskRoutingChanged { status, .. }
            | PublicRunEventKind::IntegrationLaneChanged { status, .. } => {
                state.active_agents = 1;
                state.agent_summary = safe_text(status).ok().into_iter().collect();
            }
            PublicRunEventKind::PlanReviewChanged {
                plan_id, status, ..
            } => {
                state.plan_status = plan_status_label(status);
                state.plan_binding = Some(plan_id.clone());
            }
            _ => {}
        }
    }
}

fn plan_status_label(status: &sigil_kernel::PublicPlanReviewStatus) -> &'static str {
    match status {
        sigil_kernel::PublicPlanReviewStatus::Started => "started",
        sigil_kernel::PublicPlanReviewStatus::WaitingForInput => "waiting-for-input",
        sigil_kernel::PublicPlanReviewStatus::DraftReady => "draft-ready",
        sigil_kernel::PublicPlanReviewStatus::CompileFailed => "compile-failed",
        sigil_kernel::PublicPlanReviewStatus::CompletedWithoutDraft => "completed-without-draft",
        sigil_kernel::PublicPlanReviewStatus::Blocked => "blocked",
        sigil_kernel::PublicPlanReviewStatus::Paused => "paused",
        sigil_kernel::PublicPlanReviewStatus::Failed => "failed",
        sigil_kernel::PublicPlanReviewStatus::Interrupted => "interrupted",
        sigil_kernel::PublicPlanReviewStatus::Cancelled => "cancelled",
    }
}

impl crate::RuntimeApplicationProjectionSource for RuntimeSessionProjectionBinding {
    fn validate_recovery_scope(&self, scope: &ApplicationScope) -> Result<(), ApplicationError> {
        if scope == &self.scope {
            Ok(())
        } else {
            Err(ApplicationError::ScopeMismatch)
        }
    }
    fn delivery_batch(
        &self,
        request: sigil_application::DurableDeliveryRequest,
    ) -> BoxFuture<'static, Result<sigil_application::DurableDeliveryBatch, ApplicationError>> {
        let binding = self.clone();
        Box::pin(async move {
            observation(binding.observation_counter(), move |budget| {
                if request.observer_generation != binding.observer_generation {
                    return Err(ApplicationError::ScopeMismatch);
                }
                let owner = binding.owner()?;
                let mut state = owner.state(&budget)?;
                if state.session_id.as_deref() != Some(&binding.expected_session_scope_id) {
                    return Err(ApplicationError::ScopeMismatch);
                }
                binding.validate_frontier(&request.frontier, state.sequence)?;
                if request.after_sequence > request.frontier.through_sequence {
                    return Err(ApplicationError::ResetRequired);
                }
                let start = state
                    .public
                    .partition_point(|(_, position)| position.sequence <= request.after_sequence);
                let end = state.public.partition_point(|(_, position)| {
                    position.sequence <= request.frontier.through_sequence
                });
                let mut events = Vec::new();
                let mut encoded = 0;
                let mut raw = 0;
                let mut next = start;
                for index in start..end.min(start + 256) {
                    budget.check().map_err(unavailable)?;
                    let (id, position) = state.public[index].clone();
                    if state.delivery.was_delivered(&id, "tui") {
                        next = index + 1;
                        continue;
                    }
                    if raw + position.end - position.offset > MAX_PROJECTION_RANGE_BYTES as u64 {
                        break;
                    }
                    let record = state.read_position(&owner, &position, &budget, false)?;
                    raw += position.end - position.offset;
                    let entry: PublicEventOutboxEntryV1 =
                        serde_json::from_value(record.stored_event().payload.clone())
                            .map_err(corrupt)?;
                    if entry.public_event_id != id {
                        return Err(ApplicationError::ResetRequired);
                    }
                    let bytes = serde_json::to_vec(&entry.event).map_err(corrupt)?.len();
                    if encoded + bytes > 1024 * 1024 {
                        break;
                    }
                    encoded += bytes;
                    events.push(sigil_application::DurableDeliveryEvent {
                        stream_sequence: position.sequence,
                        public_event_id: id,
                        payload_digest: entry.payload_digest,
                        event: entry.event,
                    });
                    next = index + 1;
                }
                let has_more = next < end;
                let through_sequence = if has_more {
                    next.checked_sub(1)
                        .map(|index| state.public[index].1.sequence)
                        .unwrap_or(request.after_sequence)
                } else {
                    request.frontier.through_sequence
                };
                Ok(sigil_application::DurableDeliveryBatch {
                    request,
                    through_sequence,
                    events,
                    has_more,
                })
            })
            .await
        })
    }
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let binding = self.clone();
        Box::pin(async move {
            if request.scope != binding.scope
                || request.observer_generation != binding.observer_generation
            {
                return Err(ApplicationError::ScopeMismatch);
            }
            observation(binding.observation_counter(), move |budget| {
                let snapshot = binding.snapshot_with_budget(request.resume_from, &budget)?;
                snapshot.envelope.validate()?;
                Ok(snapshot)
            })
            .await
        })
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        let binding = self.clone();
        Box::pin(async move {
            observation(binding.observation_counter(), move |budget| {
                binding.page_with_budget(request, &budget)
            })
            .await
        })
    }
}

fn parse_before_cursor(cursor: &StablePageCursor) -> Result<u64, ApplicationError> {
    let value = cursor
        .as_str()
        .strip_prefix("before:")
        .ok_or_else(|| ApplicationError::InvalidRequest("unknown page cursor".to_owned()))?;
    value
        .parse::<u64>()
        .ok()
        .filter(|ordinal| *ordinal > 0)
        .ok_or_else(|| ApplicationError::InvalidRequest("invalid page cursor".to_owned()))
}

fn safe_text(value: &str) -> Result<SafeText, ApplicationError> {
    SafeText::new(value.to_owned())
}

fn application_queue_item_kind(
    kind: sigil_kernel::ConversationInputKind,
) -> ApplicationQueueItemKind {
    match kind {
        sigil_kernel::ConversationInputKind::Chat => ApplicationQueueItemKind::Chat,
        sigil_kernel::ConversationInputKind::PlanPrompt => ApplicationQueueItemKind::PlanPrompt,
        sigil_kernel::ConversationInputKind::AgentMention => ApplicationQueueItemKind::AgentMention,
        sigil_kernel::ConversationInputKind::AgentMessage => ApplicationQueueItemKind::AgentMessage,
        sigil_kernel::ConversationInputKind::TaskGuidance => ApplicationQueueItemKind::TaskGuidance,
        sigil_kernel::ConversationInputKind::Unknown => ApplicationQueueItemKind::Unknown,
    }
}

fn application_queue_target(
    target: &sigil_kernel::ConversationInputTarget,
) -> Result<ApplicationQueueTarget, ApplicationError> {
    match target {
        sigil_kernel::ConversationInputTarget::MainThread => Ok(ApplicationQueueTarget::MainThread),
        sigil_kernel::ConversationInputTarget::AgentThread { thread_id } => {
            Ok(ApplicationQueueTarget::AgentThread {
                thread_id: safe_text(thread_id.as_str())?,
            })
        }
        sigil_kernel::ConversationInputTarget::Task { task_id } => {
            Ok(ApplicationQueueTarget::Task {
                task_id: safe_text(task_id.as_str())?,
            })
        }
    }
}

fn application_queue_item_status(status: sigil_kernel::ConversationInputStatus) -> &'static str {
    match status {
        sigil_kernel::ConversationInputStatus::Queued => "queued",
        sigil_kernel::ConversationInputStatus::Dispatching => "dispatching",
        sigil_kernel::ConversationInputStatus::Delivered => "delivered",
        sigil_kernel::ConversationInputStatus::Rejected => "rejected",
        sigil_kernel::ConversationInputStatus::Cancelled => "cancelled",
        sigil_kernel::ConversationInputStatus::Stale => "stale",
        sigil_kernel::ConversationInputStatus::Unknown => "unknown",
    }
}

#[cfg(test)]
#[path = "tests/application_projection_tests.rs"]
mod tests;
