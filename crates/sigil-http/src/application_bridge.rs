//! HTTP adapter for the transport-neutral application contract.
//!
//! This module is deliberately narrow.  It binds an authenticated HTTP client to the runtime
//! projection, managed reservation journal, and managed delivery-ack journal.  It does not expose
//! runtime paths or authority objects in the wire request.  Commands without a lossless HTTP
//! adapter mapping are rejected by the typed application executor until their host semantics are
//! migrated.

use std::{num::NonZeroUsize, sync::Arc};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_application::{
    ApplicationClient, ApplicationCommand, ApplicationCommandId, ApplicationCommandReceipt,
    ApplicationCommandRequest, ApplicationError, ApplicationPort, ApplicationScope,
    AuthenticatedSubject, HostConnectionInstanceId, PageAnchor, PageDirection,
    PageQueryFingerprint, PageRequestId, ProjectionPage, RunCommand, SessionScopeId,
    StablePageCursor,
};
use sigil_runtime::{
    ManagedApplicationReservationStore, RuntimeApplicationDeliveryAckStore,
    RuntimeApplicationDeliveryAcker, RuntimeApplicationDispatch,
    RuntimeApplicationReservationStore, RuntimeApplicationService, RuntimeSessionProjectionBinding,
};
use tokio::runtime::Handle;

use crate::{HttpRunDriverError, HttpSessionRunRegistry, HttpSessionSnapshot};

/// Host-bound command request for the HTTP application endpoint.
///
/// The client identity is intentionally carried by the authenticated transport header rather
/// than by this payload.  The server therefore injects the admission principal, epoch, and live
/// connection instance instead of trusting a caller-provided authority scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpApplicationCommandRequest {
    /// Caller-retained command identity used for response-lost retries.
    pub command_id: String,
    /// Transport-neutral grouped command.
    pub command: ApplicationCommand,
}

/// Runtime inputs captured by the HTTP composition root for one application client.
pub(crate) struct HttpApplicationContext {
    pub(crate) config_path: std::path::PathBuf,
    pub(crate) launch_cwd: std::path::PathBuf,
    pub(crate) application_instance_id: String,
    pub(crate) application_generation: u64,
    pub(crate) reservations: Arc<ManagedApplicationReservationStore>,
    pub(crate) delivery_acks: Arc<RuntimeApplicationDeliveryAckStore>,
    pub(crate) registry: Arc<HttpSessionRunRegistry>,
    pub(crate) runtime: Handle,
}

pub(crate) fn application_scope(
    application_instance_id: &str,
    session: &HttpSessionSnapshot,
) -> Result<ApplicationScope, HttpRunDriverError> {
    let application_instance =
        sigil_application::ApplicationInstanceId::new(application_instance_id.to_owned())
            .map_err(application_driver_error)?;
    let authenticated_subject =
        AuthenticatedSubject::new("http-local-user").map_err(application_driver_error)?;
    let session_scope = SessionScopeId::new(session.durable_session_scope_id.clone())
        .map_err(application_driver_error)?;
    Ok(ApplicationScope {
        application_instance,
        authenticated_subject,
        // Workspace authority is captured and enforced by the runtime session binding.  This
        // first HTTP bridge keeps the application scope session-bound; a later cross-surface
        // scope slice will expose the same host-owned workspace identity to every adapter.
        workspace: None,
        session: Some(session_scope),
    })
}

/// HTTP-local client facade used by the listener and production integration tests.
pub struct HttpApplicationClient {
    client: ApplicationClient,
    runtime: Handle,
    source_generation: u64,
}

impl HttpApplicationClient {
    pub(crate) fn refresh(
        &self,
    ) -> Result<sigil_application::ApplicationProjection, ApplicationError> {
        self.runtime.block_on(self.client.refresh())
    }

    pub(crate) fn execute(
        &self,
        command_id: &str,
        command: ApplicationCommand,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        let command_id = ApplicationCommandId::new(command_id.to_owned())?;
        self.runtime
            .block_on(self.client.execute_with_id(command_id, command))
    }

    pub(crate) fn page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ProjectionPage, ApplicationError> {
        let limit = NonZeroUsize::new(limit).ok_or_else(|| {
            ApplicationError::InvalidRequest("application page limit must be positive".to_owned())
        })?;
        if limit.get() > sigil_application::MAX_PAGE_ITEMS {
            return Err(ApplicationError::InvalidRequest(
                "application page limit exceeds the application bound".to_owned(),
            ));
        }
        let cursor = before
            .map(|ordinal| StablePageCursor::new(format!("before:{ordinal}")))
            .transpose()?;
        let request_id =
            PageRequestId::new(format!("http-application-page-{}", uuid::Uuid::new_v4()))?;
        let query = PageQueryFingerprint::new("application-transcript")?;
        self.runtime.block_on(self.client.page(
            request_id,
            self.source_generation,
            query,
            PageAnchor {
                item_id: None,
                intra_item_row: 0,
                cursor,
            },
            PageDirection::Older,
            limit,
            0,
        ))
    }
}

