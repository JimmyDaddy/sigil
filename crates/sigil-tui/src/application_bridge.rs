//! TUI adapter for the transport-neutral application port.
//!
//! The adapter deliberately keeps worker protocol details at the composition edge.  The TUI
//! receives only an application snapshot and submits grouped, bounded commands; a worker
//! enqueue is reported as `Uncertain` until the durable application event stream settles it.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use futures::future::BoxFuture;
use sha2::{Digest, Sha256};
use sigil_application::{
    ApplicationClient, ApplicationCommand, ApplicationCommandReceipt, ApplicationCommandRequest,
    ApplicationError, ApplicationPort, ApplicationProjection, ApplicationQueueAction,
    ApplicationQueueItemKind, ApplicationQueueMoveDirection, ApplicationQueueTarget,
    ApplicationReasoningEffort, ApplicationScope, AuthenticatedSubject, ConversationCommand,
    HostConnectionInstanceId, McpCommand, RunCommand,
};
use sigil_kernel::ReasoningEffort;

use crate::{
    app::AppAction,
    runner::{WorkerApprovalCommand, WorkerCommand, WorkerCommandEnvelope, WorkerCommandSender},
};

/// A TUI-local application client.  It caches only the latest bounded frontier/projection needed
/// to construct a CAS-bound command; paths and physical authority objects remain in the runtime.
pub(crate) struct TuiApplicationSession {
    client: ApplicationClient,
    reasoning_effort: ApplicationReasoningEffort,
}

impl std::fmt::Debug for TuiApplicationSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiApplicationSession")
            .field("client", &self.client)
            .finish()
    }
}

impl TuiApplicationSession {
    pub(crate) fn new(
        port: Arc<dyn ApplicationPort>,
        scope: ApplicationScope,
        observer_generation: u64,
        client_epoch: u64,
        connection_instance: HostConnectionInstanceId,
        reasoning_effort: ApplicationReasoningEffort,
    ) -> Result<Self, ApplicationError> {
        Ok(Self {
            client: ApplicationClient::new(
                port,
                scope,
                observer_generation,
                client_epoch,
                connection_instance,
            )?,
            reasoning_effort,
        })
    }

    pub(crate) async fn refresh(&self) -> Result<ApplicationProjection, ApplicationError> {
        self.client.refresh().await
    }

