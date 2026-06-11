use anyhow::Result;

use crate::{ToolCall, ToolSpec};

/// Decision returned by an approval policy for one tool call.
#[derive(Debug, Clone)]
pub enum ToolApproval {
    /// Allow the tool call to execute.
    Approve,
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
}

/// Approval policy that unconditionally approves every tool call.
pub struct AutoApproveHandler;

impl ApprovalHandler for AutoApproveHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Approve)
    }
}
