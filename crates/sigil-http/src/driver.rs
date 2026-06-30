use thiserror::Error as ThisError;

use crate::dto::{HttpApprovalDecisionRecord, HttpRunSnapshot, HttpSessionSnapshot};

/// Start context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverStart {
    /// Session snapshot at the moment the run was registered.
    pub session: HttpSessionSnapshot,
    /// Run snapshot in `starting` state.
    pub run: HttpRunSnapshot,
    /// Full prompt body. The preview is carried separately on the run snapshot.
    pub prompt: String,
}

/// Cancel context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverCancel {
    /// Owning session id.
    pub session_id: String,
    /// Run id being canceled.
    pub run_id: String,
}

/// Approval context delivered to the HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRunDriverApproval {
    /// Owning session id.
    pub session_id: String,
    /// Run id receiving the decision.
    pub run_id: String,
    /// Tool call id receiving the decision.
    pub call_id: String,
    /// Decision record routed to the driver.
    pub decision: HttpApprovalDecisionRecord,
}

/// Driver interface used by the HTTP registry.
///
/// The registry owns IDs and routing state. The driver owns actual agent execution,
/// cancellation, and approval delivery so this crate does not duplicate the agent loop.
pub trait HttpRunDriver: Send + Sync {
    /// Starts execution for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot accept the run.
    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError>;

    /// Requests cancellation for a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot route the cancellation.
    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError>;

    /// Routes a user approval decision to a registered run.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying runtime cannot route the approval decision.
    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError>;
}

/// Error returned by an HTTP run driver.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
#[error("{message}")]
pub struct HttpRunDriverError {
    /// Driver-provided error message.
    pub message: String,
}

impl HttpRunDriverError {
    /// Creates a driver error with context.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
