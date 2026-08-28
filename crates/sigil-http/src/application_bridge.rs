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
    ApplicationApprovalDecision, ApplicationApprovalResolution, ApplicationClient,
    ApplicationCommand, ApplicationCommandId, ApplicationCommandOutcome, ApplicationCommandReceipt,
    ApplicationCommandRequest, ApplicationDomainReceipt, ApplicationError,
    ApplicationPermissionMode, ApplicationPort, ApplicationQueueAction, ApplicationQueueItemKind,
    ApplicationReasoningEffort, ApplicationRecoveryAction, ApplicationRecoveryOutcome,
    ApplicationScope, AuthenticatedSubject, ConversationCommand, HostConnectionInstanceId,
    PageAnchor, PageDirection, PageQueryFingerprint, PageRequestId, ProjectionPage, RunCommand,
    RunStartOptions, SessionScopeId, StablePageCursor,
};
use sigil_runtime::{
    ManagedApplicationReservationStore, RuntimeApplicationDeliveryAckStore,
    RuntimeApplicationDeliveryAcker, RuntimeApplicationDispatch,
    RuntimeApplicationReservationStore, RuntimeApplicationService, RuntimeSessionProjectionBinding,
};
use tokio::runtime::{Handle, RuntimeFlavor};

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
    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        if let Ok(handle) = Handle::try_current()
            && matches!(handle.runtime_flavor(), RuntimeFlavor::MultiThread)
        {
            tokio::task::block_in_place(|| self.runtime.block_on(future))
        } else {
            self.runtime.block_on(future)
        }
    }

    pub(crate) fn refresh(
        &self,
    ) -> Result<sigil_application::ApplicationProjection, ApplicationError> {
        self.block_on(self.client.refresh())
    }

    pub(crate) fn execute(
        &self,
        command_id: &str,
        command: ApplicationCommand,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        let command_id = ApplicationCommandId::new(command_id.to_owned())?;
        self.block_on(self.client.execute_with_id(command_id, command))
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
        self.block_on(self.client.page(
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

pub(crate) fn application_run_start_options(
    request: &crate::HttpRunStartRequest,
) -> Result<RunStartOptions, ApplicationError> {
    let permission_mode = request.permission_mode.ok_or_else(|| {
        ApplicationError::InvalidRequest("permission mode is required".to_owned())
    })?;
    let model = request
        .model_ref
        .as_ref()
        .map(|model| {
            let selection_binding = request.model_selection_binding.as_ref().ok_or_else(|| {
                ApplicationError::InvalidRequest(
                    "model selection binding is required with a model route".to_owned(),
                )
            })?;
            Ok(sigil_application::ApplicationModelRoute {
                connection_id: sigil_application::SafeText::new(model.connection_id.clone())?,
                model_id: sigil_application::SafeText::new(model.model_id.clone())?,
                selection_binding: sigil_application::SafeText::new(selection_binding.clone())?,
            })
        })
        .transpose()?;
    let skill = request
        .skill_binding
        .as_ref()
        .map(|skill| {
            Ok(sigil_application::ApplicationSkillBinding {
                skill_id: sigil_application::SafeText::new(skill.skill_id.clone())?,
                skill_sha256: sigil_application::SafeText::new(skill.skill_sha256.clone())?,
                index_fingerprint: sigil_application::SafeText::new(
                    skill.index_fingerprint.clone(),
                )?,
            })
        })
        .transpose()?;
    let agent = request
        .agent_binding
        .as_ref()
        .map(|agent| {
            Ok(sigil_application::ApplicationAgentBinding {
                profile_id: sigil_application::SafeText::new(agent.profile_id.clone())?,
                snapshot_id: sigil_application::SafeText::new(agent.snapshot_id.clone())?,
            })
        })
        .transpose()?;
    let task_continuation = request
        .task_continuation
        .as_ref()
        .map(|task| {
            Ok(sigil_application::ApplicationTaskContinuation {
                task_id: sigil_application::SafeText::new(task.task_id.clone())?,
                guidance: task
                    .guidance
                    .as_ref()
                    .map(|guidance| sigil_application::SafeText::new(guidance.clone()))
                    .transpose()?,
            })
        })
        .transpose()?;
    Ok(RunStartOptions {
        permission_mode: match permission_mode {
            crate::HttpPermissionMode::ReadOnly => ApplicationPermissionMode::ReadOnly,
            crate::HttpPermissionMode::Manual => ApplicationPermissionMode::Manual,
            crate::HttpPermissionMode::AutoEdit => ApplicationPermissionMode::AutoEdit,
            crate::HttpPermissionMode::DangerFullAccess => {
                ApplicationPermissionMode::DangerFullAccess
            }
        },
        model,
        route_recovery_binding: request
            .route_recovery_binding
            .as_ref()
            .map(|binding| sigil_application::SafeText::new(binding.clone()))
            .transpose()?,
        reasoning_effort: request.reasoning_effort.map(|effort| match effort {
            crate::HttpReasoningEffort::Low => ApplicationReasoningEffort::Low,
            crate::HttpReasoningEffort::Medium => ApplicationReasoningEffort::Medium,
            crate::HttpReasoningEffort::High => ApplicationReasoningEffort::High,
            crate::HttpReasoningEffort::Max => ApplicationReasoningEffort::Max,
        }),
        reasoning_effort_binding: request
            .reasoning_effort_binding
            .as_ref()
            .map(|binding| sigil_application::SafeText::new(binding.clone()))
            .transpose()?,
        skill,
        agent,
        task_continuation,
    })
}

pub(crate) fn application_approval_resolution(
    request: &crate::HttpApprovalDecisionRequest,
) -> Result<ApplicationApprovalResolution, ApplicationError> {
    let decision = match request.decision {
        crate::HttpApprovalDecision::Approve => ApplicationApprovalDecision::Approve,
        crate::HttpApprovalDecision::ApproveForSession => {
            ApplicationApprovalDecision::ApproveForSession
        }
        crate::HttpApprovalDecision::ApproveForFamily => {
            ApplicationApprovalDecision::ApproveForFamily
        }
        crate::HttpApprovalDecision::Deny => ApplicationApprovalDecision::Deny,
    };
    Ok(ApplicationApprovalResolution {
        tool_call_hash: sigil_application::SafeText::new(request.tool_call_hash.clone())?,
        policy_version: sigil_application::SafeText::new(request.policy_version.clone())?,
        expires_at_ms: request.expires_at_ms,
        decision,
        expected_stream_sequence: None,
        family_pattern: request
            .family_pattern
            .as_ref()
            .map(|pattern| sigil_application::SafeText::new(pattern.clone()))
            .transpose()?,
        reason: request
            .reason
            .as_ref()
            .map(|reason| sigil_application::SafeText::new(reason.clone()))
            .transpose()?,
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
        let result = if Handle::try_current().is_ok() {
            // The HTTP driver is a synchronous adapter, while a few production operations
            // legitimately bridge into its composition runtime with `Handle::block_on`. Run the
            // adapter off the Tokio worker so those nested runtime calls cannot panic or stall the
            // application service's executor.
            std::thread::scope(|scope| {
                scope
                    .spawn(|| self.dispatch_sync(&request))
                    .join()
                    .unwrap_or(Err(ApplicationError::Unavailable))
            })
        } else {
            self.dispatch_sync(&request)
        };
        Box::pin(async move { result })
    }
}

impl HttpApplicationCommandExecutor {
    fn dispatch_sync(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<RuntimeApplicationDispatch, ApplicationError> {
        match &request.envelope.command {
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt,
                options: Some(options),
            }) => {
                let has_prompt = prompt.is_some();
                let has_task_continuation = options.task_continuation.is_some();
                if has_prompt == has_task_continuation {
                    return Err(ApplicationError::InvalidRequest(
                        "HTTP run start requires exactly one prompt or task continuation"
                            .to_owned(),
                    ));
                }
                let run_request = http_run_start_request(prompt.as_ref(), options)?;
                let run = self
                    .registry
                    .start_run(&self.session_id, run_request)
                    .map_err(|_| ApplicationError::Unavailable)?;
                let fingerprint = sigil_application::command_fingerprint(request)?;
                Ok(RuntimeApplicationDispatch::Uncertain(
                    sigil_application::UncertainCommandReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                        recovery_binding: format!("http-run-start:{}", run.id),
                    },
                ))
            }
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                options: None,
                ..
            }) => Ok(RuntimeApplicationDispatch::Rejected(
                sigil_application::CommandRejection {
                    kind: "missing_http_run_options".to_owned(),
                    reason: "HTTP run start requires explicit typed run options".to_owned(),
                },
            )),
            ApplicationCommand::Conversation(ConversationCommand::Queue {
                expected_generation,
                action,
            }) => {
                let queue = match self.registry.command_conversation_queue_from_application(
                    &self.session_id,
                    request.envelope.command_id.as_str(),
                    request.admission.principal.as_str(),
                    crate::HttpConversationQueueCommandRequest {
                        expected_generation: crate::HttpConversationQueueGeneration(
                            expected_generation.as_str().to_owned(),
                        ),
                        action: crate::application_bridge::http_queue_request_action(action)?,
                    },
                    request
                        .envelope
                        .correlation_id
                        .as_ref()
                        .map(|id| id.to_string()),
                ) {
                    Ok(queue) => queue,
                    Err(error) => {
                        return Ok(RuntimeApplicationDispatch::Rejected(
                            sigil_application::CommandRejection {
                                kind: "http_conversation_queue_rejected".to_owned(),
                                reason: error.to_string(),
                            },
                        ));
                    }
                };
                let fingerprint = sigil_application::command_fingerprint(request)?;
                Ok(RuntimeApplicationDispatch::Uncertain(
                    sigil_application::UncertainCommandReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                        recovery_binding: format!(
                            "http-queue:{}",
                            hex_binding_component(queue.generation.0.as_str())
                        ),
                    },
                ))
            }
            ApplicationCommand::Conversation(ConversationCommand::Recovery { action }) => {
                if matches!(action, ApplicationRecoveryAction::PrepareCompaction { .. }) {
                    return Ok(RuntimeApplicationDispatch::Rejected(
                        sigil_application::CommandRejection {
                            kind: "http_recovery_preview_required".to_owned(),
                            reason: "compaction preparation remains on the preview boundary"
                                .to_owned(),
                        },
                    ));
                }
                let receipt = match self
                    .registry
                    .command_conversation_recovery_from_application(
                        &self.session_id,
                        request.envelope.command_id.as_str(),
                        request.admission.principal.as_str(),
                        crate::application_bridge::http_recovery_action(action)?,
                        request
                            .envelope
                            .correlation_id
                            .as_ref()
                            .map(|id| id.to_string()),
                    ) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        return Ok(RuntimeApplicationDispatch::Rejected(
                            sigil_application::CommandRejection {
                                kind: "http_conversation_recovery_rejected".to_owned(),
                                reason: error.to_string(),
                            },
                        ));
                    }
                };
                let outcome = crate::application_bridge::application_recovery_outcome(&receipt)?;
                let frontier = sigil_application::ApplicationFrontier {
                    schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
                    scope: request.admission.scope.clone(),
                    writer_generation: request.envelope.expected_frontier.writer_generation,
                    stream_generation: 1,
                    through_sequence: receipt.recovery.through_stream_sequence,
                    durable_cursor: format!(
                        "session-stream:{}",
                        receipt.recovery.through_stream_sequence
                    ),
                };
                Ok(RuntimeApplicationDispatch::Settled(
                    ApplicationDomainReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        frontier,
                        settlement: request.envelope.command.policy().settlement,
                        summary: "HTTP conversation recovery mutation committed".to_owned(),
                        outcome: Some(Box::new(ApplicationCommandOutcome::Recovery(outcome))),
                    },
                ))
            }
            ApplicationCommand::Run(RunCommand::Cancel { binding, reason }) => {
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
                    .cancel_run_with_reason(
                        binding,
                        reason.as_ref().map(|reason| reason.as_str().to_owned()),
                    )
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
            ApplicationCommand::Approval(sigil_application::ApprovalCommand::Resolve {
                binding,
                accepted,
                resolution: Some(resolution),
            }) => {
                let (run_id, call_id, approval_request_id) = parse_approval_binding(binding)?;
                let run = self
                    .registry
                    .get_run(&run_id)
                    .map_err(|_| ApplicationError::NotFound)?;
                if run.session_id != self.session_id {
                    return Err(ApplicationError::ScopeMismatch);
                }
                let resolution_accepted =
                    !matches!(resolution.decision, ApplicationApprovalDecision::Deny);
                if resolution_accepted != *accepted {
                    return Err(ApplicationError::InvalidRequest(
                        "approval accepted flag does not match the typed decision".to_owned(),
                    ));
                }
                let decision = match resolution.decision {
                    ApplicationApprovalDecision::Approve => crate::HttpApprovalDecision::Approve,
                    ApplicationApprovalDecision::ApproveForSession => {
                        crate::HttpApprovalDecision::ApproveForSession
                    }
                    ApplicationApprovalDecision::ApproveForFamily => {
                        crate::HttpApprovalDecision::ApproveForFamily
                    }
                    ApplicationApprovalDecision::Deny => crate::HttpApprovalDecision::Deny,
                };
                let route = self
                    .registry
                    .submit_approval_decision_from_application(
                        &run_id,
                        &call_id,
                        crate::HttpApprovalDecisionRequest {
                            approval_request_id,
                            tool_call_hash: resolution.tool_call_hash.as_str().to_owned(),
                            policy_version: resolution.policy_version.as_str().to_owned(),
                            expires_at_ms: resolution.expires_at_ms,
                            decision,
                            family_pattern: resolution
                                .family_pattern
                                .as_ref()
                                .map(|pattern| pattern.as_str().to_owned()),
                            reason: resolution
                                .reason
                                .as_ref()
                                .map(|reason| reason.as_str().to_owned()),
                        },
                        resolution.expected_stream_sequence,
                    )
                    .map_err(|_| ApplicationError::Unavailable)?;
                let fingerprint = sigil_application::command_fingerprint(request)?;
                Ok(RuntimeApplicationDispatch::Uncertain(
                    sigil_application::UncertainCommandReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                        recovery_binding: format!(
                            "http-approval:{}:{}",
                            approval_route_token(route.0),
                            route.1
                        ),
                    },
                ))
            }
            ApplicationCommand::Approval(sigil_application::ApprovalCommand::Resolve {
                resolution: None,
                ..
            }) => Ok(RuntimeApplicationDispatch::Rejected(
                sigil_application::CommandRejection {
                    kind: "missing_http_approval_resolution".to_owned(),
                    reason: "HTTP approval requires its exact typed guard and decision".to_owned(),
                },
            )),
            ApplicationCommand::UserInput(sigil_application::UserInputCommand::Resolve {
                binding,
                generation,
                expected_request_hash,
                decision,
                permission_mode,
            }) => {
                if binding.trim().is_empty() {
                    return Err(ApplicationError::InvalidRequest(
                        "user input binding is empty".to_owned(),
                    ));
                }
                let receipt = match self.registry.user_input_decision_from_application(
                    &self.session_id,
                    binding,
                    request.envelope.command_id.as_str(),
                    request.admission.principal.as_str(),
                    crate::HttpUserInputDecisionRequest {
                        generation: *generation,
                        expected_request_hash: expected_request_hash.as_str().to_owned(),
                        decision: decision.clone(),
                        permission_mode: permission_mode.map(|mode| match mode {
                            ApplicationPermissionMode::ReadOnly => {
                                crate::HttpPermissionMode::ReadOnly
                            }
                            ApplicationPermissionMode::Manual => crate::HttpPermissionMode::Manual,
                            ApplicationPermissionMode::AutoEdit => {
                                crate::HttpPermissionMode::AutoEdit
                            }
                            ApplicationPermissionMode::DangerFullAccess => {
                                crate::HttpPermissionMode::DangerFullAccess
                            }
                        }),
                    },
                ) {
                    Ok(receipt) => receipt,
                    Err(crate::HttpRegistryError::DriverRejected { message, .. }) => {
                        return Ok(RuntimeApplicationDispatch::Rejected(
                            sigil_application::CommandRejection {
                                kind: "http_user_input_rejected".to_owned(),
                                reason: message,
                            },
                        ));
                    }
                    Err(crate::HttpRegistryError::UserInputStale) => {
                        return Ok(RuntimeApplicationDispatch::Rejected(
                            sigil_application::CommandRejection {
                                kind: "http_user_input_stale".to_owned(),
                                reason: "user input request is stale".to_owned(),
                            },
                        ));
                    }
                    Err(_) => return Err(ApplicationError::Unavailable),
                };
                let continuation = receipt.continuation_run_id.as_deref().unwrap_or_default();
                let fingerprint = sigil_application::command_fingerprint(request)?;
                Ok(RuntimeApplicationDispatch::Uncertain(
                    sigil_application::UncertainCommandReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                        recovery_binding: format!(
                            "http-user-input:{}:{}",
                            hex_binding_component(binding),
                            hex_binding_component(continuation)
                        ),
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

pub(crate) fn application_queue_command(
    request: &crate::HttpConversationQueueCommandRequest,
) -> Result<(sigil_application::SafeText, ApplicationQueueAction), ApplicationError> {
    let expected_generation =
        sigil_application::SafeText::new(request.expected_generation.0.clone())?;
    let action = application_queue_action(&request.action)?;
    Ok((expected_generation, action))
}

fn application_queue_action(
    action: &crate::HttpConversationQueueCommandAction,
) -> Result<ApplicationQueueAction, ApplicationError> {
    let safe = |value: &str| sigil_application::SafeText::new(value.to_owned());
    Ok(match action {
        crate::HttpConversationQueueCommandAction::Enqueue {
            prompt,
            kind,
            reasoning_effort,
        } => ApplicationQueueAction::Enqueue {
            prompt: safe(prompt)?,
            kind: match kind {
                crate::HttpConversationQueueItemKind::Chat => ApplicationQueueItemKind::Chat,
                crate::HttpConversationQueueItemKind::PlanPrompt => {
                    ApplicationQueueItemKind::PlanPrompt
                }
                crate::HttpConversationQueueItemKind::AgentMention => {
                    ApplicationQueueItemKind::AgentMention
                }
                crate::HttpConversationQueueItemKind::AgentMessage => {
                    ApplicationQueueItemKind::AgentMessage
                }
                crate::HttpConversationQueueItemKind::Unknown => ApplicationQueueItemKind::Unknown,
            },
            reasoning_effort: reasoning_effort.map(|effort| match effort {
                crate::HttpReasoningEffort::Low => ApplicationReasoningEffort::Low,
                crate::HttpReasoningEffort::Medium => ApplicationReasoningEffort::Medium,
                crate::HttpReasoningEffort::High => ApplicationReasoningEffort::High,
                crate::HttpReasoningEffort::Max => ApplicationReasoningEffort::Max,
            }),
        },
        crate::HttpConversationQueueCommandAction::Edit {
            entry_id,
            prompt,
            reasoning_effort,
        } => ApplicationQueueAction::Edit {
            entry_id: safe(entry_id)?,
            prompt: safe(prompt)?,
            reasoning_effort: reasoning_effort.map(|effort| match effort {
                crate::HttpReasoningEffort::Low => ApplicationReasoningEffort::Low,
                crate::HttpReasoningEffort::Medium => ApplicationReasoningEffort::Medium,
                crate::HttpReasoningEffort::High => ApplicationReasoningEffort::High,
                crate::HttpReasoningEffort::Max => ApplicationReasoningEffort::Max,
            }),
        },
        crate::HttpConversationQueueCommandAction::Remove { entry_id } => {
            ApplicationQueueAction::Remove {
                entry_id: safe(entry_id)?,
            }
        }
        crate::HttpConversationQueueCommandAction::Reorder {
            entry_id,
            after_entry_id,
        } => ApplicationQueueAction::Reorder {
            entry_id: safe(entry_id)?,
            after_entry_id: after_entry_id
                .as_ref()
                .map(|value| safe(value))
                .transpose()?,
        },
        crate::HttpConversationQueueCommandAction::Pause => ApplicationQueueAction::Pause,
        crate::HttpConversationQueueCommandAction::Resume => ApplicationQueueAction::Resume,
        crate::HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } => ApplicationQueueAction::InterruptAndRunNext {
            foreground_run_id: safe(foreground_run_id)?,
            foreground_owner_revision: safe(foreground_owner_revision)?,
        },
    })
}

fn http_queue_request_action(
    action: &ApplicationQueueAction,
) -> Result<crate::HttpConversationQueueCommandAction, ApplicationError> {
    let safe = |value: &sigil_application::SafeText| value.as_str().to_owned();
    Ok(match action {
        ApplicationQueueAction::Enqueue {
            prompt,
            kind,
            reasoning_effort,
        } => crate::HttpConversationQueueCommandAction::Enqueue {
            prompt: safe(prompt),
            kind: match kind {
                ApplicationQueueItemKind::Chat => crate::HttpConversationQueueItemKind::Chat,
                ApplicationQueueItemKind::PlanPrompt => {
                    crate::HttpConversationQueueItemKind::PlanPrompt
                }
                ApplicationQueueItemKind::AgentMention => {
                    crate::HttpConversationQueueItemKind::AgentMention
                }
                ApplicationQueueItemKind::AgentMessage => {
                    crate::HttpConversationQueueItemKind::AgentMessage
                }
                ApplicationQueueItemKind::Unknown => crate::HttpConversationQueueItemKind::Unknown,
            },
            reasoning_effort: reasoning_effort.map(|effort| match effort {
                ApplicationReasoningEffort::Low => crate::HttpReasoningEffort::Low,
                ApplicationReasoningEffort::Medium => crate::HttpReasoningEffort::Medium,
                ApplicationReasoningEffort::High => crate::HttpReasoningEffort::High,
                ApplicationReasoningEffort::Max => crate::HttpReasoningEffort::Max,
            }),
        },
        ApplicationQueueAction::Edit {
            entry_id,
            prompt,
            reasoning_effort,
        } => crate::HttpConversationQueueCommandAction::Edit {
            entry_id: safe(entry_id),
            prompt: safe(prompt),
            reasoning_effort: reasoning_effort.map(|effort| match effort {
                ApplicationReasoningEffort::Low => crate::HttpReasoningEffort::Low,
                ApplicationReasoningEffort::Medium => crate::HttpReasoningEffort::Medium,
                ApplicationReasoningEffort::High => crate::HttpReasoningEffort::High,
                ApplicationReasoningEffort::Max => crate::HttpReasoningEffort::Max,
            }),
        },
        ApplicationQueueAction::Remove { entry_id } => {
            crate::HttpConversationQueueCommandAction::Remove {
                entry_id: safe(entry_id),
            }
        }
        ApplicationQueueAction::Reorder {
            entry_id,
            after_entry_id,
        } => crate::HttpConversationQueueCommandAction::Reorder {
            entry_id: safe(entry_id),
            after_entry_id: after_entry_id.as_ref().map(safe),
        },
        ApplicationQueueAction::Pause => crate::HttpConversationQueueCommandAction::Pause,
        ApplicationQueueAction::Resume => crate::HttpConversationQueueCommandAction::Resume,
        ApplicationQueueAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } => crate::HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id: safe(foreground_run_id),
            foreground_owner_revision: safe(foreground_owner_revision),
        },
    })
}

