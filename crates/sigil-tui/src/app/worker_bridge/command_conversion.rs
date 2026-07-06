use sha2::{Digest, Sha256};

use super::super::{AppAction, AppState};
use crate::runner::{WorkerApprovalCommand, WorkerCommand, WorkerCommandEnvelope};

impl AppState {
    pub fn shutdown_command() -> WorkerCommand {
        WorkerCommand::Shutdown
    }

    pub fn into_worker_command(&self, action: AppAction) -> WorkerCommand {
        match action {
            AppAction::SubmitPrompt(prompt) => WorkerCommand::SubmitPrompt {
                prompt,
                reasoning_effort: self.runtime.reasoning_effort.clone(),
            },
            AppAction::QueueConversationInput {
                prompt,
                kind,
                target,
            } => WorkerCommand::QueueConversationInput {
                prompt,
                kind,
                target,
                reasoning_effort: self.runtime.reasoning_effort.clone(),
            },
            AppAction::CancelQueuedConversationInput { queue_id } => {
                WorkerCommand::CancelQueuedConversationInput { queue_id }
            }
            AppAction::EditQueuedConversationInput { queue_id, prompt } => {
                WorkerCommand::EditQueuedConversationInput {
                    queue_id,
                    prompt,
                    reasoning_effort: self.runtime.reasoning_effort.clone(),
                }
            }
            AppAction::MoveQueuedConversationInput {
                queue_id,
                direction,
            } => WorkerCommand::MoveQueuedConversationInput {
                queue_id,
                direction,
            },
            AppAction::PromoteQueuedConversationInput { queue_id } => {
                WorkerCommand::PromoteQueuedConversationInput { queue_id }
            }
            AppAction::SendQueuedConversationInputNow { queue_id } => {
                WorkerCommand::SendQueuedConversationInputNow { queue_id }
            }
            AppAction::SetConversationQueuePaused { paused } => {
                WorkerCommand::SetConversationQueuePaused { paused }
            }
            AppAction::SubmitPlanPrompt(prompt) => WorkerCommand::SubmitPlanPrompt {
                prompt,
                reasoning_effort: self.runtime.reasoning_effort.clone(),
            },
            AppAction::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            } => WorkerCommand::ApprovePlan {
                plan_text,
                permission,
                scope_summary,
                clear_planning_context,
            },
            AppAction::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                start_mode,
                permission_grant,
            } => WorkerCommand::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                start_mode,
                permission_grant,
            },
            AppAction::RejectPlan {
                plan_id,
                expected_plan_hash,
            } => WorkerCommand::RejectPlan {
                plan_id,
                expected_plan_hash,
            },
            AppAction::InvokeInlineSkill {
                skill_id,
                arguments,
            } => WorkerCommand::InvokeInlineSkill {
                skill_id,
                arguments,
                reasoning_effort: self.runtime.reasoning_effort.clone(),
            },
            AppAction::InvokeChildSessionSkill {
                skill_id,
                arguments,
            } => WorkerCommand::InvokeChildSessionSkill {
                skill_id,
                arguments,
            },
            AppAction::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            } => WorkerCommand::InvokeAgentProfile {
                profile_id,
                prompt,
                parent_prompt,
            },
            AppAction::SubmitTask(prompt) => WorkerCommand::SubmitTask { prompt },
            AppAction::ContinueTask { task_id, guidance } => {
                WorkerCommand::ContinueTask { task_id, guidance }
            }
            AppAction::ApprovalDecision { call_id, approved } => {
                self.approval_worker_command(WorkerApprovalCommand::Decision { call_id, approved })
            }
            AppAction::ApprovalSessionDecision { call_id } => {
                self.approval_worker_command(WorkerApprovalCommand::DecisionForSession { call_id })
            }
            AppAction::ApprovalDecisionWithArgs { call_id, args_json } => self
                .approval_worker_command(WorkerApprovalCommand::DecisionWithArgs {
                    call_id,
                    args_json,
                }),
            AppAction::BackgroundActiveAgent => WorkerCommand::BackgroundActiveAgent,
            AppAction::CancelRun => WorkerCommand::CancelRun,
            AppAction::CancelTerminalTask { task_id } => {
                WorkerCommand::CancelTerminalTask { task_id }
            }
            AppAction::CloseAgent { thread_id, reason } => {
                WorkerCommand::CloseAgent { thread_id, reason }
            }
            AppAction::CancelAgent { thread_id, reason } => {
                WorkerCommand::CancelAgent { thread_id, reason }
            }
            AppAction::MessageAgent { thread_id, prompt } => {
                WorkerCommand::MessageAgent { thread_id, prompt }
            }
            AppAction::CompactNow => WorkerCommand::CompactNow,
            AppAction::CheckChangedFilesDiagnostics => WorkerCommand::CheckChangedFilesDiagnostics,
            AppAction::CleanMutationArtifacts { target } => {
                WorkerCommand::CleanMutationArtifacts { target }
            }
            AppAction::DeleteMutationArtifact { artifact_id } => {
                WorkerCommand::DeleteMutationArtifact { artifact_id }
            }
            AppAction::ApproveVerificationCheck { check_spec_id } => {
                WorkerCommand::ApproveVerificationCheck { check_spec_id }
            }
            AppAction::SandboxVerificationCheck { check_spec_id } => {
                WorkerCommand::SandboxVerificationCheck { check_spec_id }
            }
            AppAction::ActivateLazyMcp { server_name } => {
                WorkerCommand::ActivateLazyMcp { server_name }
            }
            AppAction::RefreshMcpServer { server_name } => {
                WorkerCommand::RefreshMcpServer { server_name }
            }
            AppAction::StartNewSession { session_log_path } => {
                WorkerCommand::StartNewSession { session_log_path }
            }
            AppAction::SwitchSession { session_log_path } => {
                WorkerCommand::SwitchSession { session_log_path }
            }
            AppAction::SetupCompleted { .. }
            | AppAction::TrustWorkspace
            | AppAction::ConfigSaved { .. }
            | AppAction::RuntimeConfigUpdated { .. }
            | AppAction::CopyToClipboard { .. } => unreachable!(
                "setup/config/runtime updates are handled before worker command conversion"
            ),
        }
    }

    fn approval_worker_command(&self, payload: WorkerApprovalCommand) -> WorkerCommand {
        let session_id = self.session_log_path.display().to_string();
        let command_id = stable_approval_command_id(&session_id, &payload);
        WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
            command_id,
            "sigil-tui",
            session_id,
            payload,
        ))
    }
}

fn stable_approval_command_id(session_id: &str, payload: &WorkerApprovalCommand) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sigil-tui-approval-command-v1\0");
    hasher.update(session_id.as_bytes());
    hasher.update(b"\0");
    match payload {
        WorkerApprovalCommand::Decision { call_id, approved } => {
            hasher.update(b"decision\0");
            hasher.update(call_id.as_bytes());
            hasher.update(b"\0");
            let decision_label: &[u8] = if *approved { b"approve" } else { b"deny" };
            hasher.update(decision_label);
        }
        WorkerApprovalCommand::DecisionForSession { call_id } => {
            hasher.update(b"decision_for_session\0");
            hasher.update(call_id.as_bytes());
        }
        WorkerApprovalCommand::DecisionWithArgs { call_id, args_json } => {
            hasher.update(b"decision_with_args\0");
            hasher.update(call_id.as_bytes());
            hasher.update(b"\0");
            hasher.update(args_json.as_bytes());
        }
    }
    let digest = hasher.finalize();
    let short_hash = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("tui-approval-{short_hash}")
}
