use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

use reqwest::{Client, RequestBuilder, Response, StatusCode, Url, header};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    dto::{
        DESKTOP_CONVERSATION_DISPLAY_SCHEMA_VERSION, DESKTOP_CONVERSATION_QUEUE_SCHEMA_VERSION,
        DESKTOP_HTTP_PROTOCOL_VERSION, DesktopApprovalCommandReceipt,
        DesktopApprovalDecisionRequest, DesktopCatalogQuery, DesktopCheckpointRestoreRequest,
        DesktopCheckpointRestoreReview, DesktopCommandEnvelope, DesktopCompactionReview,
        DesktopConversationDisplayPage, DesktopConversationDisplayQuery,
        DesktopConversationQueueCommandAction, DesktopConversationQueueCommandReceipt,
        DesktopConversationQueueCommandRequest, DesktopConversationQueueView,
        DesktopConversationRecoveryCommandAction, DesktopConversationRecoveryCommandReceipt,
        DesktopConversationRecoveryView, DesktopErrorResponse, DesktopRunCancelCommandReceipt,
        DesktopRunCancelRequest, DesktopRunSnapshot, DesktopRunStartCommandReceipt,
        DesktopRunStartRequest, DesktopSessionCatalogBatchExecuteRequest,
        DesktopSessionCatalogBatchPlan, DesktopSessionCatalogBatchPlanRequest,
        DesktopSessionCatalogBatchReceipt, DesktopSessionCatalogPage, DesktopSessionContinuityView,
        DesktopSessionCreateRequest, DesktopSessionDeleteRequest,
        DesktopSessionInvalidSourceDeleteReceipt, DesktopSessionInvalidSourceDeleteRequest,
        DesktopSessionListResponse, DesktopSessionMutationReceipt, DesktopSessionOpenRequest,
        DesktopSessionQuarantineReceipt, DesktopSessionQuarantineRequest,
        DesktopSessionRenameRequest, DesktopSessionSnapshot, DesktopSessionTranscriptPage,
        DesktopSupportBundleExport, DesktopSupportDoctorReport, DesktopTranscriptQuery,
        DesktopVerificationRerunCommandReceipt, DesktopVerificationRerunRequest,
        DesktopVerificationView,
    },
    events::{DesktopProtocolEvent, DesktopProtocolEventClass, DesktopProtocolEventError},
    secret::DesktopBearerToken,
};

const MAX_JSON_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SSE_FRAME_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RUN_EVENT_NAME: &str = "run_event";
const MAX_CONVERSATION_QUEUE_ITEMS: usize = 100;
const MAX_CONVERSATION_QUEUE_PROMPT_PREVIEW_CHARS: usize = 240;
const MAX_CONVERSATION_QUEUE_PROMPT_BYTES: usize = 64 * 1024;

/// Authenticated typed client for one desktop-owned loopback server.
///
/// The bearer and transport address are private and this type has no serialization surface.
#[derive(Clone)]
pub struct DesktopHttpClient {
    client: Client,
    address: SocketAddr,
    bearer: Arc<DesktopBearerToken>,
    client_id: Arc<str>,
}

impl DesktopHttpClient {
    pub(crate) fn new(
        client: Client,
        address: SocketAddr,
        bearer: Arc<DesktopBearerToken>,
    ) -> Self {
        Self {
            client,
            address,
            bearer,
            client_id: Arc::from(format!("sigil-desktop-{}", Uuid::new_v4())),
        }
    }

    /// Lists process-local session handles.
    pub async fn list_sessions(&self) -> Result<DesktopSessionListResponse, DesktopClientError> {
        self.get_json(self.route(["sessions"])?, StatusCode::OK)
            .await
    }

    /// Reads path-free diagnostics from the supervised workspace server.
    pub async fn support_doctor(&self) -> Result<DesktopSupportDoctorReport, DesktopClientError> {
        self.get_json(self.route(["support", "doctor"])?, StatusCode::OK)
            .await
    }

    /// Builds a bounded private support bundle for the native save boundary.
    pub async fn support_bundle(&self) -> Result<DesktopSupportBundleExport, DesktopClientError> {
        self.post_json(self.route(["support", "bundle"])?, &(), StatusCode::OK)
            .await
    }