    /// Converts only commands with a lossless V1 application representation.  Unsupported TUI
    /// actions remain at the legacy adapter until their typed payload is added to the contract;
    /// they are never smuggled through a generic string command.
    pub(crate) fn try_execute_action(
        &self,
        action: &AppAction,
        queue_target: Option<&sigil_kernel::ConversationInputTarget>,
    ) -> Result<Option<ApplicationCommandReceipt>, ApplicationError> {
        if !matches!(
            action,
            AppAction::SubmitPrompt(_)
                | AppAction::CancelRun
                | AppAction::ApprovalDecision { .. }
                | AppAction::ActivateLazyMcp {
                    server_name: Some(_),
                }
                | AppAction::RefreshMcpServer { .. }
                | AppAction::QueueConversationInput { .. }
                | AppAction::CancelQueuedConversationInput { .. }
                | AppAction::EditQueuedConversationInput { .. }
                | AppAction::MoveQueuedConversationInput { .. }
                | AppAction::PromoteQueuedConversationInput { .. }
                | AppAction::SendQueuedConversationInputNow { .. }
                | AppAction::SetConversationQueuePaused { .. }
        ) {
            return Ok(None);
        }
        let latest = self
            .client
            .current_projection()?
            .ok_or(ApplicationError::Unavailable)?;
        let command = match action {
            AppAction::SubmitPrompt(prompt) => Some(ApplicationCommand::Conversation(
                ConversationCommand::SubmitPrompt {
                    prompt: Some(sigil_application::SafeText::new(prompt.clone())?),
                    options: None,
                },
            )),
            AppAction::CancelRun => latest
                .run
                .active_binding
                .clone()
                .map(|binding| {
                    ApplicationCommand::Run(RunCommand::Cancel {
                        binding,
                        reason: None,
                    })
                })
                .ok_or_else(|| {
                    ApplicationError::InvalidRequest(
                        "cannot cancel without an active application run binding".to_owned(),
                    )
                })
                .map(Some)?,
            AppAction::ApprovalDecision { approved, .. } => latest
                .approval
                .binding
                .clone()
                .map(|binding| {
                    ApplicationCommand::Approval(sigil_application::ApprovalCommand::Resolve {
                        binding,
                        accepted: *approved,
                        resolution: None,
                    })
                })
                .ok_or_else(|| {
                    ApplicationError::InvalidRequest(
                        "cannot resolve approval without an active application binding".to_owned(),
                    )
                })
                .map(Some)?,
            AppAction::ActivateLazyMcp { server_name } => server_name.as_ref().map(|binding| {
                ApplicationCommand::Mcp(McpCommand::Activate {
                    binding: binding.clone(),
                })
            }),
            AppAction::RefreshMcpServer { server_name } => {
                Some(ApplicationCommand::Mcp(McpCommand::Refresh {
                    binding: server_name.clone(),
                }))
            }
            AppAction::QueueConversationInput {
                prompt,
                kind,
                target,
            } => {
                let target = require_active_queue_target(queue_target, target)?;
                Some(ApplicationCommand::Conversation(
                    ConversationCommand::Queue {
                        expected_generation: latest.queue.generation.clone(),
                        action: ApplicationQueueAction::Enqueue {
                            target: application_queue_target(target)?,
                            prompt: sigil_application::SafeText::new(prompt.clone())?,
                            kind: application_queue_item_kind(*kind),
                            reasoning_effort: Some(self.reasoning_effort),
                        },
                    },
                ))
            }
            AppAction::CancelQueuedConversationInput { queue_id } => {
                let target = active_application_queue_target(queue_target)?;
                Some(ApplicationCommand::Conversation(
                    ConversationCommand::Queue {
                        expected_generation: latest.queue.generation.clone(),
                        action: ApplicationQueueAction::Remove {
                            target,
                            entry_id: sigil_application::SafeText::new(
                                queue_id.as_str().to_owned(),
                            )?,
                        },
                    },
                ))
            }
            AppAction::EditQueuedConversationInput { queue_id, prompt } => {
                let target = active_application_queue_target(queue_target)?;
                Some(ApplicationCommand::Conversation(
                    ConversationCommand::Queue {
                        expected_generation: latest.queue.generation.clone(),
                        action: ApplicationQueueAction::Edit {
                            target,
                            entry_id: sigil_application::SafeText::new(
                                queue_id.as_str().to_owned(),
                            )?,
                            prompt: sigil_application::SafeText::new(prompt.clone())?,
                            reasoning_effort: Some(self.reasoning_effort),
                        },
                    },
                ))
            }
            AppAction::MoveQueuedConversationInput {
                queue_id,
                direction,
            } => {
                let target = active_application_queue_target(queue_target)?;
                Some(ApplicationCommand::Conversation(
                    ConversationCommand::Queue {
                        expected_generation: latest.queue.generation.clone(),
                        action: ApplicationQueueAction::Move {
                            target,
                            entry_id: sigil_application::SafeText::new(
                                queue_id.as_str().to_owned(),
                            )?,
                            direction: match direction {
                                crate::runner::QueueMoveDirection::Up => {
                                    ApplicationQueueMoveDirection::Up
                                }
                                crate::runner::QueueMoveDirection::Down => {
                                    ApplicationQueueMoveDirection::Down
                                }
                            },
                        },
                    },
                ))
            }
            AppAction::PromoteQueuedConversationInput { queue_id } => {
                let target = active_application_queue_target(queue_target)?;
                Some(ApplicationCommand::Conversation(
                    ConversationCommand::Queue {
                        expected_generation: latest.queue.generation.clone(),
                        action: ApplicationQueueAction::Promote {
                            target,
                            entry_id: sigil_application::SafeText::new(
                                queue_id.as_str().to_owned(),
                            )?,
                        },
                    },
                ))
            }
            AppAction::SendQueuedConversationInputNow { queue_id } => {
                let target = active_application_queue_target(queue_target)?;
                Some(ApplicationCommand::Conversation(
                    ConversationCommand::Queue {
                        expected_generation: latest.queue.generation.clone(),
                        action: ApplicationQueueAction::SendNow {
                            target,
                            entry_id: sigil_application::SafeText::new(
                                queue_id.as_str().to_owned(),
                            )?,
                        },
                    },
                ))
            }
            AppAction::SetConversationQueuePaused { paused } => Some(
                ApplicationCommand::Conversation(ConversationCommand::Queue {
                    expected_generation: latest.queue.generation.clone(),
                    action: if *paused {
                        ApplicationQueueAction::Pause
                    } else {
                        ApplicationQueueAction::Resume
                    },
                }),
            ),
            _ => None,
        };
        let Some(command) = command else {
            return Ok(None);
        };
        Ok(Some(futures::executor::block_on(
            self.client.execute(command),
        )?))
    }
}

