use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_application::ApplicationCommandReceipt;
use sigil_kernel::{
    AgentThreadId, JsonlSessionStore, LogicalRunId, Session, SessionScopeId, UserInputActionV1,
    UserInputAnswerV1, UserInputAnswerValueV1, UserInputCommandId, UserInputContinuationBindingV1,
    UserInputDecisionCommandV1, UserInputDecisionV1, UserInputIdentityV1,
    UserInputLifecycleEntryV1, UserInputPurposeV1, UserInputQuestionV1, UserInputRequestId,
    UserInputRequestV1, UserInputRequestedV1, UserInputSourceV1,
};

use super::{WorkerRuntime, process_app_action};
use crate::{
    app::{AppAction, AppState},
    runner::{WorkerCommand, WorkerMessage},
};

#[test]
fn launcher_resumes_agent_input_with_a_new_causally_bound_operation() -> Result<()> {
    assert_launcher_recovery_uses_new_key_bound_to_committed_decision(UserInputSourceV1::Agent)
}

fn assert_launcher_recovery_uses_new_key_bound_to_committed_decision(
    source: UserInputSourceV1,
) -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let runtime = tokio::runtime::Runtime::new()?;
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
        &[
            Channel::ApplicationControlLog,
            Channel::ApplicationCommandIndex,
            Channel::ApplicationControlRecovery,
        ],
    )?;
    let (worker_tx, command_rx) = crate::application_bridge::tests::acknowledged_test_channel(
        Some(durable_session.application_operation_owner()?),
    );
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
    let original_request = application
        .prepare_action(&action, None, None)?
        .expect("original answer request");
    let operation = sigil_runtime::application_operation_owner::application_operation_binding(
        &original_request,
    )?
    .expect("user input operation");
    let first = runtime.block_on(application.execute_prepared(original_request.clone()))?;
    assert!(matches!(first, ApplicationCommandReceipt::Uncertain(_)));
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

    // The actual owner commits its accepted decision and causal marker in one batch; losing the
    // transport response cannot require another answer dispatch under the original key.
    durable_session.bind_application_operation(operation.clone())?;
    sigil_kernel::accept_user_input_decision(&mut durable_session, command.clone(), 20)?;
    // The fixture commits directly through its owner, so publish the same fresh projection
    // that the production worker delivers before the user creates a new Resume intent.
    runtime.block_on(application.refresh())?;
    // Reconcile on the production admission thread after the caller's runtime has gone away.
    // A domain proof makes this path read the real durable frontier via Tokio spawn_blocking;
    // polling only on the test runtime, or returning no proof, hides a missing thread runtime.
    drop(runtime);
    assert!(tokio::runtime::Handle::try_current().is_err());
    let mut pending = super::PendingApplicationAdmission {
        application: Arc::clone(&application),
        request: Arc::new(std::sync::Mutex::new(Some(original_request.clone()))),
        action,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
    };
    pending.start()?;
    let received = pending
        .receiver
        .as_ref()
        .expect("admission receipt receiver")
        .recv_timeout(Duration::from_secs(5));
    super::wait_for_owned_thread(
        &mut pending.handle,
        std::time::Instant::now() + Duration::from_secs(5),
    )?;
    let replay = received??;
    assert!(matches!(
        replay,
        ApplicationCommandReceipt::Replayed(_) | ApplicationCommandReceipt::Settled(_)
    ));
    assert!(matches!(
        command_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let runtime = tokio::runtime::Runtime::new()?;

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
    assert!(matches!(
        &resume,
        AppAction::ResumeCommittedUserInput { .. }
    ));
    process_app_action(&mut app, &mut worker, resume)?;
    assert!(matches!(
        command_rx.recv_timeout(Duration::from_secs(5)).with_context(|| {
            format!(
                "resume was not dispatched; admission result: {:?}",
                worker.as_ref().and_then(|worker| worker.pending_interactions.first())
                    .and_then(|pending| pending.receiver.as_ref())
                    .map(mpsc::Receiver::try_recv)
            )
        })?,
        WorkerCommand::ResumeCommittedUserInput { original_operation } if *original_operation == operation
    ));
    let pending = &worker
        .as_ref()
        .expect("attached worker")
        .pending_interactions[0];
    let pending_request = pending
        .request
        .lock()
        .expect("request mutex")
        .clone()
        .expect("frozen continuation request");
    assert_ne!(
        pending_request.envelope.command_id,
        original_request.envelope.command_id
    );
    let sigil_application::ApplicationCommand::UserInput(
        sigil_application::UserInputCommand::ResumeCommittedUserInput {
            original_key,
            original_fingerprint,
            original_operation_id,
            ..
        },
    ) = &pending_request.envelope.command
    else {
        panic!("typed continuation command");
    };
    assert_eq!(
        **original_key,
        original_request
            .admission
            .reservation_key(&original_request.envelope.command_id)
    );
    assert_eq!(*original_fingerprint, operation.fingerprint);
    assert_eq!(*original_operation_id, operation.operation_id);
    let unchanged = runtime.block_on(application.execute_prepared(original_request))?;
    assert!(matches!(unchanged, ApplicationCommandReceipt::Replayed(_)));
    assert!(matches!(
        command_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    super::shutdown_and_join_worker(&mut worker)?;
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
            question: "Which module should be changed?".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
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