    /// Creates a new durable session through the server-owned runtime path.
    pub async fn create_session(
        &self,
        request: DesktopSessionCreateRequest,
    ) -> Result<DesktopSessionSnapshot, DesktopClientError> {
        self.post_json(self.route(["sessions"])?, &request, StatusCode::CREATED)
            .await
    }

    /// Revalidates and opens one durable catalog entry.
    pub async fn open_session(
        &self,
        request: DesktopSessionOpenRequest,
    ) -> Result<DesktopSessionSnapshot, DesktopClientError> {
        self.post_json(self.route(["sessions", "open"])?, &request, StatusCode::OK)
            .await
    }

    /// Queries one generation-consistent historical session page.
    pub async fn catalog(
        &self,
        query: &DesktopCatalogQuery,
    ) -> Result<DesktopSessionCatalogPage, DesktopClientError> {
        let mut url = self.route(["session-catalog"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
            if let Some(cursor) = query.cursor.as_deref() {
                pairs.append_pair("cursor", cursor);
            }
            if let Some(value) = query.query.as_deref() {
                pairs.append_pair("q", value);
            }
            if let Some(provider) = query.provider.as_deref() {
                pairs.append_pair("provider", provider);
            }
            if let Some(pinned) = query.pinned {
                pairs.append_pair("pinned", if pinned { "true" } else { "false" });
            }
            if let Some(state) = query.state {
                pairs.append_pair(
                    "state",
                    match state {
                        crate::DesktopSessionCatalogState::Ready => "ready",
                        crate::DesktopSessionCatalogState::Oversized => "oversized",
                        crate::DesktopSessionCatalogState::ScanBudgetExceeded => {
                            "scan_budget_exceeded"
                        }
                        crate::DesktopSessionCatalogState::UnsupportedLegacy => {
                            "unsupported_legacy"
                        }
                        crate::DesktopSessionCatalogState::Invalid => "invalid",
                    },
                );
            }
        }
        self.get_json(url, StatusCode::OK).await
    }

    /// Persists a display name for one exact durable catalog identity.
    pub async fn rename_session(
        &self,
        request: DesktopSessionRenameRequest,
    ) -> Result<DesktopSessionMutationReceipt, DesktopClientError> {
        self.post_json(
            self.route(["session-catalog", "rename"])?,
            &request,
            StatusCode::OK,
        )
        .await
    }

    /// Deletes one exact durable catalog identity after native-shell confirmation.
    pub async fn delete_session(
        &self,
        request: DesktopSessionDeleteRequest,
    ) -> Result<DesktopSessionMutationReceipt, DesktopClientError> {
        self.post_json(
            self.route(["session-catalog", "delete"])?,
            &request,
            StatusCode::OK,
        )
        .await
    }

    /// Moves one exact invalid source out of the active catalog after server revalidation.
    pub async fn quarantine_session(
        &self,
        request: DesktopSessionQuarantineRequest,
    ) -> Result<DesktopSessionQuarantineReceipt, DesktopClientError> {
        self.post_json(
            self.route(["session-catalog", "quarantine"])?,
            &request,
            StatusCode::OK,
        )
        .await
    }

    /// Permanently removes one exact invalid source after native-shell confirmation.
    pub async fn delete_invalid_source(
        &self,
        request: DesktopSessionInvalidSourceDeleteRequest,
    ) -> Result<DesktopSessionInvalidSourceDeleteReceipt, DesktopClientError> {
        self.post_json(
            self.route(["session-catalog", "delete-invalid-source"])?,
            &request,
            StatusCode::OK,
        )
        .await
    }

    /// Builds a content-bound preview for one exact, bounded batch selection.
    pub async fn plan_session_catalog_batch(
        &self,
        request: DesktopSessionCatalogBatchPlanRequest,
    ) -> Result<DesktopSessionCatalogBatchPlan, DesktopClientError> {
        self.post_json(
            self.route(["session-catalog", "batch", "plan"])?,
            &request,
            StatusCode::OK,
        )
        .await
    }

    /// Executes a previously confirmed batch after server-side re-planning.
    pub async fn execute_session_catalog_batch(
        &self,
        request: DesktopSessionCatalogBatchExecuteRequest,
    ) -> Result<DesktopSessionCatalogBatchReceipt, DesktopClientError> {
        self.post_json(
            self.route(["session-catalog", "batch", "execute"])?,
            &request,
            StatusCode::OK,
        )
        .await
    }

    /// Reads one process-local session snapshot.
    pub async fn session(
        &self,
        session_id: &str,
    ) -> Result<DesktopSessionSnapshot, DesktopClientError> {
        self.get_json(self.route(["sessions", session_id])?, StatusCode::OK)
            .await
    }

    /// Probes the current durable frontier and exact process-local foreground owner.
    pub async fn continuity(
        &self,
        session_id: &str,
    ) -> Result<DesktopSessionContinuityView, DesktopClientError> {
        self.get_json(
            self.route(["sessions", session_id, "continuity"])?,
            StatusCode::OK,
        )
        .await
    }

    /// Reads one bounded chronological durable transcript page.
    pub async fn transcript(
        &self,
        session_id: &str,
        query: &DesktopTranscriptQuery,
    ) -> Result<DesktopSessionTranscriptPage, DesktopClientError> {
        validate_stream_identity(session_id)?;
        if query.before == Some(0) || query.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
            return Err(DesktopClientError::InvalidRoute);
        }
        let mut url = self.route(["sessions", session_id, "transcript"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(before) = query.before {
                pairs.append_pair("before", &before.to_string());
            }
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
        }
        self.get_json(url, StatusCode::OK).await
    }

    /// Reads one canonical, identity-ordered conversation display page.
    pub async fn conversation_display(
        &self,
        session_id: &str,
        query: &DesktopConversationDisplayQuery,
    ) -> Result<DesktopConversationDisplayPage, DesktopClientError> {
        validate_stream_identity(session_id)?;
        if query.limit.is_some_and(|limit| !(1..=100).contains(&limit)) {
            return Err(DesktopClientError::InvalidRoute);
        }
        if let Some(cursor) = query.cursor.as_deref() {
            validate_replay_cursor(cursor)?;
        }
        let mut url = self.route(["sessions", session_id, "display"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(cursor) = query.cursor.as_deref() {
                pairs.append_pair("cursor", cursor);
            }
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
        }
        let page: DesktopConversationDisplayPage = self.get_json(url, StatusCode::OK).await?;
        if page.request_scope != session_id
            || page.schema_version != DESKTOP_CONVERSATION_DISPLAY_SCHEMA_VERSION
            || page
                .items
                .iter()
                .any(|item| item.schema_version != DESKTOP_CONVERSATION_DISPLAY_SCHEMA_VERSION)
            || page.has_more != page.next_cursor.is_some()
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        Ok(page)
    }

    /// Reads the bounded, secret-free durable follow-up queue projection.
    pub async fn conversation_queue(
        &self,
        session_id: &str,
    ) -> Result<DesktopConversationQueueView, DesktopClientError> {
        validate_stream_identity(session_id)?;
        let view: DesktopConversationQueueView = self
            .get_json(
                self.route(["sessions", session_id, "queue"])?,
                StatusCode::OK,
            )
            .await?;
        validate_conversation_queue_view(session_id, &view)?;
        Ok(view)
    }

    /// Applies one exact queue mutation under the opaque queue generation CAS guard.
    pub async fn command_conversation_queue(
        &self,
        session_id: &str,
        payload: DesktopConversationQueueCommandRequest,
    ) -> Result<DesktopConversationQueueCommandReceipt, DesktopClientError> {
        validate_stream_identity(session_id)?;
        validate_conversation_queue_command(&payload)?;
        let expected_action = payload.action.kind();
        let expected_generation = payload.expected_generation.clone();
        let expected_interrupt_owner = match &payload.action {
            DesktopConversationQueueCommandAction::InterruptAndRunNext {
                foreground_run_id,
                foreground_owner_revision,
            } => Some((foreground_run_id.clone(), foreground_owner_revision.clone())),
            _ => None,
        };
        let command = self.command(session_id, None, payload);
        let expected_command_id = command.command_id.clone();
        let expected_client_id = command.client_id.clone();
        let receipt: DesktopConversationQueueCommandReceipt = self
            .post_json(
                self.route(["sessions", session_id, "queue"])?,
                &command,
                StatusCode::OK,
            )
            .await?;
        if receipt.command_id != expected_command_id
            || receipt.client_id != expected_client_id
            || receipt.session_id != session_id
            || receipt.action != expected_action
            || receipt.expected_generation != expected_generation
            || match (
                expected_interrupt_owner.as_ref(),
                receipt.interrupt_owner.as_ref(),
            ) {
                (Some((run_id, revision)), Some(owner)) => {
                    owner.run_id != *run_id || owner.owner_revision != *revision
                }
                (None, None) => false,
                _ => true,
            }
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        validate_opaque_queue_generation(&receipt.generation.0)
            .map_err(|_| DesktopClientError::InvalidResponse)?;
        validate_conversation_queue_view(session_id, &receipt.queue)?;
        if receipt.generation != receipt.queue.generation {
            return Err(DesktopClientError::InvalidResponse);
        }
        Ok(receipt)
    }

    /// Reads exact durable checkpoint and conversation-fork choices.
    pub async fn conversation_recovery(
        &self,
        session_id: &str,
    ) -> Result<DesktopConversationRecoveryView, DesktopClientError> {
        validate_stream_identity(session_id)?;
        let view: DesktopConversationRecoveryView = self
            .get_json(
                self.route(["sessions", session_id, "recovery"])?,
                StatusCode::OK,
            )
            .await?;
        validate_conversation_recovery_view(&view)?;
        Ok(view)
    }

    /// Revalidates one checkpoint and returns a bounded reverse-diff preview.
    pub async fn checkpoint_restore_review(
        &self,
        session_id: &str,
        request: DesktopCheckpointRestoreRequest,
    ) -> Result<DesktopCheckpointRestoreReview, DesktopClientError> {
        validate_stream_identity(session_id)?;
        validate_recovery_token(&request.checkpoint_id)?;
        validate_recovery_token(&request.checkpoint_digest)?;
        let review: DesktopCheckpointRestoreReview = self
            .post_json(
                self.route(["sessions", session_id, "recovery", "checkpoint-preview"])?,
                &request,
                StatusCode::OK,
            )
            .await?;
        if review.checkpoint_id != request.checkpoint_id
            || review.checkpoint_digest != request.checkpoint_digest
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        Ok(review)
    }

    /// Builds one exact portable compaction preview without applying it.
    pub async fn conversation_compaction_review(
        &self,
        session_id: &str,
    ) -> Result<DesktopCompactionReview, DesktopClientError> {
        validate_stream_identity(session_id)?;
        let review: DesktopCompactionReview = self
            .post_json(
                self.route(["sessions", session_id, "recovery", "compaction-preview"])?,
                &serde_json::json!({}),
                StatusCode::OK,
            )
            .await?;
        if let Some(preview_id) = review.preview_id.as_deref() {
            validate_recovery_token(preview_id).map_err(|_| DesktopClientError::InvalidResponse)?;
        }
        Ok(review)
    }

    /// Applies one exact restore or conversation fork under an idempotent command identity.
    pub async fn command_conversation_recovery(
        &self,
        session_id: &str,
        action: DesktopConversationRecoveryCommandAction,
    ) -> Result<DesktopConversationRecoveryCommandReceipt, DesktopClientError> {
        validate_stream_identity(session_id)?;
        validate_conversation_recovery_action(&action)?;
        let expected_action = action.kind();
        let command = self.command(session_id, None, action);
        let expected_command_id = command.command_id.clone();
        let expected_client_id = command.client_id.clone();
        let receipt: DesktopConversationRecoveryCommandReceipt = self
            .post_json(
                self.route(["sessions", session_id, "recovery", "commands"])?,
                &command,
                StatusCode::OK,
            )
            .await?;
        if receipt.command_id != expected_command_id
            || receipt.client_id != expected_client_id
            || receipt.session_id != session_id
            || receipt.action != expected_action
        {
            return Err(DesktopClientError::InvalidResponse);
        }
        validate_conversation_recovery_view(&receipt.recovery)?;
        Ok(receipt)
    }

    /// Starts a run with an idempotent command identity.
    pub async fn start_run(
        &self,
        session_id: &str,
        payload: DesktopRunStartRequest,
    ) -> Result<DesktopRunStartCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, None, payload);
        self.post_json(
            self.route(["sessions", session_id, "runs"])?,
            &command,
            StatusCode::CREATED,
        )
        .await
    }

