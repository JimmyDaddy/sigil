use crate::runner::WorkerCommandEnvelope;
use sigil_kernel::{
    ImageAttachment, MutationArtifactCleanupTarget, ResolvedModelRoute,
    TaskIntegrationReviewRequest, TaskVerificationRerunRequest,
};
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
    pub(in crate::runner) config_path: &'a PathBuf,
    pub(in crate::runner) provider_capabilities: &'a ProviderCapabilities,
    pub(in crate::runner) workspace_root: &'a PathBuf,
    pub(in crate::runner) options: &'a AgentRunOptions,
    pub(in crate::runner) permission_mode_override:
        &'a std::sync::Arc<sigil_kernel::PermissionModeOverride>,
    pub(in crate::runner) message_tx: &'a mpsc::Sender<WorkerMessage>,
    pub(in crate::runner) elicitation_handler: &'a Arc<ChannelMcpElicitationHandler>,
    pub(in crate::runner) mcp_event_handler: &'a Arc<ChannelMcpRuntimeEventHandler>,
    pub(in crate::runner) role_provider_builder: &'a Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) context_resolver: &'a sigil_runtime::RequestContextResolver,
    pub(in crate::runner) managed_extension_execution: &'a Option<
        Arc<sigil_runtime::managed_resource_adapters::RuntimeManagedExtensionExecutionRouteV1>,
    >,
    pub(in crate::runner) managed_verification_execution:
        &'a Option<Arc<dyn sigil_kernel::verification::VerificationExecutionPortV1>>,
    pub(in crate::runner) state: &'a mut WorkerLoopState,
}

impl<P> WorkerCommandContext<'_, P> {
    fn reborrow(&mut self) -> WorkerCommandContext<'_, P> {
        WorkerCommandContext {
            runtime: self.runtime,
            agent: self.agent,
            root_config: self.root_config,
            config_path: self.config_path,
            provider_capabilities: self.provider_capabilities,
            workspace_root: self.workspace_root,
            options: self.options,
            permission_mode_override: self.permission_mode_override,
            message_tx: self.message_tx,
            elicitation_handler: self.elicitation_handler,
            mcp_event_handler: self.mcp_event_handler,
            role_provider_builder: self.role_provider_builder,
            context_resolver: self.context_resolver,
            managed_extension_execution: self.managed_extension_execution,
            managed_verification_execution: self.managed_verification_execution,
            state: self.state,
        }
    }
}

mod agent_task;
mod intent_stack;
mod maintenance;
mod provider_mcp;
mod queue_compaction;
mod run_plan;
mod session;
mod verification_checkpoint;

#[cfg(test)]
pub(in crate::runner) use run_plan::preserve_revision_result_after_audit;
#[cfg(test)]
pub(in crate::runner) use session::read_tool_artifact_page_for_display;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum WorkerCommandDomain {
    RunPlan,
    Session,
    QueueCompaction,
    AgentTask,
    IntentStack,
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
    IntentStack(IntentStackCommand),
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
            Self::IntentStack(_) => WorkerCommandDomain::IntentStack,
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
        review: Option<(String, Vec<sigil_application::ReviewAnnotation>)>,
    },
    InvokeInlineSkill {
        skill_id: String,
        arguments: String,
        attachments: Vec<sigil_kernel::ImageAttachment>,
        reasoning_effort: ReasoningEffort,
    },
    ApprovalCommand(WorkerCommandEnvelope<WorkerApprovalCommand>),
    PauseTask {
        request: sigil_kernel::TaskPauseRequest,
    },
    CancelRun,
    RejectPlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    SavePlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    RevisePlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    AdoptPlanCandidate {
        plan_id: String,
        expected_candidate_hash: String,
    },
    RetryPlanReview {
        plan_id: String,
        expected_candidate_hash: Option<String>,
    },
    SubmitUserInputDecision {
        command_id: Option<String>,
        request_id: String,
        generation: u32,
        expected_request_hash: String,
        decision: sigil_kernel::UserInputDecisionV1,
    },
    ResumeRecoveredUserInput {
        command_id: String,
        request_id: String,
        generation: u32,
        expected_request_hash: String,
    },
}