pub(crate) fn application_recovery_action(
    action: &crate::HttpConversationRecoveryCommandAction,
) -> Result<ApplicationRecoveryAction, ApplicationError> {
    let safe = |value: &str| sigil_application::SafeText::new(value.to_owned());
    Ok(match action {
        crate::HttpConversationRecoveryCommandAction::PrepareCompaction { preview_id } => {
            ApplicationRecoveryAction::PrepareCompaction {
                preview_id: safe(preview_id)?,
            }
        }
        crate::HttpConversationRecoveryCommandAction::ApplyCompaction { preview_id } => {
            ApplicationRecoveryAction::ApplyCompaction {
                preview_id: safe(preview_id)?,
            }
        }
        crate::HttpConversationRecoveryCommandAction::ApplyStandaloneToolOutputShrink {
            preview_id,
        } => ApplicationRecoveryAction::ApplyStandaloneToolOutputShrink {
            preview_id: safe(preview_id)?,
        },
        crate::HttpConversationRecoveryCommandAction::RestoreCheckpoint {
            checkpoint_id,
            checkpoint_digest,
        } => ApplicationRecoveryAction::RestoreCheckpoint {
            checkpoint_id: safe(checkpoint_id)?,
            checkpoint_digest: safe(checkpoint_digest)?,
        },
        crate::HttpConversationRecoveryCommandAction::ForkConversation {
            source_turn_digest,
            model_ref,
        } => ApplicationRecoveryAction::ForkConversation {
            source_turn_digest: safe(source_turn_digest)?,
            connection_id: safe(&model_ref.connection_id)?,
            model_id: safe(&model_ref.model_id)?,
        },
    })
}