    /// Reads the latest server-owned run snapshot.
    pub async fn run(&self, run_id: &str) -> Result<DesktopRunSnapshot, DesktopClientError> {
        self.get_json(self.route(["runs", run_id])?, StatusCode::OK)
            .await
    }

    /// Connects to the server-owned replay-plus-live SSE stream for one run.
    pub async fn run_events(
        &self,
        session_id: &str,
        durable_session_id: &str,
        run_id: &str,
        owner_revision: &str,
        last_event_id: Option<&str>,
    ) -> Result<DesktopRunEventStream, DesktopClientError> {
        validate_stream_identity(session_id)?;
        validate_stream_identity(durable_session_id)?;
        validate_stream_identity(run_id)?;
        validate_owner_revision(owner_revision)?;
        if let Some(cursor) = last_event_id {
            validate_replay_cursor(cursor)?;
        }
        let mut request = self
            .client
            .get(self.route(["runs", run_id, "events"])?)
            .bearer_auth(self.bearer.expose())
            .header(header::ACCEPT, "text/event-stream")
            .header("X-Sigil-Session-Id", session_id)
            .header("X-Sigil-Owner-Revision", owner_revision);
        if let Some(cursor) = last_event_id {
            request = request.header("Last-Event-ID", cursor);
        }
        let response = request
            .send()
            .await
            .map_err(|_| DesktopClientError::RequestFailed)?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(rejected_response(response).await);
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        {
            return Err(DesktopClientError::InvalidEventStream);
        }
        Ok(DesktopRunEventStream {
            response,
            buffer: Vec::new(),
            session_id: durable_session_id.to_owned(),
            run_id: run_id.to_owned(),
            ended: false,
        })
    }