fn require_active_queue_target<'a>(
    active: Option<&'a sigil_kernel::ConversationInputTarget>,
    requested: &'a sigil_kernel::ConversationInputTarget,
) -> Result<&'a sigil_kernel::ConversationInputTarget, ApplicationError> {
    let active = active.ok_or(ApplicationError::Unavailable)?;
    if active != requested {
        return Err(ApplicationError::ScopeMismatch);
    }
    Ok(active)
}

fn active_application_queue_target(
    target: Option<&sigil_kernel::ConversationInputTarget>,
) -> Result<ApplicationQueueTarget, ApplicationError> {
    target
        .map(application_queue_target)
        .transpose()?
        .ok_or(ApplicationError::Unavailable)
}

fn application_queue_target(
    target: &sigil_kernel::ConversationInputTarget,
) -> Result<ApplicationQueueTarget, ApplicationError> {
    match target {
        sigil_kernel::ConversationInputTarget::MainThread => Ok(ApplicationQueueTarget::MainThread),
        sigil_kernel::ConversationInputTarget::AgentThread { thread_id } => {
            Ok(ApplicationQueueTarget::AgentThread {
                thread_id: sigil_application::SafeText::new(thread_id.as_str().to_owned())?,
            })
        }
        sigil_kernel::ConversationInputTarget::Task { task_id } => {
            Ok(ApplicationQueueTarget::Task {
                task_id: sigil_application::SafeText::new(task_id.as_str().to_owned())?,
            })
        }
    }
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

/// Builds the runtime application port for one already-started worker.  The worker command
/// sender is only an executor edge; reservation, projection and receipt policy remain in the
/// runtime service.
pub(crate) fn build_for_worker(
    app: &crate::app::AppState,
    worker_tx: WorkerCommandSender,
    reasoning_effort: ReasoningEffort,
) -> Result<TuiApplicationSession> {
    let cutover = app
        .boot_cutover()
        .context("application port requires the published boot cutover")?;
    let composition = app
        .authority_composition()
        .context("application port requires the composed authority")?;
    let application_instance = sigil_application::ApplicationInstanceId::new(
        cutover.manifest().application_instance_id.clone(),
    )?;
    let subject = AuthenticatedSubject::new("local-user")?;
    let workspace_id =
        sigil_kernel::stable_workspace_id(&app.workspace_root).map_err(|error| anyhow!(error))?;
    let scope = ApplicationScope {
        application_instance: application_instance.clone(),
        authenticated_subject: subject.clone(),
        workspace: Some(sigil_application::WorkspaceScopeId::new(workspace_id)?),
        session: Some(sigil_application::SessionScopeId::new(
            app.session_id.clone(),
        )?),
    };
    let session_scope_id = app.session_id.clone();
    let projection = sigil_runtime::RuntimeSessionProjectionBinding::new(
        app.config_path.clone(),
        std::env::current_dir()?,
        app.session_log_path.clone(),
        session_scope_id.clone(),
        application_instance,
        subject,
        scope.workspace.clone(),
        cutover.manifest().application_generation,
        1,
        1,
        1,
    )?;
    let reservations = sigil_runtime::ManagedApplicationReservationStore::open(
        Arc::clone(&composition.storage_writer),
        "tui-application",
    )
    .map_err(|error| anyhow!(error))?;
    let delivery_acks = sigil_runtime::RuntimeApplicationDeliveryAckStore::open(
        Arc::clone(&composition.storage_writer),
        &format!("tui-application-delivery-{}", app.session_id),
        scope.clone(),
        1,
    )
    .map_err(|error| anyhow!(error))?;
    let application_reasoning_effort = application_reasoning_effort(&reasoning_effort);
    let executor = Arc::new(TuiWorkerCommandExecutor {
        worker_tx,
        reasoning_effort,
        session_id: session_scope_id,
    });
    let service = Arc::new(sigil_runtime::RuntimeApplicationService::new(
        Arc::new(projection),
        executor,
        Arc::new(reservations),
        Arc::new(delivery_acks),
    ));
    let client_epoch = stable_tui_client_epoch(&scope);
    let connection = HostConnectionInstanceId::new(format!("tui-{}", uuid::Uuid::new_v4()))?;
    TuiApplicationSession::new(
        service,
        scope,
        1,
        client_epoch,
        connection,
        application_reasoning_effort,
    )
    .map_err(|error| anyhow!(error))
}

fn application_reasoning_effort(effort: &ReasoningEffort) -> ApplicationReasoningEffort {
    match effort {
        ReasoningEffort::Low => ApplicationReasoningEffort::Low,
        ReasoningEffort::Medium => ApplicationReasoningEffort::Medium,
        ReasoningEffort::High => ApplicationReasoningEffort::High,
        ReasoningEffort::Max => ApplicationReasoningEffort::Max,
    }
}

/// Derives the durable TUI client epoch from the host-owned application/session identity. A
/// reconnect keeps the same reservation namespace for retained command ids, while the live
/// connection instance remains unique for each attachment.
fn stable_tui_client_epoch(scope: &ApplicationScope) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"sigil-tui-application-client-epoch-v1\0");
    hasher.update(scope.application_instance.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(scope.authenticated_subject.as_str().as_bytes());
    hasher.update(b"\0");
    if let Some(workspace) = &scope.workspace {
        hasher.update(workspace.as_str().as_bytes());
    }
    hasher.update(b"\0");
    if let Some(session) = &scope.session {
        hasher.update(session.as_str().as_bytes());
    }
    let digest = hasher.finalize();
    let mut epoch_bytes = [0u8; 8];
    epoch_bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(epoch_bytes) | 1
}

