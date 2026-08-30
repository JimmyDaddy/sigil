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
