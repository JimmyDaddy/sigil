//! Durable-session adapter for the transport-neutral application projection.

use std::path::PathBuf;

use futures::future::BoxFuture;
use sigil_application::{
    APPLICATION_CONTRACT_SCHEMA_VERSION, AgentSurfaceProjection, ApplicationError,
    ApplicationFrontier, ApplicationInstanceId, ApplicationProjection, ApplicationScope,
    AttentionSurfaceProjection, CapabilitySurfaceProjection, ConfigurationSurfaceProjection,
    ConversationSurfaceProjection, OpenProjectionRequest, PageDirection, PlanTaskSurfaceProjection,
    ProjectionPage, ProjectionPageRequest, ProjectionSnapshot, ProjectionSnapshotEnvelope,
    ResourceRecoverySurfaceContractV1, RunSurfaceProjection, SafeText, SessionItemId,
    SessionScopeId, SessionSurfaceProjection, StablePageCursor, UserInputSurfaceProjection,
};

use crate::application_run::{
    application_run_context_view, application_session_frontier_view,
    application_session_transcript_page,
};

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

    fn build_projection(&self) -> Result<ProjectionSnapshotEnvelope, ApplicationError> {
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
        let frontier = self.current_frontier()?;
        let route_recovery = context.route_recovery.as_ref();
        let status = if route_recovery.is_some() {
            "recovery-required"
        } else {
            "ready"
        };
        let latest_message = transcript
            .messages
            .last()
            .and_then(|message| message.content.as_deref())
            .unwrap_or("No messages yet");
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
                status: safe_text("idle")?,
                active_binding: None,
            },
            plan_task: PlanTaskSurfaceProjection {
                status: safe_text("none")?,
                action_binding: None,
            },
            agents: AgentSurfaceProjection {
                active_count: 0,
                summary: Vec::new(),
            },
            approval: sigil_application::ApprovalSurfaceProjection {
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
                can_submit: route_recovery.is_none(),
                can_cancel: false,
                can_configure: true,
            },
            configuration: ConfigurationSurfaceProjection {
                persisted_revision: 0,
                selected_route: Some(safe_text(&context.model_name)?),
                dirty: false,
            },
            attention: AttentionSurfaceProjection { last_notice: None },
        };
        Ok(ProjectionSnapshotEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope: self.scope.clone(),
            writer_generation: self.writer_generation,
            stream_generation: self.stream_generation,
            observer_generation: self.observer_generation,
            cut: frontier,
            projection,
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
                let envelope = binding.build_projection()?;
                envelope.validate().map_err(|_| {
                    ApplicationError::CorruptProjection("invalid runtime projection".to_owned())
                })?;
                Ok(ProjectionSnapshot {
                    envelope,
                    feed: Vec::new(),
                })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_before_cursor_is_strictly_positive() {
        let cursor = StablePageCursor::new("before:4").expect("cursor");
        assert_eq!(parse_before_cursor(&cursor).expect("parse"), 4);
        let invalid = StablePageCursor::new("before:0").expect("cursor");
        assert!(parse_before_cursor(&invalid).is_err());
    }
}
