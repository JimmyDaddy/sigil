use crate::runner::WorkerCommandEnvelope;
use sigil_kernel::{ImageAttachment, MutationArtifactCleanupTarget, TaskVerificationRerunRequest};
use sigil_runtime::{
    ProviderStatusConfig, SessionDeletePreview, SessionRetentionPolicy, SessionRetentionPreview,
};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum WorkerCommandDispatchControl {
    Continue,
    Break,
}

pub(in crate::runner) struct WorkerCommandContext<'a, P> {
    pub(in crate::runner) runtime: &'a tokio::runtime::Runtime,
    pub(in crate::runner) agent: &'a mut Arc<Agent<P>>,
    pub(in crate::runner) root_config: &'a RootConfig,
    pub(in crate::runner) provider_capabilities: &'a ProviderCapabilities,
    pub(in crate::runner) workspace_root: &'a PathBuf,
    pub(in crate::runner) options: &'a AgentRunOptions,
    pub(in crate::runner) message_tx: &'a mpsc::Sender<WorkerMessage>,
    pub(in crate::runner) elicitation_handler: &'a Arc<ChannelMcpElicitationHandler>,
    pub(in crate::runner) mcp_event_handler: &'a Arc<ChannelMcpRuntimeEventHandler>,
    pub(in crate::runner) role_provider_builder: &'a Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) context_resolver: &'a sigil_runtime::RequestContextResolver,
    pub(in crate::runner) state: &'a mut WorkerLoopState,
}

mod agent_task;
mod maintenance;
mod provider_mcp;
mod queue_compaction;
mod run_plan;
mod session;
mod verification_checkpoint;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum WorkerCommandDomain {
    RunPlan,
    Session,
    QueueCompaction,
    AgentTask,
    VerificationCheckpoint,
    ProviderMcp,
    Maintenance,
}

#[derive(Debug)]
pub(in crate::runner) enum ClassifiedWorkerCommand {
    RunPlan(RunPlanCommand),
    Session(SessionCommand),
    QueueCompaction(QueueCompactionCommand),
    AgentTask(AgentTaskCommand),
    VerificationCheckpoint(VerificationCheckpointCommand),
    ProviderMcp(ProviderMcpCommand),
    Maintenance(MaintenanceCommand),
}

impl ClassifiedWorkerCommand {
    #[cfg(test)]
    pub(in crate::runner) fn domain(&self) -> WorkerCommandDomain {
        match self {
            Self::RunPlan(_) => WorkerCommandDomain::RunPlan,
            Self::Session(_) => WorkerCommandDomain::Session,
            Self::QueueCompaction(_) => WorkerCommandDomain::QueueCompaction,
            Self::AgentTask(_) => WorkerCommandDomain::AgentTask,
            Self::VerificationCheckpoint(_) => WorkerCommandDomain::VerificationCheckpoint,
            Self::ProviderMcp(_) => WorkerCommandDomain::ProviderMcp,
            Self::Maintenance(_) => WorkerCommandDomain::Maintenance,
        }
    }
}