fn http_recovery_action(
    action: &ApplicationRecoveryAction,
) -> Result<crate::HttpConversationRecoveryCommandAction, ApplicationError> {
    let text = |value: &sigil_application::SafeText| value.as_str().to_owned();
    Ok(match action {
        ApplicationRecoveryAction::PrepareCompaction { preview_id } => {
            crate::HttpConversationRecoveryCommandAction::PrepareCompaction {
                preview_id: text(preview_id),
            }
        }
        ApplicationRecoveryAction::ApplyCompaction { preview_id } => {
            crate::HttpConversationRecoveryCommandAction::ApplyCompaction {
                preview_id: text(preview_id),
            }
        }
        ApplicationRecoveryAction::ApplyStandaloneToolOutputShrink { preview_id } => {
            crate::HttpConversationRecoveryCommandAction::ApplyStandaloneToolOutputShrink {
                preview_id: text(preview_id),
            }
        }
        ApplicationRecoveryAction::RestoreCheckpoint {
            checkpoint_id,
            checkpoint_digest,
        } => crate::HttpConversationRecoveryCommandAction::RestoreCheckpoint {
            checkpoint_id: text(checkpoint_id),
            checkpoint_digest: text(checkpoint_digest),
        },
        ApplicationRecoveryAction::ForkConversation {
            source_turn_digest,
            connection_id,
            model_id,
        } => crate::HttpConversationRecoveryCommandAction::ForkConversation {
            source_turn_digest: text(source_turn_digest),
            model_ref: crate::HttpProviderModelRef {
                connection_id: text(connection_id),
                model_id: text(model_id),
            },
        },
    })
}

