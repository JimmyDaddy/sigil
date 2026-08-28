use std::sync::Arc;

use ratatui::{buffer::Buffer, layout::Rect};
use thiserror::Error;

use crate::ui::LayoutSnapshot;

/// The monotonically increasing identity of a frame prepared for one terminal epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FrameGeneration(u64);

impl FrameGeneration {
    const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// A terminal lifecycle generation. A poisoned terminal can never reuse its old epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TerminalEpoch(u64);

impl TerminalEpoch {
    const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PresentAttemptId(u64);

/// The only frame facts that may be used by presentation-bound input.
///
/// The layout snapshot is produced inside the same `Terminal::draw` callback as the cell
/// surface. It is intentionally retained behind `Arc` so event handling cannot rebuild it from
/// the mutable `AppState`.
#[derive(Debug, Clone)]
pub(crate) struct PresentedFrame {
    surface: Buffer,
    layout: Arc<LayoutSnapshot>,
}

impl PresentedFrame {
    fn new(_area: Rect, surface: Buffer, layout: Arc<LayoutSnapshot>) -> Self {
        Self { surface, layout }
    }

    #[cfg(test)]
    pub(crate) fn area(&self) -> Rect {
        self.surface.area
    }

    #[allow(dead_code)]
    pub(crate) fn surface(&self) -> &Buffer {
        &self.surface
    }
}

/// Immutable interaction facts paired with one successfully presented frame.
#[derive(Debug, Clone)]
pub(crate) struct InteractionSnapshot {
    generation: FrameGeneration,
    terminal_epoch: TerminalEpoch,
    layout: Arc<LayoutSnapshot>,
}

impl InteractionSnapshot {
    fn new(
        generation: FrameGeneration,
        terminal_epoch: TerminalEpoch,
        layout: Arc<LayoutSnapshot>,
    ) -> Self {
        Self {
            generation,
            terminal_epoch,
            layout,
        }
    }

    pub(crate) fn generation(&self) -> FrameGeneration {
        self.generation
    }

    #[allow(dead_code)]
    pub(crate) fn terminal_epoch(&self) -> TerminalEpoch {
        self.terminal_epoch
    }

    pub(crate) fn layout(&self) -> &LayoutSnapshot {
        &self.layout
    }
}

/// The atomic unit consumed by presentation-bound input.
#[derive(Debug, Clone)]
pub(crate) struct CommittedPresentation {
    #[allow(dead_code)]
    frame: Arc<PresentedFrame>,
    interaction: Arc<InteractionSnapshot>,
}

impl CommittedPresentation {
    fn new(
        generation: FrameGeneration,
        terminal_epoch: TerminalEpoch,
        frame: PresentedFrame,
    ) -> Self {
        let interaction = Arc::new(InteractionSnapshot::new(
            generation,
            terminal_epoch,
            Arc::clone(&frame.layout),
        ));
        Self {
            frame: Arc::new(frame),
            interaction,
        }
    }

    pub(crate) fn generation(&self) -> FrameGeneration {
        self.interaction.generation()
    }

    #[allow(dead_code)]
    pub(crate) fn terminal_epoch(&self) -> TerminalEpoch {
        self.interaction.terminal_epoch()
    }

    #[cfg(test)]
    pub(crate) fn frame(&self) -> &PresentedFrame {
        &self.frame
    }

    #[allow(dead_code)]
    pub(crate) fn interaction(&self) -> &InteractionSnapshot {
        &self.interaction
    }

    /// Returns the immutable hit/text geometry paired with this frame.
    pub(crate) fn layout(&self) -> &LayoutSnapshot {
        self.interaction.layout()
    }
}

/// Evidence that no backend draw, cursor mutation, buffer swap, write, enqueue or flush started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PresentNotStarted {
    attempt: PresentAttemptId,
}

impl PresentNotStarted {
    #[allow(dead_code)]
    pub(crate) fn attempt(self) -> u64 {
        self.attempt.0
    }
}

/// Typed evidence that the terminal may contain a partial effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresentFault {
    attempt: PresentAttemptId,
    reason: String,
}

