use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_application::ApplicationCommandReceipt;
use sigil_kernel::{
    AgentThreadId, JsonlSessionStore, LogicalRunId, Session, SessionScopeId, TaskId,
    UserInputActionV1, UserInputAnswerV1, UserInputAnswerValueV1, UserInputCommandId,
    UserInputContinuationBindingV1, UserInputDecisionCommandV1, UserInputDecisionV1,
    UserInputFieldKindV1, UserInputIdentityV1, UserInputLifecycleEntryV1, UserInputPurposeV1,
    UserInputQuestionV1, UserInputRequestId, UserInputRequestV1, UserInputRequestedV1,
    UserInputSourceV1,
};

use super::{WorkerRuntime, process_app_action};
use crate::{
    app::{AppAction, AppState},
    runner::{WorkerCommand, WorkerCommandSender, WorkerMessage},
};

#[test]
fn launcher_resumes_agent_input_past_the_original_uncertain_application_receipt() -> Result<()> {
    assert_launcher_recovery_bypasses_only_cached_dispatch(UserInputSourceV1::Agent)
}

#[test]
fn launcher_resumes_planner_input_past_the_original_uncertain_application_receipt() -> Result<()> {
    assert_launcher_recovery_bypasses_only_cached_dispatch(UserInputSourceV1::Planner {
        task_id: TaskId::new("launcher-recovery-task")?,
    })
}

