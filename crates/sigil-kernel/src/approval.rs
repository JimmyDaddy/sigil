use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{ToolCall, ToolSpec};

/// Sentinel carried by the V2 wire shape for approvals that remain pending until an explicit
/// decision, cancellation, route failure, or run/session shutdown. This is JavaScript's maximum
/// safe integer so Desktop can round-trip the exact request identity without precision loss.
pub const APPROVAL_REQUEST_NO_EXPIRY_MS: u64 = 9_007_199_254_740_991;

/// Exact V2 identity shared by kernel, route adapters, durable audit and clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApprovalRequestIdentityV2 {
    pub session_id: String,
    pub run_id: String,
    pub call_id: String,
    pub approval_request_id: String,
    pub plan_hash: String,
    pub policy_version: String,
    pub execution_binding_hash: String,
    pub expires_at_ms: u64,
}

/// Exact, secret-free permission identity attached to an interactive approval request.
///
/// The signature binds the raw tool-call digest, policy snapshot, subjects, risk and preview
/// identity without exposing any of those potentially sensitive values to routing adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolApprovalContext {
    pub identity: ApprovalRequestIdentityV2,
    pub permission_signature: String,
    pub policy_fingerprint: String,
    pub requested_at_ms: u64,
    pub expires_at_ms: u64,
}

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
    /// A legacy or externally bounded approval request expired before a user decision was accepted.
    Expired { reason: String },
    /// The approval route was cancelled before a user decision was accepted.
    Cancelled { reason: String },
    /// The response targeted an approval request that is no longer current.
    Stale { reason: String },
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

    /// Returns whether this exact approval route should emit an interactive presenter event.
    ///
    /// Parallel coordinators may return `false` only after atomically attaching the call to an
    /// already-presented exact aggregation group. The subsequent contextual approval call must
    /// still resolve independently for this route.
    fn should_present_tool_approval(
        &mut self,
        _call: &ToolCall,
        _spec: &ToolSpec,
        _context: &ToolApprovalContext,
    ) -> Result<bool> {
        Ok(true)
    }

    /// Releases any exact aggregation claim when the presenter cannot surface the request.
    fn tool_approval_presentation_failed(
        &mut self,
        _call: &ToolCall,
        _spec: &ToolSpec,
        _context: &ToolApprovalContext,
        _reason: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Resolves one tool call while preserving its exact permission identity for route adapters.
    ///
    /// Existing handlers remain source compatible through the default implementation. Wrappers
    /// that persist or aggregate approval routes must override this method and forward `context`.
    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        spec: &ToolSpec,
        _context: &ToolApprovalContext,
    ) -> Result<ToolApproval> {
        self.approve_tool_call(call, spec)
    }

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
