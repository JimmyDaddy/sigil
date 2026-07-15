use serde::{Deserialize, Serialize};
use sigil_kernel::PublicRunEvent;

/// Current version of the provider-neutral machine record envelope.
pub const MACHINE_PROTOCOL_VERSION: u16 = 1;

/// One machine-readable record emitted by an automation adapter.
///
/// JSON output emits exactly one terminal record. JSONL output emits zero or more event records
/// followed by exactly one terminal result or error record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
pub enum MachineRecord {
    /// One ordered public run event.
    Event {
        /// Machine envelope version.
        protocol_version: u16,
        /// Stable provider-neutral run event.
        event: Box<PublicRunEvent>,
    },
    /// The terminal run result.
    Result {
        /// Machine envelope version.
        protocol_version: u16,
        /// Terminal result payload.
        result: MachineRunResult,
    },
    /// A structured failure produced before a terminal run result is available.
    Error {
        /// Machine envelope version.
        protocol_version: u16,
        /// Safe error payload.
        error: MachineError,
    },
}

impl MachineRecord {
    /// Wraps one public run event in the current machine envelope.
    #[must_use]
    pub fn event(event: PublicRunEvent) -> Self {
        Self::Event {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            event: Box::new(event),
        }
    }

    /// Wraps one terminal result in the current machine envelope.
    #[must_use]
    pub fn result(result: MachineRunResult) -> Self {
        Self::Result {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            result,
        }
    }

    /// Wraps one structured error in the current machine envelope.
    #[must_use]
    pub fn error(error: MachineError) -> Self {
        Self::Error {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            error,
        }
    }
}

/// Terminal status for one machine-readable run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineRunStatus {
    /// The run produced a successful terminal result.
    Succeeded,
    /// The run reached a terminal execution failure.
    Failed,
    /// Cooperative cancellation reached the run boundary.
    Cancelled,
}

/// Stable terminal result returned by JSON and JSONL output modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MachineRunResult {
    /// Durable session scope used by the application run.
    pub session_id: String,
    /// Adapter-owned run identifier.
    pub run_id: String,
    /// Terminal run status.
    pub status: MachineRunStatus,
    /// Final assistant text, empty when no final answer was produced.
    pub final_text: String,
    /// Durable V2 JSONL session log path.
    pub session_log_path: String,
}

/// Stable error classes exposed to machine consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineErrorCode {
    /// Command invocation was invalid.
    InvalidInvocation,
    /// Configuration could not be loaded or validated.
    ConfigurationInvalid,
    /// Provider, tool, or runtime execution failed.
    ExecutionFailed,
    /// The run was cooperatively cancelled before a result could be produced.
    Cancelled,
    /// The failure could not be classified more narrowly without guessing.
    Internal,
}

/// Safe structured error returned to machine consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MachineError {
    /// Stable routing code.
    pub code: MachineErrorCode,
    /// User-safe diagnostic message.
    pub message: String,
    /// Whether retrying the same invocation may be useful.
    pub retryable: bool,
}

impl MachineError {
    /// Creates a structured machine error from an already-sanitized message.
    #[must_use]
    pub fn new(code: MachineErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

/// Stable process exit codes for machine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum MachineExitCode {
    /// Run succeeded.
    Success = 0,
    /// Runtime, provider, or tool execution failed.
    ExecutionFailed = 1,
    /// Invocation or configuration was invalid.
    InvalidInput = 2,
    /// Cooperative cancellation reached the process boundary.
    Cancelled = 130,
}

impl MachineExitCode {
    /// Returns the numeric process exit code.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Maps one terminal run status to its process exit code.
    #[must_use]
    pub const fn for_status(status: MachineRunStatus) -> Self {
        match status {
            MachineRunStatus::Succeeded => Self::Success,
            MachineRunStatus::Failed => Self::ExecutionFailed,
            MachineRunStatus::Cancelled => Self::Cancelled,
        }
    }

    /// Maps a structured pre-result error to its process exit code.
    #[must_use]
    pub const fn for_error(code: MachineErrorCode) -> Self {
        match code {
            MachineErrorCode::InvalidInvocation | MachineErrorCode::ConfigurationInvalid => {
                Self::InvalidInput
            }
            MachineErrorCode::Cancelled => Self::Cancelled,
            MachineErrorCode::ExecutionFailed | MachineErrorCode::Internal => Self::ExecutionFailed,
        }
    }
}

#[cfg(test)]
#[path = "tests/machine_protocol_tests.rs"]
mod tests;
