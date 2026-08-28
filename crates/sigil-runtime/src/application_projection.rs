//! Durable-session adapter for the transport-neutral application projection.

use std::path::PathBuf;

use futures::future::BoxFuture;
use sigil_application::{
    APPLICATION_CONTRACT_SCHEMA_VERSION, AgentSurfaceProjection, ApplicationError,
    ApplicationEvent, ApplicationEventEnvelope, ApplicationFrontier, ApplicationInstanceId,
    ApplicationProjection, ApplicationQueueItemKind, ApplicationQueueItemProjection,
    ApplicationQueueSurfaceProjection, ApplicationQueueTarget, ApplicationScope,
    AttentionSurfaceProjection, CapabilitySurfaceProjection, ConfigurationSurfaceProjection,
    ConversationSurfaceProjection, OpenProjectionRequest, PageDirection, PlanTaskSurfaceProjection,
    ProjectionFeedItem, ProjectionPage, ProjectionPageRequest, ProjectionSnapshot,
    ProjectionSnapshotEnvelope, ResourceRecoverySurfaceContractV1, RunSurfaceProjection, SafeText,
    SessionItemId, SessionScopeId, SessionSurfaceProjection, StablePageCursor,
    UserInputSurfaceProjection,
};
use sigil_kernel::{
    ConversationQueueDurableProjection, JsonlSessionStore, PublicEventOutboxEntryV1,
    PublicEventOutboxProjectionV1, PublicRunEventKind,
};

use crate::application_run::{
    application_run_context_view, application_session_frontier_view,
    application_session_transcript_page,
};

const MAX_PROJECTION_FEED_ITEMS: usize = 256;

type ProjectionBuild = (
    ProjectionSnapshotEnvelope,
    Vec<sigil_kernel::SessionStreamRecord>,
    Vec<(u64, PublicEventOutboxEntryV1)>,
);