#[derive(Debug)]
pub(in crate::runner) enum SessionCommand {
    LoadBranchKnowledge {
        request_id: u64,
        target_session_id: String,
        source_session_ref: sigil_kernel::SessionRef,
        source_session_id: String,
    },
    ImportBranchKnowledge {
        request_id: u64,
        target_session_id: String,
        request:
            sigil_runtime::application_branch_knowledge::ApplicationBranchKnowledgeImportRequest,
    },
    LoadConversationForkPoints {
        request_id: u64,
        source_session_id: String,
    },
    ForkConversation {
        request_id: u64,
        source_session_id: String,
        source_turn_digest: String,
        target_model_ref: sigil_kernel::ModelRef,
    },
    InspectLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    ReadToolArtifactPage {
        request_id: u64,
        artifact_ref: sigil_kernel::ToolArtifactRefV1,
        selector: sigil_kernel::ToolArtifactSelectorV1,
    },
    ForkLocalSession {
        request_id: u64,
        source_path: PathBuf,
        current_model_route: ResolvedModelRoute,
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
        attachment_recovery_binding: Option<String>,
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
    StartV2Compaction,
    PreviewV2Compaction,
    ApplyV2Compaction {
        request_id: u64,
    },
    ApplyStandaloneToolOutputShrink {
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
        identity: TerminalTaskControlIdentity,
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
    ReviewTaskIntegration {
        request: TaskIntegrationReviewRequest,
    },
    AcceptTaskIntegration {
        request: TaskIntegrationReviewRequest,
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
pub(in crate::runner) enum IntentStackCommand {
    Load {
        request_id: u64,
    },
    PreviewDrop {
        request_id: u64,
        intent_ref: sigil_kernel::IntentVersionRef,
    },
    ExecuteDrop {
        request_id: u64,
        request: sigil_kernel::IntentDropRequestV1,
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
    RefreshConnectionModels {
        cache_root: PathBuf,
        root_config: Box<RootConfig>,
        request: sigil_runtime::provider_connections::ModelCatalogRequest,
        prepared_credential: Option<sigil_runtime::provider_connections::PreparedCredential>,
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
    McpOAuth {
        server_name: String,
        action: McpOAuthUserAction,
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
        WorkerCommand::QueryApplicationOperation { .. }
        | WorkerCommand::FindCommittedApplicationOperation { .. }
        | WorkerCommand::PrepareApplicationOperation { .. }
        | WorkerCommand::ResumeCommittedUserInput { .. }
        | WorkerCommand::ApplicationDispatch { .. } => {
            unreachable!("application envelopes are handled before domain classification")
        }
        WorkerCommand::SubmitPrompt {
            prompt,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::Submit {
            prompt,
            attachments: Vec::new(),
            reasoning_effort,
            plan_mode: false,
            review: None,
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
            review: None,
        }),
        WorkerCommand::SubmitReviewedPrompt {
            prompt,
            attachments,
            reasoning_effort,
            expected_session_id,
            annotations,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::Submit {
            prompt,
            attachments,
            reasoning_effort,
            plan_mode: false,
            review: Some((expected_session_id, annotations)),
        }),
        WorkerCommand::SubmitPlanPrompt {
            prompt,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::Submit {
            prompt,
            attachments: Vec::new(),
            reasoning_effort,
            plan_mode: true,
            review: None,
        }),
        WorkerCommand::InvokeInlineSkill {
            skill_id,
            arguments,
            attachments,
            reasoning_effort,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::InvokeInlineSkill {
            skill_id,
            arguments,
            attachments,
            reasoning_effort,
        }),
        WorkerCommand::ApprovalCommand(command) => {
            ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ApprovalCommand(command))
        }
        WorkerCommand::CancelRun => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::CancelRun),
        // Handled before classification in dispatch_worker_command; unreachable here.
        WorkerCommand::UpdateActiveRunPermissionMode { .. } => {
            unreachable!("permission mode updates are handled before classification")
        }
        WorkerCommand::RejectPlan {
            plan_id,
            expected_plan_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::RejectPlan {
            plan_id,
            expected_plan_hash,
        }),
        WorkerCommand::SavePlan {
            plan_id,
            expected_plan_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::SavePlan {
            plan_id,
            expected_plan_hash,
        }),
        WorkerCommand::RevisePlan {
            plan_id,
            expected_plan_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::RevisePlan {
            plan_id,
            expected_plan_hash,
        }),
        WorkerCommand::AdoptPlanCandidate {
            plan_id,
            expected_candidate_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::AdoptPlanCandidate {
            plan_id,
            expected_candidate_hash,
        }),
        WorkerCommand::RetryPlanReview {
            plan_id,
            expected_candidate_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::RetryPlanReview {
            plan_id,
            expected_candidate_hash,
        }),
        WorkerCommand::SubmitUserInputDecision {
            command_id,
            request_id,
            generation,
            expected_request_hash,
            decision,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::SubmitUserInputDecision {
            command_id,
            request_id,
            generation,
            expected_request_hash,
            decision,
        }),
        WorkerCommand::ResumeRecoveredUserInput {
            command_id,
            request_id,
            generation,
            expected_request_hash,
        } => ClassifiedWorkerCommand::RunPlan(RunPlanCommand::ResumeRecoveredUserInput {
            command_id,
            request_id,
            generation,
            expected_request_hash,
        }),
        WorkerCommand::LoadBranchKnowledge {
            request_id,
            target_session_id,
            source_session_ref,
            source_session_id,
        } => ClassifiedWorkerCommand::Session(SessionCommand::LoadBranchKnowledge {
            request_id,
            target_session_id,
            source_session_ref,
            source_session_id,
        }),
        WorkerCommand::ImportBranchKnowledge {
            request_id,
            target_session_id,
            request,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ImportBranchKnowledge {
            request_id,
            target_session_id,
            request,
        }),
        WorkerCommand::LoadConversationForkPoints {
            request_id,
            source_session_id,
        } => ClassifiedWorkerCommand::Session(SessionCommand::LoadConversationForkPoints {
            request_id,
            source_session_id,
        }),
        WorkerCommand::ForkConversation {
            request_id,
            source_session_id,
            source_turn_digest,
            target_model_ref,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ForkConversation {
            request_id,
            source_session_id,
            source_turn_digest,
            target_model_ref,
        }),
        WorkerCommand::InspectLocalSession {
            request_id,
            source_path,
        } => ClassifiedWorkerCommand::Session(SessionCommand::InspectLocalSession {
            request_id,
            source_path,
        }),
        WorkerCommand::ReadToolArtifactPage {
            request_id,
            artifact_ref,
            selector,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ReadToolArtifactPage {
            request_id,
            artifact_ref,
            selector,
        }),
        WorkerCommand::ForkLocalSession {
            request_id,
            source_path,
            current_model_route,
        } => ClassifiedWorkerCommand::Session(SessionCommand::ForkLocalSession {
            request_id,
            source_path,
            current_model_route,
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
        WorkerCommand::SwitchSession {
            session_log_path,
            attachment_recovery_binding,
        } => ClassifiedWorkerCommand::Session(SessionCommand::SwitchSession {
            session_log_path,
            attachment_recovery_binding,
        }),
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
        WorkerCommand::StartV2Compaction => {
            ClassifiedWorkerCommand::QueueCompaction(QueueCompactionCommand::StartV2Compaction)
        }
        WorkerCommand::PreviewV2Compaction => {
            ClassifiedWorkerCommand::QueueCompaction(QueueCompactionCommand::PreviewV2Compaction)
        }
        WorkerCommand::ApplyV2Compaction { request_id } => {
            ClassifiedWorkerCommand::QueueCompaction(QueueCompactionCommand::ApplyV2Compaction {
                request_id,
            })
        }
        WorkerCommand::ApplyStandaloneToolOutputShrink { request_id } => {
            ClassifiedWorkerCommand::QueueCompaction(
                QueueCompactionCommand::ApplyStandaloneToolOutputShrink { request_id },
            )
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
        WorkerCommand::PauseTask { request } => {
            ClassifiedWorkerCommand::RunPlan(RunPlanCommand::PauseTask { request })
        }
        WorkerCommand::BackgroundActiveAgent => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::BackgroundActiveAgent)
        }
        WorkerCommand::CancelTerminalTask { identity } => {
            ClassifiedWorkerCommand::AgentTask(AgentTaskCommand::CancelTerminalTask { identity })
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
        WorkerCommand::ReviewTaskIntegration { request } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::ReviewTaskIntegration { request },
            )
        }
        WorkerCommand::AcceptTaskIntegration { request } => {
            ClassifiedWorkerCommand::VerificationCheckpoint(
                VerificationCheckpointCommand::AcceptTaskIntegration { request },
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
        WorkerCommand::LoadIntentStack { request_id } => {
            ClassifiedWorkerCommand::IntentStack(IntentStackCommand::Load { request_id })
        }
        WorkerCommand::PreviewIntentDrop {
            request_id,
            intent_ref,
        } => ClassifiedWorkerCommand::IntentStack(IntentStackCommand::PreviewDrop {
            request_id,
            intent_ref,
        }),
        WorkerCommand::ExecuteIntentDrop {
            request_id,
            request,
        } => ClassifiedWorkerCommand::IntentStack(IntentStackCommand::ExecuteDrop {
            request_id,
            request,
        }),
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
        WorkerCommand::RefreshConnectionModels {
            cache_root,
            root_config,
            request,
            prepared_credential,
        } => ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::RefreshConnectionModels {
            cache_root,
            root_config,
            request,
            prepared_credential,
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
        WorkerCommand::McpOAuth {
            server_name,
            action,
        } => ClassifiedWorkerCommand::ProviderMcp(ProviderMcpCommand::McpOAuth {
            server_name,
            action,
        }),
        WorkerCommand::Shutdown => {
            ClassifiedWorkerCommand::Maintenance(MaintenanceCommand::Shutdown)
        }
    }
}

fn managed_research_operation_parent(
    owner: &sigil_kernel::SessionApplicationOperationOwner,
    target: &sigil_kernel::ApplicationOperationTargetV1,
    observe_only: bool,
) -> anyhow::Result<Option<Session>> {
    if !matches!(
        target,
        sigil_kernel::ApplicationOperationTargetV1::UserInputDecision { .. }
            | sigil_kernel::ApplicationOperationTargetV1::UserInputContinuation { .. }
    ) {
        return Ok(None);
    }
    let parent = if observe_only {
        owner.attach_for_observation()?
    } else {
        owner.attach_for_control()?
    };
    Ok(
        sigil_runtime::PlanReviewCoordinator::is_managed_research_application_target(
            &parent, target,
        )?
        .then_some(parent),
    )
}

fn query_worker_application_operation(
    state: &WorkerLoopState,
    binding: &sigil_kernel::ApplicationOperationBindingV1,
) -> anyhow::Result<(
    sigil_kernel::ApplicationOperationBindingV1,
    Option<sigil_kernel::session::ApplicationOperationCommitProofV1>,
)> {
    let owner = state
        .session
        .application_operation_owner
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("application operation owner unavailable"))?;
    if let Some(parent) = managed_research_operation_parent(owner, &binding.target, true)? {
        let provisioner = state
            .managed_plan_review_child_resources
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("managed research operation owner unavailable"))?;
        return sigil_runtime::PlanReviewCoordinator::query_managed_research_application_operation(
            &parent,
            binding,
            provisioner,
        )?
        .ok_or_else(|| anyhow::anyhow!("managed research target changed"));
    }
    let (binding, reader) = owner.observe_operation(binding)?;
    let proof = sigil_kernel::session::reconcile_application_operation(&reader, &binding)?;
    Ok((binding, proof))
}

pub(in crate::runner) fn dispatch_worker_command<P>(
    mut context: WorkerCommandContext<'_, P>,
    command: WorkerCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let command = match command {
        WorkerCommand::ResumeCommittedUserInput { original_operation } => {
            let recovered = (|| -> anyhow::Result<WorkerCommand> {
                query_worker_application_operation(context.state, &original_operation)?
                    .1
                    .ok_or_else(|| {
                        anyhow::anyhow!("input continuation lacks its original committed decision")
                    })?;
                let sigil_kernel::ApplicationOperationTargetV1::UserInputDecision {
                    request_id,
                    generation,
                    request_hash,
                    command_id,
                } = &original_operation.target
                else {
                    anyhow::bail!("continuation does not target a user input decision");
                };
                Ok(WorkerCommand::ResumeRecoveredUserInput {
                    command_id: command_id.clone(),
                    request_id: request_id.clone(),
                    generation: *generation,
                    expected_request_hash: request_hash.clone(),
                })
            })();
            match recovered {
                Ok(command) => return dispatch_worker_command(context, command),
                Err(error) => {
                    let _ = context.message_tx.send(WorkerMessage::Notice(format!(
                        "user input continuation unavailable: {error:#}"
                    )));
                    return WorkerCommandDispatchControl::Continue;
                }
            }
        }
        WorkerCommand::QueryApplicationOperation { binding, reply } => {
            let result = query_worker_application_operation(context.state, &binding);
            let _ = reply.send(result.map_err(|error| format!("{error:#}")));
            return WorkerCommandDispatchControl::Continue;
        }
        WorkerCommand::FindCommittedApplicationOperation {
            target,
            key_digest,
            reply,
        } => {
            let result = (|| -> anyhow::Result<_> {
                let owner = context
                    .state
                    .session
                    .application_operation_owner
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("application operation owner unavailable"))?;
                if let Some(parent) = managed_research_operation_parent(owner, &target, true)? {
                    let provisioner = context
                        .state
                        .managed_plan_review_child_resources
                        .as_deref()
                        .ok_or_else(|| {
                            anyhow::anyhow!("managed research operation owner unavailable")
                        })?;
                    return sigil_runtime::PlanReviewCoordinator::committed_managed_research_application_operation(
                        &parent, &target, &key_digest, provisioner,
                    )?.ok_or_else(|| anyhow::anyhow!("managed research target changed"));
                }
                let (scope, reader) = owner.observe_target(&target)?;
                sigil_kernel::session::committed_application_operation(&reader, &scope, &key_digest)
            })();
            let _ = reply.send(result.map_err(|error| format!("{error:#}")));
            return WorkerCommandDispatchControl::Continue;
        }
        WorkerCommand::PrepareApplicationOperation { binding, reply } => {
            let result = (|| -> anyhow::Result<()> {
                let owner = context
                    .state
                    .session
                    .application_operation_owner
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow::anyhow!("application operation session owner is unavailable")
                    })?;
                if let Some(parent) =
                    managed_research_operation_parent(owner, &binding.target, false)?
                {
                    let provisioner = context
                        .state
                        .managed_plan_review_child_resources
                        .as_deref()
                        .ok_or_else(|| {
                            anyhow::anyhow!("managed research operation owner unavailable")
                        })?;
                    sigil_runtime::PlanReviewCoordinator::prepare_managed_research_application_operation(
                        &parent, &binding, provisioner,
                    )?.ok_or_else(|| anyhow::anyhow!("managed research target changed"))?;
                    return Ok(());
                }
                owner.prepare(&binding)?;
                let (_, binding) = owner.resolve_operation(&binding)?;
                if binding.domain_session_scope_id() != binding.session_scope_id {
                    return Ok(());
                }
                let entry = ControlEntry::ApplicationOperationPreparedV1(binding);
                if let Some(session) = context.state.session.current.as_mut() {
                    if !session.entries().iter().any(|existing| matches!(existing,SessionLogEntry::Control(ControlEntry::ApplicationOperationPreparedV1(prior)) if matches!(&entry,ControlEntry::ApplicationOperationPreparedV1(current) if prior==current))) {
                        session.record_durably_appended_controls([entry]);
                    }
                } else {
                    context.state.session.detached_durable_controls.push(entry);
                }
                Ok(())
            })();
            let _ = reply.send(result.map_err(|error| format!("{error:#}")));
            return WorkerCommandDispatchControl::Continue;
        }
        WorkerCommand::ApplicationDispatch {
            binding,
            run_admission,
            command,
            reply,
        } => {
            let _admission = match run_admission.as_ref().map(|admission| admission.enter()) {
                Some(Some(guard)) => Some(guard),
                Some(None) => {
                    let _ = reply.send(Ok(
                        crate::runner::WorkerApplicationDispatchOutcome::CancelledBeforeDispatch,
                    ));
                    return WorkerCommandDispatchControl::Continue;
                }
                None => None,
            };
            let detached = context.state.session.current.is_none();
            let mut initial_entry_count = 0;
            if let Some(binding) = binding {
                let bound = (|| -> anyhow::Result<()> {
                    if context.state.session.current.is_none() {
                        let owner = context
                            .state
                            .session
                            .application_operation_owner
                            .as_ref()
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "application operation session owner is unavailable"
                                )
                            })?;
                        context.state.session.current = Some(owner.attach_for_control()?);
                    }
                    let session = context.state.session.current.as_mut().ok_or_else(|| {
                        anyhow::anyhow!("application operation attachment is unavailable")
                    })?;
                    initial_entry_count = session.entries().len();
                    if sigil_runtime::PlanReviewCoordinator::is_managed_research_application_target(
                        session,
                        &binding.target,
                    )? {
                        let provisioner = context
                            .state
                            .managed_plan_review_child_resources
                            .as_deref()
                            .ok_or_else(|| {
                                anyhow::anyhow!("managed research operation owner unavailable")
                            })?;
                        if !sigil_runtime::PlanReviewCoordinator::bind_managed_research_application_operation(
                            session, &binding, provisioner,
                        )? { anyhow::bail!("managed research target changed"); }
                        Ok(())
                    } else {
                        session.bind_application_operation(*binding)
                    }
                })();
                if let Err(error) = bound {
                    if detached {
                        context.state.session.current = None;
                    }
                    let _ = reply.send(Err(format!("{error:#}")));
                    return WorkerCommandDispatchControl::Continue;
                }
            }
            let control = dispatch_worker_command(context.reborrow(), *command);
            if let Some(session) = context.state.session.current.as_mut() {
                session.clear_application_operation();
            }
            if detached
                && initial_entry_count > 0
                && let Some(session) = context.state.session.current.take()
            {
                context.state.session.detached_durable_controls.extend(
                    session.entries()[initial_entry_count..]
                        .iter()
                        .filter_map(|entry| {
                            if let SessionLogEntry::Control(control) = entry {
                                Some(control.clone())
                            } else {
                                None
                            }
                        }),
                );
            }
            // This acknowledges actual owner dispatch. The application still requires a causal
            // durable receipt before reporting a committed command.
            let _ = reply.send(Ok(
                crate::runner::WorkerApplicationDispatchOutcome::Dispatched,
            ));
            return control;
        }
        command => command,
    };
    context.state.defer_startup_artifact_gc = false;
    if let WorkerCommand::UpdateActiveRunPermissionMode { mode } = command {
        context.permission_mode_override.set(mode);
        let _ = context.message_tx.send(WorkerMessage::Notice(format!(
            "permission mode -> {} (active run)",
            mode.as_str()
        )));
        return WorkerCommandDispatchControl::Continue;
    }
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
        ClassifiedWorkerCommand::IntentStack(command) => {
            intent_stack::dispatch_intent_stack_command(context, command)
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