/// Builds one HTTP client from trusted composition data and a transport client identity.
pub(crate) fn build_client(
    context: &HttpApplicationContext,
    session: &HttpSessionSnapshot,
    client_id: &str,
) -> Result<HttpApplicationClient, HttpRunDriverError> {
    validate_client_id(client_id)?;
    let scope = application_scope(&context.application_instance_id, session)?;
    let application_instance = scope.application_instance.clone();
    let authenticated_subject = scope.authenticated_subject.clone();
    let projection = RuntimeSessionProjectionBinding::new(
        context.config_path.clone(),
        context.launch_cwd.clone(),
        session.session_log_path.clone().into(),
        session.durable_session_scope_id.clone(),
        application_instance,
        authenticated_subject.clone(),
        scope.workspace.clone(),
        context.application_generation,
        1,
        1,
        1,
    )
    .map_err(application_driver_error)?;
    let executor = Arc::new(HttpApplicationCommandExecutor {
        registry: Arc::clone(&context.registry),
        session_id: session.id.clone(),
    });
    let service: Arc<dyn ApplicationPort> = Arc::new(RuntimeApplicationService::new(
        Arc::new(projection),
        executor,
        Arc::clone(&context.reservations) as Arc<dyn RuntimeApplicationReservationStore>,
        Arc::clone(&context.delivery_acks) as Arc<dyn RuntimeApplicationDeliveryAcker>,
    ));
    let client_epoch = stable_http_client_epoch(&scope, client_id);
    let connection_instance =
        HostConnectionInstanceId::new(format!("http-{}", uuid::Uuid::new_v4()))
            .map_err(application_driver_error)?;
    let client = ApplicationClient::new(service, scope, 1, client_epoch, connection_instance)
        .map_err(application_driver_error)?;
    Ok(HttpApplicationClient {
        client,
        runtime: context.runtime.clone(),
        source_generation: context.application_generation,
    })
}

fn application_driver_error(error: ApplicationError) -> HttpRunDriverError {
    HttpRunDriverError::new(format!("application client binding failed: {error}"))
}

fn validate_client_id(client_id: &str) -> Result<(), HttpRunDriverError> {
    if client_id.trim().is_empty()
        || client_id.len() > 256
        || client_id.chars().any(char::is_control)
    {
        return Err(HttpRunDriverError::new(
            "application client id is empty, over-bounded, or contains control characters",
        ));
    }
    Ok(())
}

fn stable_http_client_epoch(scope: &ApplicationScope, client_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"sigil-http-application-client-epoch-v1\0");
    for component in [
        scope.application_instance.as_str(),
        scope.authenticated_subject.as_str(),
        client_id,
    ] {
        hasher.update((component.len() as u64).to_be_bytes());
        hasher.update(component.as_bytes());
    }
    if let Some(session) = &scope.session {
        hasher.update((session.as_str().len() as u64).to_be_bytes());
        hasher.update(session.as_str().as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes) | 1
}

struct HttpApplicationCommandExecutor {
    registry: Arc<HttpSessionRunRegistry>,
    session_id: String,
}

impl sigil_runtime::RuntimeApplicationCommandExecutor for HttpApplicationCommandExecutor {
    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>> {
        let result = self.dispatch_sync(&request);
        Box::pin(async move { result })
    }
}

impl HttpApplicationCommandExecutor {
    fn dispatch_sync(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<RuntimeApplicationDispatch, ApplicationError> {
        match &request.envelope.command {
            ApplicationCommand::Run(RunCommand::Cancel { binding }) => {
                if binding.trim().is_empty() {
                    return Err(ApplicationError::InvalidRequest(
                        "run cancellation binding is empty".to_owned(),
                    ));
                }
                let run = self
                    .registry
                    .get_run(binding)
                    .map_err(|_| ApplicationError::NotFound)?;
                if run.session_id != self.session_id {
                    return Err(ApplicationError::ScopeMismatch);
                }
                self.registry
                    .cancel_run(binding)
                    .map_err(|_| ApplicationError::Unavailable)?;
                let fingerprint = sigil_application::command_fingerprint(request)?;
                Ok(RuntimeApplicationDispatch::Uncertain(
                    sigil_application::UncertainCommandReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                        recovery_binding: "http-run-cancel-event-reconcile".to_owned(),
                    },
                ))
            }
            _ => Ok(RuntimeApplicationDispatch::Rejected(
                sigil_application::CommandRejection {
                    kind: "unsupported_http_application_command".to_owned(),
                    reason: "this HTTP bridge has no lossless host mapping for the command"
                        .to_owned(),
                },
            )),
        }
    }
}