fn assert_launcher_recovery_bypasses_only_cached_dispatch(source: UserInputSourceV1) -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let fixture = tempfile::tempdir()?;
    let config_path = fixture.path().join("sigil.toml");
    let session_path = fixture.path().join("session.jsonl");
    let state_root = fixture.path().join("state");
    let execution_temp = fixture.path().join("execution-temp");
    std::fs::create_dir_all(state_root.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    config.storage.state_root = sigil_kernel::StorageRoot::Path(state_root.display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(state_root.join("cache").display().to_string());
    config.save(&config_path)?;
    let (provider_name, model_route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let store = JsonlSessionStore::new(&session_path)?;
    let mut durable_session =
        Session::new_with_route(provider_name, model_route).with_store(store.clone());
    durable_session.ensure_identity_entry()?;
    sigil_runtime::bind_session_composition(&mut durable_session, &config)?;
    let requested = recovery_request(&durable_session, source)?;
    durable_session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(
        Box::new(requested.clone()),
    )])?;
    let command = UserInputDecisionCommandV1 {
        identity: requested.request.identity.clone(),
        request_hash: requested.request_hash.clone(),
        command_id: UserInputCommandId::new("launcher-accepted-input-command")?,
        decision: UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "the exact privately persisted answer".to_owned(),
                },
            }],
        },
    };
    let action = AppAction::SubmitUserInputDecision {
        command_id: Some(command.command_id.as_str().to_owned()),
        request_id: command.identity.request_id.as_str().to_owned(),
        generation: command.identity.generation,
        expected_request_hash: command.request_hash.clone(),
        decision: command.decision.clone(),
    };
    let composition = sigil_runtime::r71_authority_composition::compose_runtime_authority(
        &state_root,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x61; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[Channel::ApplicationControlLog],
    )?;
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let application = Arc::new(crate::application_bridge::tests::connect_real_worker(
        &config_path,
        fixture.path(),
        &session_path,
        durable_session.session_scope_id(),
        worker_tx.clone(),
        &composition,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
    )?);
    runtime.block_on(application.refresh())?;
    let first = application
        .try_execute_action(&action, None, None)?
        .expect("the shipping application bridge must admit the original answer");
    let ApplicationCommandReceipt::Uncertain(original_receipt) = first else {
        panic!("an asynchronous worker enqueue must retain an uncertain receipt");
    };
    assert_eq!(
        original_receipt.owner_recovery_binding.as_deref(),
        Some("tui-worker:launcher-accepted-input-command")
    );
    assert!(matches!(
        command_rx.recv_timeout(Duration::from_secs(1))?,
        WorkerCommand::SubmitUserInputDecision {
            command_id: Some(command_id), request_id, generation, expected_request_hash, decision,
        } if command_id == command.command_id.as_str()
            && request_id == command.identity.request_id.as_str()
            && generation == command.identity.generation
            && expected_request_hash == command.request_hash
            && decision == command.decision
    ));

    // Model the worker accepting the answer durably before its continuation is dispatched.
    // The application enqueue reservation remains Uncertain across that independent write.
    sigil_kernel::accept_user_input_decision(&mut durable_session, command.clone(), 20)?;
    let replay = application
        .try_execute_action(&action, None, None)?
        .expect("the exact original application command must replay");
    assert!(matches!(replay,
        ApplicationCommandReceipt::ReplayedUncertain(receipt) if receipt == original_receipt
    ));
    assert!(
        matches!(command_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a cached application receipt must not enqueue a second answer"
    );

    let mut app = AppState::from_root_config(&config_path, &config);
    app.session_log_path = session_path;
    app.session_id = durable_session.session_scope_id().to_owned();
    app.handle_worker_message(WorkerMessage::RecoveredUserInputAttention {
        command: command.clone(),
        entries: durable_session.entries().to_vec(),
    })?;
    assert_eq!(
        app.pending_user_input()
            .and_then(|form| form.recovery_command.as_ref()),
        Some(&command),
        "the worker recovery message must restore the exact private Resume command"
    );
    let resume = app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
        .expect("the recovered form must expose Resume");
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        application: Some(Arc::clone(&application)),
        pending_admission: None,
        pending_interactions: Vec::new(),
        join_handle: None,
        worker_tx,
        worker_rx,
        ready: true,
    });
    // The ordinary launcher application branch is disabled under cfg(test). The calls above
    // establish the real service/cache boundary directly; this call exercises only the shipping
    // private recovery branch, which must run before ordinary application admission.
    process_app_action(&mut app, &mut worker, resume)?;
    assert!(
        matches!(
            command_rx.recv_timeout(Duration::from_secs(1))?,
            WorkerCommand::ResumeRecoveredUserInput {
                command_id, request_id, generation, expected_request_hash,
            } if command_id == command.command_id.as_str()
                && request_id == command.identity.request_id.as_str()
                && generation == command.identity.generation
                && expected_request_hash == command.request_hash
        ),
        "launcher recovery must reach the worker through the no-answer private command"
    );
    assert!(matches!(
        command_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(
        worker
            .as_ref()
            .expect("worker remains attached")
            .pending_interactions
            .is_empty()
    );

    let unchanged = application
        .try_execute_action(&action, None, None)?
        .expect("the original application reservation must still exist");
    assert!(
        matches!(unchanged,
            ApplicationCommandReceipt::ReplayedUncertain(receipt) if receipt == original_receipt
        ),
        "private recovery must not reopen or replace the application reservation"
    );
    assert!(matches!(
        command_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    Ok(())
}

fn recovery_request(session: &Session, source: UserInputSourceV1) -> Result<UserInputRequestedV1> {
    UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: UserInputIdentityV1 {
            session_scope_id: SessionScopeId::new(session.session_scope_id())?,
            root_logical_run_id: LogicalRunId::new("launcher-recovery-run")?,
            source_thread_id: AgentThreadId::new("main")?,
            request_id: UserInputRequestId::new("launcher-recovery-request")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        source,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "Choose the scope before continuing".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "scope".to_owned(),
            header: "Scope".to_owned(),
            question: "Which module should be changed?".to_owned(),
            description: None,
            required: true,
            field: UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 128,
            },
        }],
        allowed_actions: vec![UserInputActionV1::Submit, UserInputActionV1::CancelRun],
        requested_at_unix_ms: 10,
        continuation: Some(UserInputContinuationBindingV1 {
            assistant_message_id: "launcher-input-assistant".to_owned(),
            tool_call_id: "launcher-input-call".to_owned(),
            provider_name: "fixture".to_owned(),
            model_name: "fixture-model".to_owned(),
        }),
    })
}