pub(crate) fn application_recovery_outcome(
    receipt: &crate::HttpConversationRecoveryCommandReceipt,
) -> Result<ApplicationRecoveryOutcome, ApplicationError> {
    let safe = |value: String| sigil_application::SafeText::new(value);
    let count = |value: usize| {
        u64::try_from(value).map_err(|_| {
            ApplicationError::InvalidRequest("recovery count exceeds application bounds".to_owned())
        })
    };
    if let Some(compaction) = &receipt.compaction {
        return Ok(ApplicationRecoveryOutcome::Compaction {
            compaction_id: safe(compaction.compaction_id.clone())?,
            attempt_id: safe(compaction.attempt_id.clone())?,
            task_memory_id: safe(compaction.task_memory_id.clone())?,
            folded_event_count: count(compaction.folded_event_count)?,
            tool_output_projection_recorded: compaction.tool_output_projection_recorded,
            native_carrier_materialized: compaction.native_carrier_materialized,
            native_carrier_status: compaction
                .native_carrier_status
                .clone()
                .map(safe)
                .transpose()?,
        });
    }
    if let Some(shrink) = &receipt.tool_output_shrink {
        return Ok(ApplicationRecoveryOutcome::ToolOutputShrink {
            context_epoch_id: safe(shrink.context_epoch_id.clone())?,
            projected_output_count: count(shrink.projected_output_count)?,
        });
    }
    if let Some(restore) = &receipt.restore {
        return Ok(ApplicationRecoveryOutcome::Restore {
            checkpoint_id: safe(restore.checkpoint_id.clone())?,
            batch_id: safe(restore.batch_id.clone())?,
            restored_file_count: count(restore.restored_file_count)?,
            verification_stale: restore.verification_stale,
        });
    }
    if let Some(fork) = &receipt.fork {
        return Ok(ApplicationRecoveryOutcome::Fork {
            session_ref: safe(fork.session_ref.clone())?,
            session_id: safe(fork.session_id.clone())?,
            copied_message_count: count(fork.copied_message_count)?,
            copied_external_provenance_count: count(fork.copied_external_provenance_count)?,
        });
    }
    Err(ApplicationError::InvalidRequest(
        "recovery mutation returned no typed domain outcome".to_owned(),
    ))
}