struct TuiWorkerCommandExecutor {
    worker_tx: WorkerCommandSender,
    reasoning_effort: ReasoningEffort,
    session_id: String,
}

impl sigil_runtime::RuntimeApplicationCommandExecutor for TuiWorkerCommandExecutor {
    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<sigil_runtime::RuntimeApplicationDispatch, ApplicationError>>
    {
        let result = self.dispatch_sync(&request);
        Box::pin(async move { result })
    }
}

impl TuiWorkerCommandExecutor {
    fn dispatch_sync(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<sigil_runtime::RuntimeApplicationDispatch, ApplicationError> {
        let command = match &request.envelope.command {
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: Some(prompt),
                ..
            }) => WorkerCommand::SubmitPrompt {
                prompt: prompt.as_str().to_owned(),
                reasoning_effort: self.reasoning_effort.clone(),
            },
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: None,
                ..
            }) => {
                return Ok(sigil_runtime::RuntimeApplicationDispatch::Rejected(
                    sigil_application::CommandRejection {
                        kind: "missing_tui_prompt".to_owned(),
                        reason: "the TUI worker adapter requires a prompt".to_owned(),
                    },
                ));
            }
            ApplicationCommand::Run(RunCommand::Cancel { .. }) => WorkerCommand::CancelRun,
            ApplicationCommand::Approval(sigil_application::ApprovalCommand::Resolve {
                binding,
                accepted,
                ..
            }) => {
                let (_, call_id, approval_request_id) = parse_approval_binding(binding)?;
                WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
                    request.envelope.command_id.as_str(),
                    "sigil-tui-application",
                    &self.session_id,
                    WorkerApprovalCommand::Decision {
                        call_id,
                        approval_request_id,
                        approved: *accepted,
                    },
                ))
            }
            ApplicationCommand::Mcp(McpCommand::Activate { binding }) => {
                WorkerCommand::ActivateLazyMcp {
                    server_name: Some(binding.clone()),
                }
            }
            ApplicationCommand::Mcp(McpCommand::Refresh { binding }) => {
                WorkerCommand::RefreshMcpServer {
                    server_name: binding.clone(),
                }
            }
            ApplicationCommand::Conversation(ConversationCommand::Queue { action, .. }) => {
                match action {
                    ApplicationQueueAction::Enqueue {
                        target,
                        prompt,
                        kind,
                        reasoning_effort,
                    } => WorkerCommand::QueueConversationInput {
                        prompt: prompt.as_str().to_owned(),
                        kind: tui_queue_item_kind(*kind),
                        target: tui_queue_target(target)?,
                        reasoning_effort: tui_reasoning_effort(
                            reasoning_effort
                                .unwrap_or(application_reasoning_effort(&self.reasoning_effort)),
                        ),
                    },
                    ApplicationQueueAction::Edit {
                        target: _,
                        entry_id,
                        prompt,
                        reasoning_effort,
                    } => WorkerCommand::EditQueuedConversationInput {
                        queue_id: sigil_kernel::ConversationInputQueueId::new(
                            entry_id.as_str().to_owned(),
                        )
                        .map_err(|_| {
                            ApplicationError::InvalidRequest("invalid queue id".to_owned())
                        })?,
                        prompt: prompt.as_str().to_owned(),
                        reasoning_effort: tui_reasoning_effort(
                            reasoning_effort
                                .unwrap_or(application_reasoning_effort(&self.reasoning_effort)),
                        ),
                    },
                    ApplicationQueueAction::Remove {
                        target: _,
                        entry_id,
                    } => WorkerCommand::CancelQueuedConversationInput {
                        queue_id: sigil_kernel::ConversationInputQueueId::new(
                            entry_id.as_str().to_owned(),
                        )
                        .map_err(|_| {
                            ApplicationError::InvalidRequest("invalid queue id".to_owned())
                        })?,
                    },
                    ApplicationQueueAction::Move {
                        target: _,
                        entry_id,
                        direction,
                    } => WorkerCommand::MoveQueuedConversationInput {
                        queue_id: sigil_kernel::ConversationInputQueueId::new(
                            entry_id.as_str().to_owned(),
                        )
                        .map_err(|_| {
                            ApplicationError::InvalidRequest("invalid queue id".to_owned())
                        })?,
                        direction: match direction {
                            ApplicationQueueMoveDirection::Up => {
                                crate::runner::QueueMoveDirection::Up
                            }
                            ApplicationQueueMoveDirection::Down => {
                                crate::runner::QueueMoveDirection::Down
                            }
                        },
                    },
                    ApplicationQueueAction::Promote {
                        target: _,
                        entry_id,
                    } => WorkerCommand::PromoteQueuedConversationInput {
                        queue_id: sigil_kernel::ConversationInputQueueId::new(
                            entry_id.as_str().to_owned(),
                        )
                        .map_err(|_| {
                            ApplicationError::InvalidRequest("invalid queue id".to_owned())
                        })?,
                    },
                    ApplicationQueueAction::SendNow {
                        target: _,
                        entry_id,
                    } => WorkerCommand::SendQueuedConversationInputNow {
                        queue_id: sigil_kernel::ConversationInputQueueId::new(
                            entry_id.as_str().to_owned(),
                        )
                        .map_err(|_| {
                            ApplicationError::InvalidRequest("invalid queue id".to_owned())
                        })?,
                    },
                    ApplicationQueueAction::Pause => {
                        WorkerCommand::SetConversationQueuePaused { paused: true }
                    }
                    ApplicationQueueAction::Resume => {
                        WorkerCommand::SetConversationQueuePaused { paused: false }
                    }
                    ApplicationQueueAction::Reorder { .. }
                    | ApplicationQueueAction::InterruptAndRunNext { .. } => {
                        return Ok(sigil_runtime::RuntimeApplicationDispatch::Rejected(
                            sigil_application::CommandRejection {
                                kind: "unsupported_tui_queue_command".to_owned(),
                                reason: "the TUI worker adapter does not support this queue action"
                                    .to_owned(),
                            },
                        ));
                    }
                }
            }
            _ => {
                return Ok(sigil_runtime::RuntimeApplicationDispatch::Rejected(
                    sigil_application::CommandRejection {
                        kind: "unsupported_tui_command".to_owned(),
                        reason: "the TUI adapter has no lossless worker mapping".to_owned(),
                    },
                ));
            }
        };
        self.worker_tx
            .send(command)
            .map_err(|_| ApplicationError::Unavailable)?;
        let fingerprint = sigil_application::command_fingerprint(request)?;
        Ok(sigil_runtime::RuntimeApplicationDispatch::Uncertain(
            sigil_application::UncertainCommandReceipt {
                command_id: request.envelope.command_id.clone(),
                command_kind: request.envelope.command.kind().to_owned(),
                reservation_fingerprint: fingerprint,
                recovery_binding: "tui-worker-event-reconcile".to_owned(),
            },
        ))
    }
}