#[derive(Debug)]
pub(in crate::runner) enum RunPlanCommand {
    Submit {
        prompt: String,
        attachments: Vec<ImageAttachment>,
        reasoning_effort: ReasoningEffort,
        plan_mode: bool,
    },
    InvokeInlineSkill {
        skill_id: String,
        arguments: String,
        reasoning_effort: ReasoningEffort,
    },
    ApprovalDecision {
        call_id: String,
        approved: bool,
    },
    ApprovalSessionDecision {
        call_id: String,
    },
    ApprovalDecisionWithArgs {
        call_id: String,
        args_json: String,
    },
    ApprovalCommand(WorkerCommandEnvelope<WorkerApprovalCommand>),
    CancelRun,
    ApprovePlan {
        plan_text: String,
        permission: PlanApprovalPermission,
        scope_summary: String,
        clear_planning_context: bool,
    },
    RejectPlan {
        plan_id: String,
        expected_plan_hash: String,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum SessionCommand {
    InspectLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    ForkLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    ExportLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    SetLocalSessionPin {
        request_id: u64,
        source_path: PathBuf,
        pinned: bool,
    },
    PreviewLocalSessionDelete {
        request_id: u64,
        source_path: PathBuf,
    },
    ApplyLocalSessionDelete {
        request_id: u64,
        preview: SessionDeletePreview,
    },
    PreviewSessionRetention {
        request_id: u64,
        policy: SessionRetentionPolicy,
    },
    ApplySessionRetention {
        request_id: u64,
        preview: SessionRetentionPreview,
    },
    StartNewSession {
        session_log_path: PathBuf,
    },
    SwitchSession {
        session_log_path: PathBuf,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum QueueCompactionCommand {
    QueueConversationInput {
        prompt: String,
        kind: ConversationInputKind,
        target: ConversationInputTarget,
        reasoning_effort: ReasoningEffort,
    },
    CancelQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    EditQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    MoveQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        direction: QueueMoveDirection,
    },
    PromoteQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    SendQueuedConversationInputNow {
        queue_id: ConversationInputQueueId,
    },
    SetConversationQueuePaused {
        paused: bool,
    },
    PreviewV2Compaction,
    ApplyV2Compaction {
        request_id: u64,
    },
    CancelV2CompactionReview {
        request_id: u64,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum AgentTaskCommand {
    InvokeAgentProfile {
        profile_id: String,
        prompt: String,
        parent_prompt: String,
    },
    InvokeChildSessionSkill {
        skill_id: String,
        arguments: String,
    },
    SubmitTask {
        prompt: String,
    },
    ContinueTask {
        task_id: Option<String>,
        guidance: Option<String>,
    },
    BackgroundActiveAgent,
    CancelTerminalTask {
        task_id: String,
    },
    CreateTaskFromPlan {
        plan_id: String,
        expected_plan_hash: String,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    },
    CloseAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    CancelAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    MessageAgent {
        thread_id: AgentThreadId,
        prompt: String,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum VerificationCheckpointCommand {
    CheckChangedFilesDiagnostics,
    CleanMutationArtifacts {
        target: MutationArtifactCleanupTarget,
    },
    DeleteMutationArtifact {
        artifact_id: String,
    },
    ApproveVerificationCheck {
        check_spec_id: String,
    },
    SandboxVerificationCheck {
        check_spec_id: String,
    },
    RerunTaskVerification {
        request: TaskVerificationRerunRequest,
    },
    PreviewCheckpointRestore {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    ExecuteCheckpointRestore {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    ForkConversationAtCheckpoint {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum ProviderMcpCommand {
    RefreshProviderBalance {
        request_id: u64,
        provider_config: ProviderStatusConfig,
    },
    RefreshProviderModels {
        request_id: u64,
        provider_config: ProviderStatusConfig,
    },
    CancelProviderModelsRefresh {
        request_id: u64,
    },
    ActivateLazyMcp {
        server_name: Option<String>,
    },
    RefreshMcpServer {
        server_name: String,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum MaintenanceCommand {
    Shutdown,
}

pub(in crate::runner) fn classify_worker_command(
    command: WorkerCommand,
) -> ClassifiedWorkerCommand {
    match command {
        WorkerCommand::SubmitPrompt {
            prompt,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::Submit {
            prompt,
            attachments: Vec::new(),
            reasoning_effort,
            plan_mode: false,
        }),
        WorkerCommand::SubmitPromptWithAttachments {
            prompt,
            attachments,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::Submit {
            prompt,
            attachments,
            reasoning_effort,
            plan_mode: false,
        }),
        WorkerCommand::SubmitPlanPrompt {
            prompt,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::Submit {
            prompt,
            attachments: Vec::new(),
            reasoning_effort,
            plan_mode: true,
        }),
        WorkerCommand::InvokeInlineSkill {
            skill_id,
            arguments,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::InvokeInlineSkill {
            skill_id,
            arguments,
            reasoning_effort,
        }),
        WorkerCommand::ApprovalDecision { call_id, approved } => {
            ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ApprovalDecision { call_id, approved })
        }
        WorkerCommand::ApprovalSessionDecision { call_id } => {
            ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ApprovalSessionDecision { call_id })
        }
        WorkerCommand::ApprovalDecisionWithArgs { call_id, args_json } => {
            ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ApprovalDecisionWithArgs {
                call_id,
                args_json,
            })
        }
        WorkerCommand::ApprovalCommand(command) => {
            ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ApprovalCommand(command))
        }
        WorkerCommand::CancelRun => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::CancelRun),
        WorkerCommand::ApprovePlan {
            plan_text,
            permission,
            scope_summary,
            clear_planning_context,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ApprovePlan {
            plan_text,
            permission,
            scope_summary,
            clear_planning_context,
        }),
        WorkerCommand::RejectPlan {
            plan_id,
            expected_plan_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::RejectPlan {
            plan_id,
            expected_plan_hash,
        }),
        WorkerCommand::InspectLocalSession {
            request_id,
            source_path,
        } => ClassifiedWorkerCommand::Session(SessionCommand::InspectLocalSession {
            request_id,
            source_path,
        }),
        WorkerCommand::ForkLocalSession {
            request_id,
            source_path,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ForkLocalSession {
            request_id,
            source_path,
        }),
        WorkerCommand::ExportLocalSession {
            request_id,
            source_path,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ExportLocalSession {
            request_id,
            source_path,
        }),
        WorkerCommand::SetLocalSessionPin {
            request_id,
            source_path,
            pinned,
        } => ClassifiedWorkerCommand::Session(SessionCommand::SetLocalSessionPin {
            request_id,
            source_path,
            pinned,
        }),
        WorkerCommand::PreviewLocalSessionDelete {
            request_id,
            source_path,
        } => ClassifiedWorkerCommand::Session(SessionCommand::PreviewLocalSessionDelete {
            request_id,
            source_path,
        }),
        WorkerCommand::ApplyLocalSessionDelete {
            request_id,
            preview,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ApplyLocalSessionDelete {
            request_id,
            preview,
        }),
        WorkerCommand::PreviewSessionRetention { request_id, policy } => {
            ClassifiedWorkerCommand::Session(SessionCommand::PreviewSessionRetention {
                request_id,
                policy,
            })
        }
        WorkerCommand::ApplySessionRetention {
            request_id,
            preview,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ApplySessionRetention {
            request_id,
            preview,
        }),
        WorkerCommand::StartNewSession { session_log_path } => {
            ClassifiedWorkerCommand::Session(SessionCommand::StartNewSession { session_log_path })
        }
        WorkerCommand::SwitchSession { session_log_path } => {
            ClassifiedWorkerCommand::Session(SessionCommand::SwitchSession { session_log_path })
        }
        WorkerCommand::QueueConversationInput {
            prompt,
            kind,
            target,
            reasoning_effort,
        } => ClassifiedWorkerCommand::QueueCompaction(
            QueueCompactionCommand::QueueConversationInput {
                prompt,
                kind,
                target,
                reasoning_effort,
            },
        ),
        WorkerCommand::CancelQueuedConversationInput { queue_id } => {
            ClassifiedWorkerCommand::QueueCompaction(
                QueueCompactionCommand::CancelQueuedConversationInput { queue_id },
            )
        }
        WorkerCommand::EditQueuedConversationInput {
            queue_id,
            prompt,
            reasoning_effort,
        } => ClassifiedWorkerCommand::QueueCompaction(
            QueueCompactionCommand::EditQueuedConversationInput {
                queue_id,
                prompt,
                reasoning_effort,
            },
        ),
        WorkerCommand::MoveQueuedConversationInput {
            queue_id,
            direction,
        } => ClassifiedWorkerCommand::QueueCompaction(
            QueueCompactionCommand::MoveQueuedConversationInput {
                queue_id,
                direction,
            },
        ),
        WorkerCommand::PromoteQueuedConversationInput { queue_id } => {
            ClassifiedWorkerCommand::QueueCompaction(
                QueueCompactionCommand::PromoteQueuedConversationInput { queue_id },
            )
        }
        WorkerCommand::SendQueuedConversationInputNow { queue_id } => {
            ClassifiedWorkerCommand::QueueCompaction(
                QueueCompactionCommand::SendQueuedConversationInputNow { queue_id },
            )
        }
        WorkerCommand::SetConversationQueuePaused { paused } => {
            ClassifiedWorkerCommand::QueueCompaction(
                QueueCompactionCommand::SetConversationQueuePaused { paused },
            )
        }
        WorkerCommand::PreviewV2Compaction => {
            ClassifiedWorkerCommand::QueueCompaction(QueueCompactionCommand::PreviewV2Compaction)
        }
        WorkerCommand::ApplyV2Compaction { request_id } => {
            ClassifiedWorkerCommand::QueueCompaction(QueueCompactionCommand::ApplyV2Compaction {
                request_id,
            })
        }
        WorkerCommand::CancelV2CompactionReview { request_id } => {
            ClassifiedWorkerCommand::QueueCompaction(
                QueueCompactionCommand::CancelV2CompactionReview { request_id },
            )
        }
        WorkerCommand::InvokeAgentProfile {
            profile_id,
            prompt,
            parent_prompt,
        } => ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::InvokeAgentProfile {
            profile_id,
            prompt,
            parent_prompt,
        }),
        WorkerCommand::InvokeChildSessionSkill {
            skill_id,
            arguments,
        } => ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::InvokeChildSessionSkill {
            skill_id,
            arguments,
        }),
        WorkerCommand::SubmitTask { prompt } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::SubmitTask { prompt })
        }
        WorkerCommand::ContinueTask { task_id, guidance } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::ContinueTask { task_id, guidance })
        }
        WorkerCommand::BackgroundActiveAgent => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::BackgroundActiveAgent)
        }
        WorkerCommand::CancelTerminalTask { task_id } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::CancelTerminalTask { task_id })
        }
        WorkerCommand::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash,
            start_mode,
            permission_grant,
        } => ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash,
            start_mode,
            permission_grant,
        }),
        WorkerCommand::CloseAgent { thread_id, reason } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::CloseAgent { thread_id, reason })
        }
        WorkerCommand::CancelAgent { thread_id, reason } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::CancelAgent { thread_id, reason })
        }
        WorkerCommand::MessageAgent { thread_id, prompt } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::MessageAgent { thread_id, prompt })
        }
        WorkerCommand::CheckChangedFilesDiagnostics => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::CheckChangedFilesDiagnostics,
            )
        }
        WorkerCommand::CleanMutationArtifacts { target } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::CleanMutationArtifacts { target },
            )
        }
        WorkerCommand::DeleteMutationArtifact { artifact_id } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::DeleteMutationArtifact { artifact_id },
            )
        }
        WorkerCommand::ApproveVerificationCheck { check_spec_id } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::ApproveVerificationCheck { check_spec_id },
            )
        }
        WorkerCommand::SandboxVerificationCheck { check_spec_id } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::SandboxVerificationCheck { check_spec_id },
            )
        }
        WorkerCommand::RerunTaskVerification { request } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::RerunTaskVerification { request },
            )
        }
        WorkerCommand::PreviewCheckpointRestore {
            request_id,
            request,
        } => ClassifiedWorkerCommand::VerificationCheckpoint(
            VerificationCheckpointCommand::PreviewCheckpointRestore {
                request_id,
                request,
            },
        ),
        WorkerCommand::ExecuteCheckpointRestore {
            request_id,
            request,
        } => ClassifiedWorkerCommand::VerificationCheckpoint(
            VerificationCheckpointCommand::ExecuteCheckpointRestore {
                request_id,
                request,
            },
        ),
        WorkerCommand::ForkConversationAtCheckpoint {
            request_id,
            request,
        } => ClassifiedWorkerCommand::VerificationCheckpoint(
            VerificationCheckpointCommand::ForkConversationAtCheckpoint {
                request_id,
                request,
            },
        ),
        WorkerCommand::RefreshProviderBalance {
            request_id,
            provider_config,
        } => ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::RefreshProviderBalance {
            request_id,
            provider_config,
        }),
        WorkerCommand::RefreshProviderModels {
            request_id,
            provider_config,
        } => ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::RefreshProviderModels {
            request_id,
            provider_config,
        }),
        WorkerCommand::CancelProviderModelsRefresh { request_id } => {
            ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::CancelProviderModelsRefresh {
                request_id,
            })
        }
        WorkerCommand::ActivateLazyMcp { server_name } => {
            ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::ActivateLazyMcp {
                server_name,
            })
        }
        WorkerCommand::RefreshMcpServer { server_name } => {
            ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::RefreshMcpServer {
                server_name,
            })
        }
        WorkerCommand::Shutdown => {
            ClassifiedWorkerCommand::Maintenance(MaintenanceCommand::Shutdown)
        }
    }
}

pub(in crate::runner) fn dispatch_worker_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: WorkerCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    match classify_worker_command(command) {
        ClassifiedWorkerCommand::RunPlan(command) => {
            run_plan::dispatch_run_plan_command(context, command)
        }
        ClassifiedWorkerCommand::Session(command) => {
            session::dispatch_session_command(context, command)
        }
        ClassifiedWorkerCommand::QueueCompaction(command) => {
            queue_compaction::dispatch_queue_compaction_command(context, command)
        }
        ClassifiedWorkerCommand::AgentTask(command) => {
            agent_task::dispatch_agent_task_command(context, command)
        }
        ClassifiedWorkerCommand::VerificationCheckpoint(command) => {
            verification_checkpoint::dispatch_verification_checkpoint_command(context, command)
        }
        ClassifiedWorkerCommand::ProviderMcp(command) => {
            provider_mcp::dispatch_provider_mcp_command(context, command)
        }
        ClassifiedWorkerCommand::Maintenance(command) => {
            maintenance::dispatch_maintenance_command(context, command)
        }
    }
}