    /// Requests cooperative cancellation with an optimistic sequence guard.
    pub async fn cancel_run(
        &self,
        session_id: &str,
        run_id: &str,
        expected_stream_sequence: u64,
        payload: DesktopRunCancelRequest,
    ) -> Result<DesktopRunCancelCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, Some(expected_stream_sequence), payload);
        self.post_json(
            self.route(["runs", run_id, "cancel"])?,
            &command,
            StatusCode::OK,
        )
        .await
    }

    /// Resolves one pending approval using the exact durable guard material.
    pub async fn resolve_approval(
        &self,
        session_id: &str,
        run_id: &str,
        call_id: &str,
        expected_stream_sequence: u64,
        payload: DesktopApprovalDecisionRequest,
    ) -> Result<DesktopApprovalCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, Some(expected_stream_sequence), payload);
        self.post_json(
            self.route(["runs", run_id, "approvals", call_id])?,
            &command,
            StatusCode::OK,
        )
        .await
    }

    /// Projects the current server-owned verification card for one session.
    pub async fn verification(
        &self,
        session_id: &str,
    ) -> Result<DesktopVerificationView, DesktopClientError> {
        self.get_json(
            self.route(["sessions", session_id, "verification"])?,
            StatusCode::OK,
        )
        .await
    }

    /// Projects typed model, permission-mode, and context usage facts for one session.
    pub async fn run_context(
        &self,
        session_id: &str,
    ) -> Result<crate::DesktopRunContextView, DesktopClientError> {
        self.get_json(
            self.route(["sessions", session_id, "run-context"])?,
            StatusCode::OK,
        )
        .await
    }

    /// Projects safe bounded child-agent lifecycle and result-handoff state.
    pub async fn agent_activity(
        &self,
        session_id: &str,
    ) -> Result<crate::DesktopAgentActivityView, DesktopClientError> {
        self.get_json(
            self.route(["sessions", session_id, "agent-activity"])?,
            StatusCode::OK,
        )
        .await
    }

    /// Reruns one exact stale-safe verification recommendation.
    pub async fn rerun_verification(
        &self,
        session_id: &str,
        payload: DesktopVerificationRerunRequest,
    ) -> Result<DesktopVerificationRerunCommandReceipt, DesktopClientError> {
        let command = self.command(session_id, None, payload);
        self.post_json(
            self.route(["sessions", session_id, "verification", "rerun"])?,
            &command,
            StatusCode::OK,
        )
        .await
    }

    fn command<T>(
        &self,
        session_id: &str,
        expected_stream_sequence: Option<u64>,
        payload: T,
    ) -> DesktopCommandEnvelope<T> {
        DesktopCommandEnvelope {
            protocol_version: DESKTOP_HTTP_PROTOCOL_VERSION,
            command_id: format!("desktop-command-{}", Uuid::new_v4()),
            client_id: self.client_id.to_string(),
            session_id: session_id.to_owned(),
            expected_stream_sequence,
            correlation_id: None,
            payload,
        }
    }

    fn route<const N: usize>(&self, segments: [&str; N]) -> Result<Url, DesktopClientError> {
        let mut url = Url::parse(&format!("http://{}/", self.address))
            .map_err(|_| DesktopClientError::InvalidRoute)?;
        let mut path = url
            .path_segments_mut()
            .map_err(|_| DesktopClientError::InvalidRoute)?;
        path.clear();
        for segment in segments {
            if segment.is_empty() || segment.len() > 512 {
                return Err(DesktopClientError::InvalidRoute);
            }
            path.push(segment);
        }
        drop(path);
        Ok(url)
    }

    async fn get_json<T>(&self, url: Url, status: StatusCode) -> Result<T, DesktopClientError>
    where
        T: DeserializeOwned,
    {
        self.send_json(self.client.get(url), status).await
    }

    async fn post_json<T, B>(
        &self,
        url: Url,
        body: &B,
        status: StatusCode,
    ) -> Result<T, DesktopClientError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.send_json(self.client.post(url).json(body), status)
            .await
    }

    async fn send_json<T>(
        &self,
        request: RequestBuilder,
        expected_status: StatusCode,
    ) -> Result<T, DesktopClientError>
    where
        T: DeserializeOwned,
    {
        let mut response = request
            .bearer_auth(self.bearer.expose())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|_| DesktopClientError::RequestFailed)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_JSON_RESPONSE_BYTES as u64)
        {
            return Err(DesktopClientError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DesktopClientError::RequestFailed)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
                return Err(DesktopClientError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        if status != expected_status {
            let code = serde_json::from_slice::<DesktopErrorResponse>(&body)
                .ok()
                .and_then(|error| safe_error_code(error.error.code));
            return Err(DesktopClientError::Rejected {
                status: status.as_u16(),
                code,
            });
        }
        serde_json::from_slice(&body).map_err(|_| DesktopClientError::InvalidResponse)
    }
}