fn parse_approval_binding(binding: &str) -> Result<(String, String, String), ApplicationError> {
    let mut parts = binding.splitn(3, ':');
    let run_id = parts.next().unwrap_or_default();
    let call_id = parts.next().unwrap_or_default();
    let request_id = parts.next().unwrap_or_default();
    if run_id.is_empty() || call_id.is_empty() || request_id.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "approval binding is malformed".to_owned(),
        ));
    }
    Ok((run_id.to_owned(), call_id.to_owned(), request_id.to_owned()))
}

fn tui_queue_item_kind(kind: ApplicationQueueItemKind) -> sigil_kernel::ConversationInputKind {
    match kind {
        ApplicationQueueItemKind::Chat => sigil_kernel::ConversationInputKind::Chat,
        ApplicationQueueItemKind::PlanPrompt => sigil_kernel::ConversationInputKind::PlanPrompt,
        ApplicationQueueItemKind::AgentMention => sigil_kernel::ConversationInputKind::AgentMention,
        ApplicationQueueItemKind::AgentMessage => sigil_kernel::ConversationInputKind::AgentMessage,
        ApplicationQueueItemKind::TaskGuidance => sigil_kernel::ConversationInputKind::TaskGuidance,
        ApplicationQueueItemKind::Unknown => sigil_kernel::ConversationInputKind::Unknown,
    }
}