pub(crate) fn http_recovery_receipt(
    command_id: String,
    client_id: String,
    session_id: String,
    action: crate::HttpConversationRecoveryCommandActionKind,
    outcome: ApplicationRecoveryOutcome,
    recovery: crate::HttpConversationRecoveryView,
    correlation_id: Option<String>,
    replayed: bool,
) -> Result<crate::HttpConversationRecoveryCommandReceipt, ApplicationError> {
    let as_usize = |value: u64| {
        usize::try_from(value).map_err(|_| {
            ApplicationError::InvalidRequest("recovery count exceeds host bounds".to_owned())
        })
    };
    let mut receipt = crate::HttpConversationRecoveryCommandReceipt {
        command_id,
        client_id,
        session_id,
        action,
        compaction: None,
        compaction_review: None,
        tool_output_shrink: None,
        restore: None,
        fork: None,
        recovery,
        correlation_id,
        replayed,
    };
    match outcome {
        ApplicationRecoveryOutcome::Compaction {
            compaction_id,
            attempt_id,
            task_memory_id,
            folded_event_count,
            tool_output_projection_recorded,
            native_carrier_materialized,
            native_carrier_status,
        } => {
            if action != crate::HttpConversationRecoveryCommandActionKind::ApplyCompaction {
                return Err(ApplicationError::InvalidRequest(
                    "recovery outcome does not match action".to_owned(),
                ));
            }
            receipt.compaction = Some(crate::HttpCompactionReceipt {
                compaction_id: compaction_id.as_str().to_owned(),
                attempt_id: attempt_id.as_str().to_owned(),
                task_memory_id: task_memory_id.as_str().to_owned(),
                folded_event_count: as_usize(folded_event_count)?,
                tool_output_projection_recorded,
                native_carrier_materialized,
                native_carrier_status: native_carrier_status.map(|value| value.as_str().to_owned()),
            });
        }
        ApplicationRecoveryOutcome::ToolOutputShrink {
            context_epoch_id,
            projected_output_count,
        } => {
            if action
                != crate::HttpConversationRecoveryCommandActionKind::ApplyStandaloneToolOutputShrink
            {
                return Err(ApplicationError::InvalidRequest(
                    "recovery outcome does not match action".to_owned(),
                ));
            }
            receipt.tool_output_shrink = Some(crate::HttpToolOutputShrinkReceipt {
                context_epoch_id: context_epoch_id.as_str().to_owned(),
                projected_output_count: as_usize(projected_output_count)?,
            });
        }
        ApplicationRecoveryOutcome::Restore {
            checkpoint_id,
            batch_id,
            restored_file_count,
            verification_stale,
        } => {
            if action != crate::HttpConversationRecoveryCommandActionKind::RestoreCheckpoint {
                return Err(ApplicationError::InvalidRequest(
                    "recovery outcome does not match action".to_owned(),
                ));
            }
            receipt.restore = Some(crate::HttpCheckpointRestoreReceipt {
                checkpoint_id: checkpoint_id.as_str().to_owned(),
                batch_id: batch_id.as_str().to_owned(),
                restored_file_count: as_usize(restored_file_count)?,
                verification_stale,
            });
        }
        ApplicationRecoveryOutcome::Fork {
            session_ref,
            session_id,
            copied_message_count,
            copied_external_provenance_count,
        } => {
            if action != crate::HttpConversationRecoveryCommandActionKind::ForkConversation {
                return Err(ApplicationError::InvalidRequest(
                    "recovery outcome does not match action".to_owned(),
                ));
            }
            receipt.fork = Some(crate::HttpConversationForkReceipt {
                session_ref: session_ref.as_str().to_owned(),
                session_id: session_id.as_str().to_owned(),
                copied_message_count: as_usize(copied_message_count)?,
                copied_external_provenance_count: as_usize(copied_external_provenance_count)?,
            });
        }
    }
    Ok(receipt)
}

