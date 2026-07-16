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

pub(in crate::runner) fn classify_worker_command(command: &WorkerCommand) -> WorkerCommandDomain {
    match command {
        WorkerCommand::SubmitPrompt { .. }
        | WorkerCommand::SubmitPromptWithAttachments { .. }
        | WorkerCommand::SubmitPlanPrompt { .. }
        | WorkerCommand::InvokeInlineSkill { .. }
        | WorkerCommand::ApprovalDecision { .. }
        | WorkerCommand::ApprovalSessionDecision { .. }
        | WorkerCommand::ApprovalDecisionWithArgs { .. }
        | WorkerCommand::ApprovalCommand(_)
        | WorkerCommand::CancelRun
        | WorkerCommand::ApprovePlan { .. }
        | WorkerCommand::RejectPlan { .. } => WorkerCommandDomain::RunPlan,
        WorkerCommand::InspectLocalSession { .. }
        | WorkerCommand::ForkLocalSession { .. }
        | WorkerCommand::ExportLocalSession { .. }
        | WorkerCommand::SetLocalSessionPin { .. }
        | WorkerCommand::PreviewLocalSessionDelete { .. }
        | WorkerCommand::ApplyLocalSessionDelete { .. }
        | WorkerCommand::PreviewSessionRetention { .. }
        | WorkerCommand::ApplySessionRetention { .. }
        | WorkerCommand::StartNewSession { .. }
        | WorkerCommand::SwitchSession { .. } => WorkerCommandDomain::Session,
        WorkerCommand::QueueConversationInput { .. }
        | WorkerCommand::CancelQueuedConversationInput { .. }
        | WorkerCommand::EditQueuedConversationInput { .. }
        | WorkerCommand::MoveQueuedConversationInput { .. }
        | WorkerCommand::PromoteQueuedConversationInput { .. }
        | WorkerCommand::SendQueuedConversationInputNow { .. }
        | WorkerCommand::SetConversationQueuePaused { .. }
        | WorkerCommand::PreviewV2Compaction
        | WorkerCommand::ApplyV2Compaction { .. }
        | WorkerCommand::CancelV2CompactionReview { .. } => WorkerCommandDomain::QueueCompaction,
        WorkerCommand::InvokeAgentProfile { .. }
        | WorkerCommand::InvokeChildSessionSkill { .. }
        | WorkerCommand::SubmitTask { .. }
        | WorkerCommand::ContinueTask { .. }
        | WorkerCommand::BackgroundActiveAgent
        | WorkerCommand::CancelTerminalTask { .. }
        | WorkerCommand::CreateTaskFromPlan { .. }
        | WorkerCommand::CloseAgent { .. }
        | WorkerCommand::CancelAgent { .. }
        | WorkerCommand::MessageAgent { .. } => WorkerCommandDomain::AgentTask,
        WorkerCommand::CheckChangedFilesDiagnostics
        | WorkerCommand::CleanMutationArtifacts { .. }
        | WorkerCommand::DeleteMutationArtifact { .. }
        | WorkerCommand::ApproveVerificationCheck { .. }
        | WorkerCommand::SandboxVerificationCheck { .. }
        | WorkerCommand::RerunTaskVerification { .. }
        | WorkerCommand::PreviewCheckpointRestore { .. }
        | WorkerCommand::ExecuteCheckpointRestore { .. }
        | WorkerCommand::ForkConversationAtCheckpoint { .. } => {
            WorkerCommandDomain::VerificationCheckpoint
        }
        WorkerCommand::RefreshProviderBalance { .. }
        | WorkerCommand::RefreshProviderModels { .. }
        | WorkerCommand::CancelProviderModelsRefresh { .. }
        | WorkerCommand::ActivateLazyMcp { .. }
        | WorkerCommand::RefreshMcpServer { .. } => WorkerCommandDomain::ProviderMcp,
        WorkerCommand::Shutdown => WorkerCommandDomain::Maintenance,
    }
}

pub(in crate::runner) fn dispatch_worker_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: WorkerCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    match classify_worker_command(&command) {
        WorkerCommandDomain::RunPlan => run_plan::dispatch_run_plan_command(context, command),
        WorkerCommandDomain::Session => session::dispatch_session_command(context, command),
        WorkerCommandDomain::QueueCompaction => {
            queue_compaction::dispatch_queue_compaction_command(context, command)
        }
        WorkerCommandDomain::AgentTask => agent_task::dispatch_agent_task_command(context, command),
        WorkerCommandDomain::VerificationCheckpoint => {
            verification_checkpoint::dispatch_verification_checkpoint_command(context, command)
        }
        WorkerCommandDomain::ProviderMcp => {
            provider_mcp::dispatch_provider_mcp_command(context, command)
        }
        WorkerCommandDomain::Maintenance => {
            maintenance::dispatch_maintenance_command(context, command)
        }
    }
}