impl PresentFault {
    fn new(attempt: PresentAttemptId, reason: impl Into<String>) -> Self {
        Self {
            attempt,
            reason: reason.into(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn attempt(&self) -> u64 {
        self.attempt.0
    }

    #[allow(dead_code)]
    pub(crate) fn reason(&self) -> &str {
        &self.reason
    }
}

/// Private proof that the backend draw and flush completed.
///
/// The fields and constructor remain private so callers cannot manufacture a successful
/// presentation from a queued or merely prepared frame.
#[derive(Debug)]
pub(crate) struct TrustedPresentReceipt {
    attempt: PresentAttemptId,
    generation: FrameGeneration,
    terminal_epoch: TerminalEpoch,
}

impl TrustedPresentReceipt {
    fn new(
        attempt: PresentAttemptId,
        generation: FrameGeneration,
        terminal_epoch: TerminalEpoch,
    ) -> Self {
        Self {
            attempt,
            generation,
            terminal_epoch,
        }
    }
}

pub(crate) enum PresentOutcome {
    Presented(TrustedPresentReceipt),
    #[allow(dead_code)]
    NotStarted(PresentNotStarted),
    IndeterminateAfterIo(PresentFault),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PresentationSessionState {
    AwaitingPresent {
        generation: FrameGeneration,
        has_current: bool,
        interaction_barrier: bool,
    },
    Presenting {
        generation: FrameGeneration,
        attempt: u64,
        has_current: bool,
    },
    Synchronized {
        generation: FrameGeneration,
    },
    Poisoned,
    Closed,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum PresentationError {
    #[error("presentation session is already preparing a frame")]
    AlreadyPreparing,
    #[error("presentation session is poisoned and accepts only owner recovery or teardown")]
    Poisoned,
    #[error("presentation session is closed")]
    Closed,
    #[error("presentation outcome does not match the active frame attempt")]
    AttemptMismatch,
    #[error("presented outcome is missing the prepared frame")]
    MissingPreparedFrame,
    #[error("trusted present receipt does not match the active frame attempt")]
    ReceiptMismatch,
}

enum SessionState {
    Empty,
    AwaitingPresent {
        current: Option<Arc<CommittedPresentation>>,
        generation: FrameGeneration,
        interaction_barrier: bool,
    },
    Presenting {
        current: Option<Arc<CommittedPresentation>>,
        generation: FrameGeneration,
        attempt: PresentAttemptId,
        interaction_barrier: bool,
    },
    Synchronized(Arc<CommittedPresentation>),
    Poisoned(PresentFault),
    Closed,
}

pub(crate) struct PresentationSession {
    next_generation: u64,
    next_attempt: u64,
    terminal_epoch: TerminalEpoch,
    state: SessionState,
}

impl std::fmt::Debug for PresentationSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PresentationSession")
            .field("next_generation", &self.next_generation)
            .field("next_attempt", &self.next_attempt)
            .field("terminal_epoch", &self.terminal_epoch)
            .field("state", &self.state_tag())
            .finish()
    }
}

impl PresentationSession {
    pub(crate) fn new() -> Self {
        Self {
            next_generation: 1,
            next_attempt: 1,
            terminal_epoch: TerminalEpoch::new(1),
            state: SessionState::Empty,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn terminal_epoch(&self) -> TerminalEpoch {
        self.terminal_epoch
    }

    pub(crate) fn current(&self) -> Option<&Arc<CommittedPresentation>> {
        match &self.state {
            SessionState::Empty => None,
            SessionState::Synchronized(current) => Some(current),
            SessionState::AwaitingPresent { .. } | SessionState::Presenting { .. } => None,
            SessionState::Poisoned(_) | SessionState::Closed => None,
        }
    }

    pub(crate) fn state(&self) -> PresentationSessionState {
        match &self.state {
            SessionState::Empty => PresentationSessionState::AwaitingPresent {
                generation: FrameGeneration::new(0),
                has_current: false,
                interaction_barrier: true,
            },
            SessionState::AwaitingPresent {
                generation,
                current,
                interaction_barrier,
            } => PresentationSessionState::AwaitingPresent {
                generation: *generation,
                has_current: current.is_some(),
                interaction_barrier: *interaction_barrier,
            },
            SessionState::Presenting {
                generation,
                attempt,
                current,
                ..
            } => PresentationSessionState::Presenting {
                generation: *generation,
                attempt: attempt.0,
                has_current: current.is_some(),
            },
            SessionState::Synchronized(current) => PresentationSessionState::Synchronized {
                generation: current.generation(),
            },
            SessionState::Poisoned(_) => PresentationSessionState::Poisoned,
            SessionState::Closed => PresentationSessionState::Closed,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn fault(&self) -> Option<&PresentFault> {
        match &self.state {
            SessionState::Poisoned(fault) => Some(fault),
            SessionState::Empty
            | SessionState::AwaitingPresent { .. }
            | SessionState::Presenting { .. }
            | SessionState::Synchronized(_)
            | SessionState::Closed => None,
        }
    }

    pub(crate) fn begin_prepare(&mut self) -> Result<FrameGeneration, PresentationError> {
        let current = match &self.state {
            SessionState::Empty => None,
            SessionState::Synchronized(current) => Some(Arc::clone(current)),
            SessionState::AwaitingPresent { .. } | SessionState::Presenting { .. } => {
                return Err(PresentationError::AlreadyPreparing);
            }
            SessionState::Poisoned(_) => return Err(PresentationError::Poisoned),
            SessionState::Closed => return Err(PresentationError::Closed),
        };
        let generation = FrameGeneration::new(self.next_generation);
        self.next_generation = self.next_generation.saturating_add(1);
        let interaction_barrier = current.is_none();
        self.state = SessionState::AwaitingPresent {
            current,
            generation,
            interaction_barrier,
        };
        Ok(generation)
    }

    pub(crate) fn begin_present(
        &mut self,
        generation: FrameGeneration,
    ) -> Result<PresentAttemptId, PresentationError> {
        let SessionState::AwaitingPresent {
            current,
            generation: expected_generation,
            interaction_barrier,
        } = std::mem::replace(&mut self.state, SessionState::Closed)
        else {
            return Err(PresentationError::AttemptMismatch);
        };
        if expected_generation != generation {
            self.state = SessionState::AwaitingPresent {
                current,
                generation: expected_generation,
                interaction_barrier,
            };
            return Err(PresentationError::AttemptMismatch);
        }
        let attempt = PresentAttemptId(self.next_attempt);
        self.next_attempt = self.next_attempt.saturating_add(1);
        self.state = SessionState::Presenting {
            current,
            generation,
            attempt,
            interaction_barrier,
        };
        Ok(attempt)
    }

    pub(crate) fn finish_present(
        &mut self,
        generation: FrameGeneration,
        frame: Option<PresentedFrame>,
        outcome: PresentOutcome,
    ) -> Result<(), PresentationError> {
        let SessionState::Presenting {
            current,
            generation: expected_generation,
            attempt,
            interaction_barrier,
        } = std::mem::replace(&mut self.state, SessionState::Closed)
        else {
            return Err(PresentationError::AttemptMismatch);
        };
        if expected_generation != generation {
            self.state = SessionState::Presenting {
                current,
                generation: expected_generation,
                attempt,
                interaction_barrier,
            };
            return Err(PresentationError::AttemptMismatch);
        }

        match outcome {
            PresentOutcome::Presented(receipt) => {
                if receipt.attempt != attempt
                    || receipt.generation != generation
                    || receipt.terminal_epoch != self.terminal_epoch
                {
                    self.state = SessionState::Poisoned(PresentFault::new(
                        attempt,
                        "trusted present receipt did not match the active terminal epoch",
                    ));
                    return Err(PresentationError::ReceiptMismatch);
                }
                let Some(frame) = frame else {
                    self.state = SessionState::Poisoned(PresentFault::new(
                        attempt,
                        "presented outcome did not carry frame facts",
                    ));
                    return Err(PresentationError::MissingPreparedFrame);
                };
                let committed = Arc::new(CommittedPresentation::new(
                    generation,
                    self.terminal_epoch,
                    frame,
                ));
                self.state = SessionState::Synchronized(committed);
            }
            PresentOutcome::NotStarted(not_started) => {
                if not_started.attempt != attempt {
                    self.state = SessionState::Poisoned(PresentFault::new(
                        attempt,
                        "not-started proof did not match the active present attempt",
                    ));
                    return Err(PresentationError::AttemptMismatch);
                }
                if interaction_barrier {
                    self.state = match current {
                        Some(current) => SessionState::Synchronized(current),
                        None => SessionState::Empty,
                    };
                } else if let Some(current) = current {
                    self.state = SessionState::Synchronized(current);
                } else {
                    // There is no committed frame to retain. Leave the session retryable;
                    // keeping an empty AwaitingPresent state would turn a backend no-op into a
                    // permanent AlreadyPreparing error.
                    self.state = SessionState::Empty;
                }
            }
            PresentOutcome::IndeterminateAfterIo(fault) => {
                self.state = SessionState::Poisoned(fault);
            }
        }
        Ok(())
    }

    pub(crate) fn finish_draw(
        &mut self,
        generation: FrameGeneration,
        attempt: PresentAttemptId,
        area: Rect,
        surface: Buffer,
        layout: Arc<LayoutSnapshot>,
    ) -> Result<(), PresentationError> {
        self.finish_present(
            generation,
            Some(PresentedFrame::new(area, surface, layout)),
            PresentOutcome::Presented(TrustedPresentReceipt::new(
                attempt,
                generation,
                self.terminal_epoch,
            )),
        )
    }

    pub(crate) fn fail_after_io(
        &mut self,
        generation: FrameGeneration,
        attempt: PresentAttemptId,
        reason: impl Into<String>,
    ) -> Result<(), PresentationError> {
        self.finish_present(
            generation,
            None,
            PresentOutcome::IndeterminateAfterIo(PresentFault::new(attempt, reason)),
        )
    }

    /// The owner may call this only after the terminal has been restored and a full repaint is
    /// ready to start. The old committed frame is deliberately discarded.
    #[allow(dead_code)]
    pub(crate) fn begin_new_epoch(&mut self) -> Result<TerminalEpoch, PresentationError> {
        if matches!(self.state, SessionState::Closed) {
            return Err(PresentationError::Closed);
        }
        if !matches!(self.state, SessionState::Poisoned(_)) {
            return Err(PresentationError::AttemptMismatch);
        }
        self.terminal_epoch = TerminalEpoch::new(self.terminal_epoch.0.saturating_add(1));
        self.state = SessionState::Empty;
        Ok(self.terminal_epoch)
    }

    #[allow(dead_code)]
    pub(crate) fn close(&mut self) {
        self.state = SessionState::Closed;
    }

    fn state_tag(&self) -> PresentationSessionState {
        self.state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> PresentedFrame {
        let layout = Arc::new(crate::ui::LayoutSnapshot::empty(Rect::new(0, 0, 12, 8)));
        PresentedFrame::new(
            Rect::new(0, 0, 12, 8),
            Buffer::empty(Rect::new(0, 0, 12, 8)),
            layout,
        )
    }

    #[test]
    fn not_started_keeps_the_last_committed_frame() {
        let mut session = PresentationSession::new();
        let generation = session.begin_prepare().expect("first prepare");
        let attempt = session.begin_present(generation).expect("first attempt");
        session
            .finish_present(
                generation,
                Some(frame()),
                PresentOutcome::Presented(TrustedPresentReceipt::new(
                    attempt,
                    generation,
                    session.terminal_epoch,
                )),
            )
            .expect("first present");
        let committed = session.current().cloned().expect("committed frame");

        let next_generation = session.begin_prepare().expect("second prepare");
        let next_attempt = session
            .begin_present(next_generation)
            .expect("second attempt");
        session
            .finish_present(
                next_generation,
                None,
                PresentOutcome::NotStarted(PresentNotStarted {
                    attempt: next_attempt,
                }),
            )
            .expect("not-started outcome");

        assert_eq!(
            session.current().expect("old frame").generation(),
            committed.generation()
        );
        assert!(matches!(
            session.state(),
            PresentationSessionState::Synchronized { .. }
        ));
    }

    #[test]
    fn not_started_before_first_frame_leaves_the_session_retryable() {
        let mut session = PresentationSession::new();
        let generation = session.begin_prepare().expect("first prepare");
        let attempt = session.begin_present(generation).expect("first attempt");
        session
            .finish_present(
                generation,
                None,
                PresentOutcome::NotStarted(PresentNotStarted { attempt }),
            )
            .expect("not-started outcome");

        assert!(session.current().is_none());
        assert!(matches!(
            session.state(),
            PresentationSessionState::AwaitingPresent {
                generation: FrameGeneration(0),
                has_current: false,
                interaction_barrier: true,
            }
        ));
        session.begin_prepare().expect("retry after no-op present");
    }

    #[test]
    fn committed_layout_survives_a_pending_next_present() {
        let mut session = PresentationSession::new();
        let generation = session.begin_prepare().expect("first prepare");
        let attempt = session.begin_present(generation).expect("first attempt");
        session
            .finish_present(
                generation,
                Some(frame()),
                PresentOutcome::Presented(TrustedPresentReceipt::new(
                    attempt,
                    generation,
                    session.terminal_epoch,
                )),
            )
            .expect("first present");
        let committed = session.current().cloned().expect("committed frame");
        let next_generation = session.begin_prepare().expect("second prepare");

        assert!(session.current().is_none());
        assert_eq!(committed.layout().screen, Rect::new(0, 0, 12, 8));
        assert_eq!(committed.frame().area(), Rect::new(0, 0, 12, 8));

        let next_attempt = session
            .begin_present(next_generation)
            .expect("second attempt");
        session
            .finish_present(
                next_generation,
                None,
                PresentOutcome::NotStarted(PresentNotStarted {
                    attempt: next_attempt,
                }),
            )
            .expect("second no-op present");
        assert_eq!(
            session
                .current()
                .expect("previous frame restored")
                .generation(),
            committed.generation()
        );
    }

    #[test]
    fn indeterminate_present_poisoned_session_rejects_new_frames_and_input() {
        let mut session = PresentationSession::new();
        let generation = session.begin_prepare().expect("prepare");
        let attempt = session.begin_present(generation).expect("attempt");
        session
            .finish_present(
                generation,
                None,
                PresentOutcome::IndeterminateAfterIo(PresentFault::new(attempt, "partial write")),
            )
            .expect("fault should be recorded");

        assert!(session.current().is_none());
        assert_eq!(session.state(), PresentationSessionState::Poisoned);
        assert_eq!(session.begin_prepare(), Err(PresentationError::Poisoned));
        let epoch = session.begin_new_epoch().expect("owner recovery");
        assert_eq!(epoch.value(), 2);
        assert!(session.current().is_none());
    }
}