fn tui_queue_target(
    target: &ApplicationQueueTarget,
) -> Result<sigil_kernel::ConversationInputTarget, ApplicationError> {
    match target {
        ApplicationQueueTarget::MainThread => Ok(sigil_kernel::ConversationInputTarget::MainThread),
        ApplicationQueueTarget::AgentThread { thread_id } => {
            Ok(sigil_kernel::ConversationInputTarget::AgentThread {
                thread_id: sigil_kernel::AgentThreadId::new(thread_id.as_str().to_owned())
                    .map_err(|_| {
                        ApplicationError::InvalidRequest("invalid agent thread id".to_owned())
                    })?,
            })
        }
        ApplicationQueueTarget::Task { task_id } => {
            Ok(sigil_kernel::ConversationInputTarget::Task {
                task_id: sigil_kernel::TaskId::new(task_id.as_str().to_owned())
                    .map_err(|_| ApplicationError::InvalidRequest("invalid task id".to_owned()))?,
            })
        }
    }
}

fn tui_reasoning_effort(effort: ApplicationReasoningEffort) -> ReasoningEffort {
    match effort {
        ApplicationReasoningEffort::Low => ReasoningEffort::Low,
        ApplicationReasoningEffort::Medium => ReasoningEffort::Medium,
        ApplicationReasoningEffort::High => ReasoningEffort::High,
        ApplicationReasoningEffort::Max => ReasoningEffort::Max,
    }
}