/// Bounded incremental decoder for one authenticated run SSE response.
pub struct DesktopRunEventStream {
    response: Response,
    buffer: Vec<u8>,
    session_id: String,
    run_id: String,
    ended: bool,
}

impl DesktopRunEventStream {
    /// Returns the next protocol event, ignoring SSE comments and keep-alives.
    pub async fn next_event(&mut self) -> Result<Option<DesktopProtocolEvent>, DesktopClientError> {
        loop {
            if let Some(frame_end) = sse_frame_end(&self.buffer) {
                let frame = self.buffer.drain(..frame_end).collect::<Vec<_>>();
                let delimiter = if self.buffer.starts_with(b"\r\n\r\n") {
                    4
                } else {
                    2
                };
                self.buffer.drain(..delimiter);
                if let Some(event) = decode_sse_frame(&frame, &self.session_id, &self.run_id)? {
                    return Ok(Some(event));
                }
                continue;
            }
            if self.ended {
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(DesktopClientError::InvalidEventStream)
                };
            }
            let next = self
                .response
                .chunk()
                .await
                .map_err(|_| DesktopClientError::RequestFailed)?;
            match next {
                Some(chunk) => {
                    if self.buffer.len().saturating_add(chunk.len()) > MAX_SSE_FRAME_BYTES {
                        return Err(DesktopClientError::ResponseTooLarge);
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                None => self.ended = true,
            }
        }
    }
}

impl fmt::Debug for DesktopRunEventStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopRunEventStream")
            .field("transport", &"authenticated loopback SSE")
            .field("buffered_bytes", &self.buffer.len())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

fn sse_frame_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .or_else(|| buffer.windows(4).position(|window| window == b"\r\n\r\n"))
}