fn approval_route_token(route: crate::HttpApprovalRouteState) -> &'static str {
    match route {
        crate::HttpApprovalRouteState::DecisionAccepted => "accepted",
        crate::HttpApprovalRouteState::DeliveryUncertain => "uncertain",
        crate::HttpApprovalRouteState::Terminal => "terminal",
    }
}

fn hex_binding_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn parse_approval_binding(binding: &str) -> Result<(String, String, String), ApplicationError> {
    let mut parts = binding.splitn(3, ':');
    let run_id = parts.next().unwrap_or_default();
    let call_id = parts.next().unwrap_or_default();
    let approval_request_id = parts.next().unwrap_or_default();
    if run_id.is_empty() || call_id.is_empty() || approval_request_id.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "approval binding is malformed".to_owned(),
        ));
    }
    Ok((
        run_id.to_owned(),
        call_id.to_owned(),
        approval_request_id.to_owned(),
    ))
}

fn http_run_start_request(
    prompt: Option<&sigil_application::SafeText>,
    options: &RunStartOptions,
) -> Result<crate::HttpRunStartRequest, ApplicationError> {
    let has_prompt = prompt.is_some();
    let has_task_continuation = options.task_continuation.is_some();
    if has_prompt == has_task_continuation {
        return Err(ApplicationError::InvalidRequest(
            "HTTP run start requires exactly one prompt or task continuation".to_owned(),
        ));
    }
    let model_ref = options
        .model
        .as_ref()
        .map(|model| crate::HttpProviderModelRef {
            connection_id: model.connection_id.as_str().to_owned(),
            model_id: model.model_id.as_str().to_owned(),
        });
    let skill_binding = options
        .skill
        .as_ref()
        .map(|skill| crate::HttpApplicationSkillBinding {
            skill_id: skill.skill_id.as_str().to_owned(),
            skill_sha256: skill.skill_sha256.as_str().to_owned(),
            index_fingerprint: skill.index_fingerprint.as_str().to_owned(),
        });
    let agent_binding = options
        .agent
        .as_ref()
        .map(|agent| crate::HttpApplicationAgentBinding {
            profile_id: agent.profile_id.as_str().to_owned(),
            snapshot_id: agent.snapshot_id.as_str().to_owned(),
        });
    let task_continuation =
        options
            .task_continuation
            .as_ref()
            .map(|task| crate::HttpTaskContinuationRequest {
                task_id: task.task_id.as_str().to_owned(),
                guidance: task
                    .guidance
                    .as_ref()
                    .map(|guidance| guidance.as_str().to_owned()),
            });
    Ok(crate::HttpRunStartRequest {
        prompt: prompt.map_or_else(String::new, |prompt| prompt.as_str().to_owned()),
        model_ref,
        model_selection_binding: options
            .model
            .as_ref()
            .map(|model| model.selection_binding.as_str().to_owned()),
        route_recovery_binding: options
            .route_recovery_binding
            .as_ref()
            .map(|binding| binding.as_str().to_owned()),
        permission_mode: Some(match options.permission_mode {
            ApplicationPermissionMode::ReadOnly => crate::HttpPermissionMode::ReadOnly,
            ApplicationPermissionMode::Manual => crate::HttpPermissionMode::Manual,
            ApplicationPermissionMode::AutoEdit => crate::HttpPermissionMode::AutoEdit,
            ApplicationPermissionMode::DangerFullAccess => {
                crate::HttpPermissionMode::DangerFullAccess
            }
        }),
        reasoning_effort: options.reasoning_effort.map(|effort| match effort {
            ApplicationReasoningEffort::Low => crate::HttpReasoningEffort::Low,
            ApplicationReasoningEffort::Medium => crate::HttpReasoningEffort::Medium,
            ApplicationReasoningEffort::High => crate::HttpReasoningEffort::High,
            ApplicationReasoningEffort::Max => crate::HttpReasoningEffort::Max,
        }),
        reasoning_effort_binding: options
            .reasoning_effort_binding
            .as_ref()
            .map(|binding| binding.as_str().to_owned()),
        skill_binding,
        agent_binding,
        task_continuation,
    })
}
