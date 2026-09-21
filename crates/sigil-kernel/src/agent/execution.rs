//! Execution facts shared by child lifecycle and Task settlement projections.

use crate::{AgentRunOutcome, AgentRunTerminalReason, ToolError, ToolErrorKind};

/// Agent execution disposition, before goal delivery and verification are evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionDisposition {
    Completed,
    Blocked,
    Interrupted,
    Cancelled,
    Failed,
}

impl AgentRunOutcome {
    /// Reduces terminal facts once. Historical denied tools do not defeat a later accepted
    /// answer; unresolved recovery/effect blockers retain their owner's active state.
    #[must_use]
    pub fn execution_disposition(&self, final_text: &str) -> ExecutionDisposition {
        if self.terminal_reason == AgentRunTerminalReason::MaxTurns
            || !self.interrupted_tool_calls.is_empty()
        {
            return ExecutionDisposition::Interrupted;
        }
        let has_answer = !final_text.trim().is_empty();
        if self.terminal_reason.blocks_successful_completion()
            || self.tool_errors.iter().any(recovery_tool_error_is_active)
            || (!has_answer
                && (self.approval_denials > 0
                    || self.tool_errors.iter().any(|error| {
                        matches!(
                            error.kind,
                            ToolErrorKind::ApprovalRequired
                                | ToolErrorKind::ApprovalDenied
                                | ToolErrorKind::PermissionDenied
                                | ToolErrorKind::PathOutsideWorkspace
                                | ToolErrorKind::ExternalDirectoryRequired
                        )
                    })))
        {
            ExecutionDisposition::Blocked
        } else if !has_answer && !self.tool_errors.is_empty() {
            ExecutionDisposition::Failed
        } else if !has_answer {
            ExecutionDisposition::Blocked
        } else {
            ExecutionDisposition::Completed
        }
    }
}

/// Reads a recovery blocker's explicit current state; an unauthenticated missing state remains
/// unresolved. A final answer cannot clear this fact.
#[must_use]
pub fn recovery_tool_error_is_active(error: &ToolError) -> bool {
    error.kind.is_recovery_blocker()
        && error
            .details
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
}

/// Classifies typed execution failures without interpreting their display text.
#[must_use]
pub fn execution_failure_disposition(error: &anyhow::Error) -> ExecutionDisposition {
    if let Some(recovery) = error.downcast_ref::<crate::ProviderTurnRecoveryTerminalError>() {
        return match recovery.disposition {
            crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked
            | crate::ProviderTurnRecoveryTerminalDispositionV1::Paused => {
                ExecutionDisposition::Blocked
            }
            crate::ProviderTurnRecoveryTerminalDispositionV1::Cancelled => {
                ExecutionDisposition::Cancelled
            }
            crate::ProviderTurnRecoveryTerminalDispositionV1::Irrecoverable => {
                ExecutionDisposition::Failed
            }
        };
    }
    if error
        .downcast_ref::<crate::ProviderProtocolViolation>()
        .is_some()
        || error
            .downcast_ref::<crate::verification::UnresolvedVerificationRequirement>()
            .is_some()
    {
        ExecutionDisposition::Blocked
    } else {
        ExecutionDisposition::Failed
    }
}