fn decode_sse_frame(
    frame: &[u8],
    session_id: &str,
    run_id: &str,
) -> Result<Option<DesktopProtocolEvent>, DesktopClientError> {
    let text = std::str::from_utf8(frame).map_err(|_| DesktopClientError::InvalidEventStream)?;
    let mut event_name = None;
    let mut event_id = None;
    let mut data = String::new();
    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" if event_name.replace(value).is_some() => {
                return Err(DesktopClientError::InvalidEventStream);
            }
            "id" if event_id.replace(value).is_some() => {
                return Err(DesktopClientError::InvalidEventStream);
            }
            "data" => {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            "retry" => {}
            _ => {}
        }
    }
    if event_name.is_none() && event_id.is_none() && data.is_empty() {
        return Ok(None);
    }
    if event_name == Some("stream_gap") {
        return Err(DesktopClientError::EventStreamGap);
    }
    if event_name != Some(RUN_EVENT_NAME) || data.is_empty() {
        return Err(DesktopClientError::InvalidEventStream);
    }
    let event = serde_json::from_str::<DesktopProtocolEvent>(&data)
        .map_err(|_| DesktopClientError::InvalidEventStream)?;
    event
        .validate(session_id, run_id)
        .map_err(DesktopClientError::ProtocolEvent)?;
    match event.event_class {
        DesktopProtocolEventClass::Durable if event_id != event.replay_id.as_deref() => {
            Err(DesktopClientError::InvalidEventStream)
        }
        DesktopProtocolEventClass::Transient if event_id.is_some() => {
            Err(DesktopClientError::InvalidEventStream)
        }
        _ => Ok(Some(event)),
    }
}