/// Runtime-owned binding used to construct an application projection without exposing its durable
/// paths to the application contract or renderer.
#[derive(Debug, Clone)]
pub struct RuntimeSessionProjectionBinding {
    config_path: PathBuf,
    launch_cwd: PathBuf,
    session_path: PathBuf,
    expected_session_scope_id: String,
    scope: ApplicationScope,
    writer_generation: u64,
    stream_generation: u64,
    observer_generation: u64,
    source_generation: u64,
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
            launch_cwd,
            session_path,
            expected_session_scope_id,
            scope,
            writer_generation,
            stream_generation,
            observer_generation,
            source_generation,
        })
    }

    pub fn scope(&self) -> &ApplicationScope {
        &self.scope
    }

    fn current_frontier(&self) -> Result<ApplicationFrontier, ApplicationError> {
        let view =
            application_session_frontier_view(&self.session_path, &self.expected_session_scope_id)
                .map_err(|_| ApplicationError::Unavailable)?;
        Ok(ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: self.scope.clone(),
            writer_generation: self.writer_generation,
            stream_generation: self.stream_generation,
            through_sequence: view.through_stream_sequence,
            durable_cursor: format!("session-stream:{}", view.through_stream_sequence),
        })
    }

    fn build_projection(&self) -> Result<ProjectionBuild, ApplicationError> {
        let records = JsonlSessionStore::read_event_records(&self.session_path)
            .map_err(|_| ApplicationError::Unavailable)?;
        let actual_session_scope_id = records
            .first()
            .map(|record| record.session_id())
            .ok_or(ApplicationError::Unavailable)?;
        if actual_session_scope_id != self.expected_session_scope_id
            || records
                .iter()
                .any(|record| record.session_id() != self.expected_session_scope_id)
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        let last_sequence = records
            .last()
            .map(sigil_kernel::SessionStreamRecord::stream_sequence)
            .ok_or(ApplicationError::Unavailable)?;
        let frontier = ApplicationFrontier {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: self.scope.clone(),
            writer_generation: self.writer_generation,
            stream_generation: self.stream_generation,
            through_sequence: last_sequence,
            durable_cursor: format!("session-stream:{last_sequence}"),
        };
        let context = application_run_context_view(
            &self.config_path,
            &self.launch_cwd,
            &self.session_path,
            &self.expected_session_scope_id,
        )
        .map_err(|_| ApplicationError::Unavailable)?;
        let transcript = application_session_transcript_page(
            &self.session_path,
            &self.expected_session_scope_id,
            None,
            1,
        )
        .map_err(|_| ApplicationError::Unavailable)?;
        let public_events =
            PublicEventOutboxProjectionV1::from_records(&records).map_err(|_| {
                ApplicationError::CorruptProjection("invalid public event outbox".to_owned())
            })?;
        let public_stream_events = records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind()
                    == Some(sigil_kernel::DurableEventType::PublicEventOutbox)
            })
            .map(|record| {
                let entry = serde_json::from_value::<PublicEventOutboxEntryV1>(
                    record.stored_event().payload.clone(),
                )
                .map_err(|_| {
                    ApplicationError::CorruptProjection(
                        "invalid public event outbox payload".to_owned(),
                    )
                })?;
                if public_events.entry(&entry.public_event_id).is_none() {
                    return Err(ApplicationError::CorruptProjection(
                        "public event outbox projection lost its entry".to_owned(),
                    ));
                }
                Ok((record.stream_sequence(), entry))
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        let events = public_stream_events
            .iter()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        let route_recovery = context.route_recovery.as_ref();
        let event_state = ProjectionEventState::from_events(&events);
        let status = route_recovery
            .map(|_| "recovery-required")
            .unwrap_or(event_state.run_status);
        let latest_message = transcript
            .messages
            .last()
            .and_then(|message| message.content.as_deref())
            .unwrap_or("No messages yet");
        let queue = ConversationQueueDurableProjection::from_records(&records).map_err(|_| {
            ApplicationError::CorruptProjection("invalid conversation queue projection".to_owned())
        })?;
        let queue_revision = queue.current_revision();
        let queue_next_dispatchable = queue.queue.next_dispatchable.clone();
        let queue_paused = queue.queue.paused;
        let queue_items = queue.queue.items;
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
                message_count: transcript.total_messages,
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
                can_submit: route_recovery.is_none() && !event_state.run_active,
                can_cancel: event_state.run_active,
                can_configure: true,
            },
            configuration: ConfigurationSurfaceProjection {
                persisted_revision: 0,
                selected_route: Some(safe_text(&context.model_name)?),
                dirty: false,
            },
            attention: AttentionSurfaceProjection {
                last_notice: event_state.last_notice,
            },
            queue,
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
        Ok((envelope, records, public_stream_events))
    }

    fn build_snapshot(
        &self,
        resume_from: Option<ApplicationFrontier>,
    ) -> Result<ProjectionSnapshot, ApplicationError> {
        let (current, records, public_events) = self.build_projection()?;
        let Some(resume_from) = resume_from else {
            return Ok(ProjectionSnapshot {
                envelope: current,
                feed: Vec::new(),
            });
        };
        if resume_from.scope != self.scope
            || resume_from.writer_generation != self.writer_generation
            || resume_from.stream_generation != self.stream_generation
        {
            return Err(ApplicationError::ResetRequired);
        }
        if resume_from.through_sequence > current.cut.through_sequence {
            return Err(ApplicationError::ResetRequired);
        }
        if resume_from.through_sequence > 0
            && !records
                .iter()
                .any(|record| record.stream_sequence() == resume_from.through_sequence)
        {
            return Err(ApplicationError::ResetRequired);
        }

        let all_public = public_events
            .iter()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        let prefix_public = public_events
            .iter()
            .filter(|(sequence, _)| *sequence <= resume_from.through_sequence)
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        let mut base_projection = current.projection.clone();
        let base_state = ProjectionEventState::from_events(&prefix_public);
        apply_projection_event_state(&mut base_projection, &base_state)?;
        base_projection.frontier = resume_from.clone();
        let mut base = ProjectionSnapshotEnvelope {
            schema_version: current.schema_version,
            scope: current.scope.clone(),
            writer_generation: current.writer_generation,
            stream_generation: current.stream_generation,
            observer_generation: current.observer_generation,
            cut: resume_from,
            projection: base_projection,
        };
        base.validate()?;

        let mut feed = Vec::new();
        let mut public_index = prefix_public.len();
        let resume_sequence = base.cut.through_sequence;
        for record in records
            .iter()
            .filter(|record| record.stream_sequence() > resume_sequence)
        {
            if feed.len() == MAX_PROJECTION_FEED_ITEMS {
                return Ok(ProjectionSnapshot {
                    envelope: base,
                    feed: vec![ProjectionFeedItem::ResetRequired {
                        reason: "projection-feed-overflow",
                    }],
                });
            }
            let expected_sequence = base.cut.through_sequence.saturating_add(1);
            if record.stream_sequence() != expected_sequence {
                return Ok(ProjectionSnapshot {
                    envelope: base,
                    feed: vec![ProjectionFeedItem::Gap {
                        expected: expected_sequence,
                        observed: record.stream_sequence(),
                    }],
                });
            }
            if public_index < all_public.len()
                && public_events[public_index].0 == record.stream_sequence()
            {
                public_index += 1;
            }
            let state = ProjectionEventState::from_events(&all_public[..public_index]);
            let mut next_projection = base.projection.clone();
            apply_projection_event_state(&mut next_projection, &state)?;
            let next_frontier = ApplicationFrontier {
                through_sequence: record.stream_sequence(),
                durable_cursor: format!("session-stream:{}", record.stream_sequence()),
                ..base.cut.clone()
            };
            next_projection.frontier = next_frontier.clone();
            let payload = ApplicationEvent::ProjectionReplaced(Box::new(next_projection.clone()));
            let event = ApplicationEventEnvelope {
                schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
                scope: self.scope.clone(),
                writer_generation: self.writer_generation,
                stream_generation: self.stream_generation,
                observer_generation: self.observer_generation,
                event_id: format!(
                    "application-stream:{}:{}",
                    record.stream_sequence(),
                    record.event_id()
                ),
                base_frontier: base.cut.clone(),
                next_frontier: next_frontier.clone(),
                payload_digest: sigil_application::event_payload_digest(&payload)?,
                payload,
            };
            feed.push(ProjectionFeedItem::Event(Box::new(event)));
            base.cut = next_frontier;
            base.projection = next_projection;
        }
        Ok(ProjectionSnapshot {
            envelope: base,
            feed,
        })
    }

    fn page_sync(
        &self,
        request: ProjectionPageRequest,
    ) -> Result<ProjectionPage, ApplicationError> {
        if request.scope != self.scope || request.source_generation != self.source_generation {
            return Err(ApplicationError::ScopeMismatch);
        }
        if request.at_frontier != self.current_frontier()? {
            return Err(ApplicationError::ResetRequired);
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
        let page = application_session_transcript_page(
            &self.session_path,
            &self.expected_session_scope_id,
            before,
            request.limit.get(),
        )
        .map_err(|_| ApplicationError::Unavailable)?;
        let items = page
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    crate::application_run::ApplicationTranscriptRole::User => "user",
                    crate::application_run::ApplicationTranscriptRole::Assistant => "assistant",
                    crate::application_run::ApplicationTranscriptRole::Tool => "tool",
                };
                let item_id = SessionItemId::new(message.message_id)?;
                let text = message
                    .content
                    .map(|content| safe_text(&content))
                    .transpose()?;
                Ok(sigil_application::RendererSafeItem {
                    item_id,
                    ordinal: message.ordinal,
                    role: role.to_owned(),
                    text,
                    estimated_height: 1,
                })
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        let before = page.next_before.map(|ordinal| {
            StablePageCursor::new(format!("before:{ordinal}"))
                .expect("generated before cursor is bounded and non-empty")
        });
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
    last_notice: Option<SafeText>,
}

