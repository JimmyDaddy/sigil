use std::time::Duration;

use sigil_kernel::SessionRef;
use thiserror::Error as ThisError;

use crate::dto::{
    HttpApprovalDecisionRecord, HttpRunSnapshot, HttpSessionBinding, HttpSessionSnapshot,
};

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
    /// Optional user-facing reason persisted by the runtime cancellation control plane.
    pub reason: Option<String>,
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
    /// Creates or resolves the durable session binding for one adapter session.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime cannot establish a durable V2 session scope and path.
    fn bind_session(&self, session_id: &str) -> Result<HttpSessionBinding, HttpRunDriverError>;

    /// Resolves an existing durable session after the registry validates its wire identity.
    ///
    /// Synthetic drivers that do not model historical sessions reject this operation by default.
    ///
    /// # Errors
    ///
    /// Returns a bounded error direction when current workspace truth cannot authorize the reopen.
    fn bind_existing_session(
        &self,
        _session_ref: &SessionRef,
        _expected_session_id: &str,
    ) -> Result<HttpSessionBinding, HttpSessionOpenBindingError> {
        Err(HttpSessionOpenBindingError::Unavailable)
    }

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

    /// Waits until every driver-owned run supervisor has completed cleanup.
    ///
    /// Synthetic drivers own no background execution by default. Production drivers override this
    /// hook so a successful listener shutdown cannot leave an unowned run task behind.
    ///
    /// # Errors
    ///
    /// Returns an error when owned work does not drain before `timeout`.
    fn wait_for_idle(&self, _timeout: Duration) -> Result<(), HttpRunDriverError> {
        Ok(())
    }
}

/// Bounded, path-free failure direction returned while reopening an existing durable session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
pub enum HttpSessionOpenBindingError {
    /// The requested direct-child source is absent from current workspace truth.
    #[error("durable session was not found")]
    NotFound,
    /// The source exists but is not a ready, supported V2 stream.
    #[error("durable session is not ready")]
    NotReady,
    /// The source identity no longer matches the catalog candidate selected by the client.
    #[error("durable session identity changed")]
    IdentityChanged,
    /// Current bounded lifecycle or durable stream validation could not complete.
    #[error("durable session is unavailable")]
    Unavailable,
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