fn validate_stream_identity(value: &str) -> Result<(), DesktopClientError> {
    if value.is_empty()
        || value.len() > 512
        || value.contains('/')
        || value.chars().any(char::is_control)
    {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

fn validate_owner_revision(value: &str) -> Result<(), DesktopClientError> {
    let Some(hash) = value.strip_prefix("sha256:") else {
        return Err(DesktopClientError::InvalidRoute);
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

fn validate_replay_cursor(value: &str) -> Result<(), DesktopClientError> {
    if value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

fn validate_conversation_queue_view(
    session_id: &str,
    view: &DesktopConversationQueueView,
) -> Result<(), DesktopClientError> {
    use std::collections::BTreeSet;

    if view.schema_version != DESKTOP_CONVERSATION_QUEUE_SCHEMA_VERSION
        || view.session_id != session_id
        || view.items.len() > MAX_CONVERSATION_QUEUE_ITEMS
        || usize::try_from(view.total_items).map_or(true, |total| total < view.items.len())
        || view.truncated != (view.total_items as usize > view.items.len())
    {
        return Err(DesktopClientError::InvalidResponse);
    }
    validate_opaque_queue_generation(&view.generation.0)
        .map_err(|_| DesktopClientError::InvalidResponse)?;
    let mut ids = BTreeSet::new();
    for (order, item) in view.items.iter().enumerate() {
        validate_stream_identity(&item.entry_id)
            .map_err(|_| DesktopClientError::InvalidResponse)?;
        if !ids.insert(item.entry_id.as_str())
            || item.order as usize != order
            || item.prompt_preview.chars().count() > MAX_CONVERSATION_QUEUE_PROMPT_PREVIEW_CHARS
            || item.dispatchable != item.blocked_reason.is_none()
            || item
                .created_at_ms
                .zip(item.updated_at_ms)
                .is_some_and(|(created, updated)| updated < created)
        {
            return Err(DesktopClientError::InvalidResponse);
        }
    }
    if let Some(entry_id) = view.next_dispatchable_entry_id.as_deref() {
        validate_stream_identity(entry_id).map_err(|_| DesktopClientError::InvalidResponse)?;
        let projected_item = view.items.iter().find(|item| item.entry_id == entry_id);
        if projected_item.is_some_and(|item| !item.dispatchable)
            || (projected_item.is_none() && !view.truncated)
        {
            return Err(DesktopClientError::InvalidResponse);
        }
    }
    Ok(())
}

fn validate_conversation_recovery_view(
    view: &DesktopConversationRecoveryView,
) -> Result<(), DesktopClientError> {
    if view.checkpoints.len() > 256 || view.fork_points.len() > 256 {
        return Err(DesktopClientError::InvalidResponse);
    }
    for checkpoint in &view.checkpoints {
        validate_recovery_token(&checkpoint.checkpoint_id)
            .map_err(|_| DesktopClientError::InvalidResponse)?;
        validate_recovery_token(&checkpoint.checkpoint_digest)
            .map_err(|_| DesktopClientError::InvalidResponse)?;
        if checkpoint.turn_index == 0 || checkpoint.files.len() > 256 {
            return Err(DesktopClientError::InvalidResponse);
        }
    }
    for point in &view.fork_points {
        validate_recovery_token(&point.source_turn_digest)
            .map_err(|_| DesktopClientError::InvalidResponse)?;
        if point.source_turn_index == 0
            || point.source_boundary_stream_sequence == 0
            || point.source_finalized_stream_sequence < point.source_boundary_stream_sequence
        {
            return Err(DesktopClientError::InvalidResponse);
        }
    }
    Ok(())
}

fn validate_conversation_recovery_action(
    action: &DesktopConversationRecoveryCommandAction,
) -> Result<(), DesktopClientError> {
    match action {
        DesktopConversationRecoveryCommandAction::ApplyCompaction { preview_id } => {
            validate_recovery_token(preview_id)
        }
        DesktopConversationRecoveryCommandAction::RestoreCheckpoint {
            checkpoint_id,
            checkpoint_digest,
        } => {
            validate_recovery_token(checkpoint_id)?;
            validate_recovery_token(checkpoint_digest)
        }
        DesktopConversationRecoveryCommandAction::ForkConversation { source_turn_digest } => {
            validate_recovery_token(source_turn_digest)
        }
    }
}

fn validate_recovery_token(value: &str) -> Result<(), DesktopClientError> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        Err(DesktopClientError::InvalidRoute)
    } else {
        Ok(())
    }
}

fn validate_conversation_queue_command(
    request: &DesktopConversationQueueCommandRequest,
) -> Result<(), DesktopClientError> {
    validate_opaque_queue_generation(&request.expected_generation.0)?;
    match &request.action {
        DesktopConversationQueueCommandAction::Enqueue { prompt, .. } => {
            validate_queue_prompt(prompt)?;
        }
        DesktopConversationQueueCommandAction::Edit {
            entry_id, prompt, ..
        } => {
            validate_stream_identity(entry_id)?;
            validate_queue_prompt(prompt)?;
        }
        DesktopConversationQueueCommandAction::Remove { entry_id } => {
            validate_stream_identity(entry_id)?;
        }
        DesktopConversationQueueCommandAction::Reorder {
            entry_id,
            after_entry_id,
        } => {
            validate_stream_identity(entry_id)?;
            if let Some(after_entry_id) = after_entry_id {
                validate_stream_identity(after_entry_id)?;
                if after_entry_id == entry_id {
                    return Err(DesktopClientError::InvalidRoute);
                }
            }
        }
        DesktopConversationQueueCommandAction::Pause
        | DesktopConversationQueueCommandAction::Resume => {}
        DesktopConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } => {
            validate_stream_identity(foreground_run_id)?;
            validate_owner_revision(foreground_owner_revision)?;
        }
    }
    Ok(())
}

fn validate_opaque_queue_generation(value: &str) -> Result<(), DesktopClientError> {
    if value.is_empty()
        || value.len() > 512
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

fn validate_queue_prompt(prompt: &str) -> Result<(), DesktopClientError> {
    if prompt.trim().is_empty() || prompt.len() > MAX_CONVERSATION_QUEUE_PROMPT_BYTES {
        return Err(DesktopClientError::InvalidRoute);
    }
    Ok(())
}

async fn rejected_response(mut response: Response) -> DesktopClientError {
    let status = response.status();
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
            return DesktopClientError::ResponseTooLarge;
        }
        body.extend_from_slice(&chunk);
    }
    let code = serde_json::from_slice::<DesktopErrorResponse>(&body)
        .ok()
        .and_then(|error| safe_error_code(error.error.code));
    DesktopClientError::Rejected {
        status: status.as_u16(),
        code,
    }
}

impl fmt::Debug for DesktopHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopHttpClient")
            .field("transport", &"loopback HTTP")
            .field("bearer", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn safe_error_code(code: String) -> Option<String> {
    (!code.is_empty()
        && code.len() <= 128
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(code)
}

/// Path- and credential-free failures safe for native-shell projection.
#[derive(Debug, Error)]
pub enum DesktopClientError {
    #[error("desktop server route is invalid")]
    InvalidRoute,
    #[error("desktop server request failed")]
    RequestFailed,
    #[error("desktop server response exceeded its size limit")]
    ResponseTooLarge,
    #[error("desktop server returned HTTP {status}")]
    Rejected { status: u16, code: Option<String> },
    #[error("desktop server response is invalid")]
    InvalidResponse,
    #[error("desktop server event stream is invalid")]
    InvalidEventStream,
    #[error("desktop server event stream reported a live gap")]
    EventStreamGap,
    #[error(transparent)]
    ProtocolEvent(#[from] DesktopProtocolEventError),
}

#[cfg(test)]
#[path = "tests/client_tests.rs"]
mod tests;
