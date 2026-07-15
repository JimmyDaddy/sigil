use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Result;
use sigil_kernel::{
    AgentRunOutcome, AgentRunOutput, AgentRunResult, AgentRunTerminalReason, ApprovalHandler,
    AutoApproveHandler, JsonlSessionStore, PublicRunEvent, PublicRunEventKind,
    RunCancellationOwner, RunCancellationTerminalOutcome, RunEvent, Session, ToolApproval,
    ToolCall, ToolSpec,
};

use super::{
    ApplicationRunControl, ApplicationRunEventHandler, ApplicationRunEventSequence,
    ApplicationRunInteraction, ApplicationRunTerminalStatus, ApplicationSessionLeaseManager,
    PublicApplicationEventBridge, application_run_input, application_terminal_projection,
    default_application_session_path, validate_execution_contract,
};

#[test]
fn session_lease_rejects_overlapping_foreground_runs_and_releases_on_drop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("sessions/session.jsonl");
    let manager = ApplicationSessionLeaseManager::new();

    let first = manager.acquire(&path)?;
    let error = manager
        .acquire(&path)
        .expect_err("same durable session must have one foreground run");
    assert!(error.to_string().contains("active foreground run"));

    drop(first);
    let reacquired = manager.acquire(&path)?;
    drop(reacquired);
    Ok(())
}

#[cfg(unix)]
#[test]
fn session_lease_collapses_symlink_aliases_to_one_canonical_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let real = temp.path().join("real-session.jsonl");
    let alias = temp.path().join("alias-session.jsonl");
    std::fs::File::create(&real)?;
    std::os::unix::fs::symlink(&real, &alias)?;
    let manager = ApplicationSessionLeaseManager::new();

    let first = manager.acquire(&real)?;
    let error = manager
        .acquire(&alias)
        .expect_err("symlink alias must resolve to the active durable session");
    assert!(error.to_string().contains("active foreground run"));
    drop(first);
    Ok(())
}

#[test]
fn default_session_path_and_repo_context_are_application_owned() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("README.md"), "Sigil application service")?;

    let path = default_application_session_path(&temp.path().join("sessions"));
    let input = application_run_input(temp.path(), "summarize README.md".to_owned());

    assert!(path.starts_with(temp.path().join("sessions")));
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("jsonl")
    );
    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "repo-file:README.md")
    );
    Ok(())
}

#[test]
fn interaction_contract_distinguishes_noninteractive_and_external_surfaces() {
    assert_eq!(
        ApplicationRunInteraction::NonInteractive.kernel_mode(),
        sigil_kernel::InteractionMode::Headless
    );
    assert_eq!(
        ApplicationRunInteraction::ExternallyInteractive.kernel_mode(),
        sigil_kernel::InteractionMode::Interactive
    );
}

struct ExplicitApprovalHandler;

impl ApprovalHandler for ExplicitApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Approve)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

#[test]
fn externally_interactive_runs_reject_automated_approval_handlers() {
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &AutoApproveHandler,
            true,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &ExplicitApprovalHandler,
            false,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &ExplicitApprovalHandler,
            true,
        )
        .is_ok()
    );
}

#[test]
fn public_event_bridge_sequences_lifecycle_and_kernel_events() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let mut recorder = Recorder::default();
    let events = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder);
    bridge.emit(PublicRunEventKind::RunStarted {
        prompt: "hello".to_owned(),
    })?;
    sigil_kernel::EventHandler::handle(&mut bridge, RunEvent::TextDelta("hi".to_owned()))?;
    bridge.emit(PublicRunEventKind::RunFinished {
        final_text: "hi".to_owned(),
    })?;
    drop(bridge);

    assert_eq!(
        recorder
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(matches!(
        recorder.0[0].event,
        PublicRunEventKind::RunStarted { .. }
    ));
    assert!(matches!(
        recorder.0[1].event,
        PublicRunEventKind::TextDelta { .. }
    ));
    assert!(matches!(
        recorder.0[2].event,
        PublicRunEventKind::RunFinished { .. }
    ));
    Ok(())
}

#[test]
fn public_event_sequence_seals_after_root_terminal() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let sequence = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut recorder = Recorder::default();
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    )?;
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunFailed {
            error: "interrupted".to_owned(),
        },
    )?;
    assert!(
        sequence
            .emit(
                &mut recorder,
                PublicRunEventKind::TextDelta {
                    text: "late".to_owned(),
                },
            )
            .is_err()
    );
    assert_eq!(recorder.0.len(), 2);
    Ok(())
}

#[test]
fn non_final_kernel_terminals_do_not_project_as_run_finished() {
    for (terminal_reason, expected_status) in [
        (
            AgentRunTerminalReason::MaxTurns,
            ApplicationRunTerminalStatus::Interrupted,
        ),
        (
            AgentRunTerminalReason::DelegationUnsatisfied,
            ApplicationRunTerminalStatus::Blocked,
        ),
    ] {
        let output = AgentRunOutput {
            result: AgentRunResult {
                final_text: String::new(),
                tool_calls: 0,
                final_message_id: None,
            },
            outcome: AgentRunOutcome {
                terminal_reason,
                ..AgentRunOutcome::default()
            },
        };
        let (status, event) = application_terminal_projection(&output);

        assert_eq!(status, expected_status);
        assert!(matches!(event, PublicRunEventKind::RunFailed { .. }));
    }
}

#[tokio::test]
async fn cancellation_control_persists_request_then_terminal_after_quiescence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(
            ApplicationSessionLeaseManager::new().acquire(&temp.path().join("session.jsonl"))?,
        ),
    };
    let unblocked = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&unblocked);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let ticket = control.request_cancellation("test cancel", None, move || {
        signal.store(true, Ordering::SeqCst);
    })?;
    assert!(unblocked.load(Ordering::SeqCst));
    assert!(control.handle().is_cancel_requested());
    drop(root_task_guard);

    let outcome = control
        .finalize_cancellation(ticket, true, &mut events)
        .await?;
    assert_eq!(outcome, RunCancellationTerminalOutcome::Cancelled);
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunCancelled)
    ));
    Ok(())
}

#[tokio::test]
async fn cancellation_audit_failure_still_unblocks_and_requires_failed_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    temp.close()?;
    let unblocked = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&unblocked);

    let error = control
        .request_cancellation("test audit failure", None, move || {
            signal.store(true, Ordering::SeqCst);
        })
        .expect_err("removed session parent must reject the durable append");
    assert!(unblocked.load(Ordering::SeqCst));
    assert!(control.handle().is_cancel_requested());
    let ticket = error
        .into_ticket()
        .expect("activated cancellation must return a cleanup ticket");
    drop(root_task_guard);

    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();
    assert!(
        control
            .finalize_cancellation(ticket, true, &mut events)
            .await
            .is_err()
    );
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    Ok(())
}
