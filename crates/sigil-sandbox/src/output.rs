//! RFC-0071 section 8.4/18 R71.3: bounded output supervision contract.
//!
//! The supervisor performs a single OS drain with bounded fan-out: protocol/MCP subscribers
//! never lose frames; sustained backpressure produces a typed protocol/process failure; the UI
//! may use an independent lossy projection that never blocks the supervisor drain. Exactly one
//! terminal EOF per process; channel and truncated state are never guessed by consumers.

use std::time::Duration;

/// Closed output drain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputDrainStateV1 {
    Draining,
    Backpressured,
    TerminalEof,
    Failed(String),
}

/// Closed output supervision error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputSupervisionErrorV1 {
    #[error("sustained consumer backpressure exceeded the bounded deadline; protocol failure")]
    BackpressureTimeout,
    #[error("output exceeds the per-channel byte cap; capture is truncated truthfully")]
    ByteCapExceeded,
    #[error("supervisor drain lost a frame before terminal EOF")]
    FrameLoss,
    #[error("exactly one terminal EOF per process is required")]
    DuplicateEof,
}

/// Bounded per-channel output cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputByteCapV1 {
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub pty_bytes: u64,
}

impl Default for OutputByteCapV1 {
    fn default() -> Self {
        Self {
            stdout_bytes: 64 * 1024,
            stderr_bytes: 64 * 1024,
            pty_bytes: 128 * 1024,
        }
    }
}

/// Fan-out policy: exactly-one take from the single drain, lossy downstream allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFanOutPolicyV1 {
    SingleDrainExactOnce,
    LossyUiProjection,
}

/// Validates one output frame against the supervision contract (sequence monotonic,
/// exactly one terminal EOF).
pub fn validate_frame_sequence(
    previous_sequence: Option<u64>,
    next_sequence: u64,
    _eof: bool,
    seen_eof: bool,
) -> Result<(), OutputSupervisionErrorV1> {
    if seen_eof {
        return Err(OutputSupervisionErrorV1::DuplicateEof);
    }
    if previous_sequence.is_some_and(|previous| next_sequence != previous + 1) {
        return Err(OutputSupervisionErrorV1::FrameLoss);
    }
    Ok(())
}

/// Backpressure check: sustained pressure beyond the deadline is a typed failure, not a stall.
pub fn backpressure_allowed(
    timeout: Duration,
    sustained_ms: u64,
) -> Result<(), OutputSupervisionErrorV1> {
    let allowed_ms = timeout.as_millis() as u64;
    if sustained_ms > allowed_ms {
        return Err(OutputSupervisionErrorV1::BackpressureTimeout);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_output_sequence_must_be_monotonic_and_single_eof() {
        validate_frame_sequence(None, 1, false, false).expect("first");
        validate_frame_sequence(Some(1), 2, true, false).expect("second/eof");
        let error = validate_frame_sequence(Some(2), 3, false, true).expect_err("duplicate eof");
        assert!(matches!(error, OutputSupervisionErrorV1::DuplicateEof));
    }

    #[test]
    fn r71_output_frame_loss_is_detected() {
        let error = validate_frame_sequence(Some(1), 3, false, false).expect_err("gap");
        assert!(matches!(error, OutputSupervisionErrorV1::FrameLoss));
    }

    #[test]
    fn r71_backpressure_beyond_deadline_fails_typed() {
        let error = backpressure_allowed(Duration::from_millis(1000), 2000).expect_err("too long");
        assert!(matches!(
            error,
            OutputSupervisionErrorV1::BackpressureTimeout
        ));
        backpressure_allowed(Duration::from_millis(1000), 500).expect("within");
    }
}