fn apply_projection_event_state(
    projection: &mut ApplicationProjection,
    state: &ProjectionEventState,
) -> Result<(), ApplicationError> {
    let status = if projection.session.status.as_str() == "recovery-required" {
        "recovery-required"
    } else {
        state.run_status
    };
    projection.session.status = safe_text(status)?;
    projection.run.status = safe_text(status)?;
    projection.run.active_binding = state.run_binding.clone();
    projection.plan_task.status = safe_text(state.plan_status)?;
    projection.plan_task.action_binding = state.plan_binding.clone();
    projection.agents.active_count = state.active_agents;
    projection.agents.summary = state.agent_summary.clone();
    projection.approval.pending = state.approval_pending;
    projection.approval.binding = state.approval_binding.clone();
    projection.approval.summary = state.approval_summary.clone();
    projection.user_input.pending = state.user_input_pending;
    projection.user_input.binding = state.user_input_binding.clone();
    projection.user_input.prompt = state.user_input_prompt.clone();
    projection.capabilities.can_submit = status != "recovery-required" && !state.run_active;
    projection.capabilities.can_cancel = state.run_active;
    projection.attention.last_notice = state.last_notice.clone();
    Ok(())
}

impl ProjectionEventState {
    fn from_events(events: &[&sigil_kernel::PublicEventOutboxEntryV1]) -> Self {
        let mut state = Self {
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
            last_notice: None,
        };
        for entry in events {
            match &entry.event.event {
                PublicRunEventKind::RunStarted { .. }
                | PublicRunEventKind::TaskRunStarted { .. }
                | PublicRunEventKind::TaskPhaseChanged { .. }
                | PublicRunEventKind::TaskExecutionAdmitted { .. } => {
                    state.run_status = "running";
                    state.run_active = true;
                    state.run_binding = Some(entry.run_id.clone());
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
                    state.user_input_pending = true;
                    state.user_input_binding =
                        Some(format!("{request_id}:{generation}:{request_hash}"));
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
        state
    }
}

fn plan_status_label(status: &sigil_kernel::PublicPlanReviewStatus) -> &'static str {
    match status {
        sigil_kernel::PublicPlanReviewStatus::Started => "started",
        sigil_kernel::PublicPlanReviewStatus::WaitingForInput => "waiting-for-input",
        sigil_kernel::PublicPlanReviewStatus::Finalizing => "finalizing",
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
            tokio::task::spawn_blocking(move || {
                let snapshot = binding.build_snapshot(request.resume_from)?;
                snapshot.envelope.validate().map_err(|_| {
                    ApplicationError::CorruptProjection("invalid runtime projection".to_owned())
                })?;
                Ok(snapshot)
            })
            .await
            .map_err(|_| ApplicationError::Unavailable)?
        })
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        let binding = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || binding.page_sync(request))
                .await
                .map_err(|_| ApplicationError::Unavailable)?
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
mod tests {
    use super::*;

