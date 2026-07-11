use anyhow::Result;

use crate::{ToolCall, ToolSpec};

/// Decision returned by an approval policy for one tool call.
#[derive(Debug, Clone)]
pub enum ToolApproval {
    /// Allow the tool call to execute.
    Approve,
    /// Allow the tool call and grant the same normalized approval scope for this session.
    ApproveForSession,
    /// Allow the tool call to execute with approved argument overrides.
    ///
    /// This is intended for UI-mediated safety transforms that preserve the requested tool but
    /// change execution mode, such as moving an agent invocation to the background before it runs.
    ApproveWithArgs { args_json: String },
    /// Deny the tool call and persist a user-facing reason.
    Deny { reason: String },
}

/// Approval policy used by the agent loop before executing mutating tools.
pub trait ApprovalHandler {
    /// Resolves one tool call approval decision.
    ///
    /// # Errors
    ///
    /// Returns an error when approval state cannot be produced, such as channel shutdown,
    /// UI failure, or policy backend failure.
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval>;

    /// Reports whether approvals returned by this handler represent an explicit user action.
    ///
    /// Automated and test handlers remain `false` by default. Interactive route adapters must
    /// opt in and should forward this property when wrapping another handler.
    fn approval_is_explicit_user_action(&self) -> bool {
        false
    }
}

/// Approval policy that unconditionally approves every tool call.
pub struct AutoApproveHandler;

impl ApprovalHandler for AutoApproveHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Approve)
    }
}