    fn outbox_entry(
        sequence: u64,
        event: PublicRunEventKind,
    ) -> sigil_kernel::PublicEventOutboxEntryV1 {
        let event = sigil_kernel::PublicRunEvent::new("session-1", "run-1", sequence, event);
        sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: format!("event-{sequence}"),
            domain_event_id: format!("domain-{sequence}"),
            run_id: event.run_id.clone(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(
                serde_json::to_vec(&event).expect("public event encodes"),
            ),
            event,
        }
    }

    #[test]
    fn generated_before_cursor_is_strictly_positive() {
        let cursor = StablePageCursor::new("before:4").expect("cursor");
        assert_eq!(parse_before_cursor(&cursor).expect("parse"), 4);
        let invalid = StablePageCursor::new("before:0").expect("cursor");
        assert!(parse_before_cursor(&invalid).is_err());
    }

    #[test]
    fn projection_state_rebuilds_from_delivered_history() {
        let started = outbox_entry(
            1,
            PublicRunEventKind::RunStarted {
                prompt: "run".into(),
            },
        );
        let notice = outbox_entry(
            2,
            PublicRunEventKind::Notice {
                message: "checkpoint".into(),
            },
        );
        let finished = outbox_entry(
            3,
            PublicRunEventKind::RunFinished {
                final_text: "done".into(),
            },
        );
        let mut entries = vec![&finished, &started, &notice];
        entries.sort_by_key(|entry| entry.sequence);

        let state = ProjectionEventState::from_events(&entries);
        assert_eq!(state.run_status, "finished");
        assert!(!state.run_active);
        assert_eq!(
            state.last_notice.as_ref().map(SafeText::as_str),
            Some("checkpoint")
        );
    }
}
