//! A real root Session owns both the user-input decision and its application commit evidence.

use super::*;
use sigil_application::UserInputCommand;
use sigil_kernel::{
    AgentThreadId, ApplicationOperationTargetV1, LogicalRunId, ModelMessage, SessionScopeId,
    UserInputActionV1, UserInputContinuationBindingV1, UserInputDecisionV1,
    UserInputDurableDecisionV1, UserInputIdentityV1, UserInputLifecycleEntryV1, UserInputPurposeV1,
    UserInputQuestionV1, UserInputRequestId, UserInputRequestV1, UserInputRequestedV1,
    UserInputResolutionV1, UserInputSourceV1,
};

fn seed_root_input_request(
    owner: &sigil_kernel::SessionApplicationOperationOwner,
) -> Result<UserInputRequestedV1> {
    let mut session = owner.attach_for_control()?;
    let call = ToolCall {
        id: "http-root-input-call".to_owned(),
        name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        args_json: "{}".to_owned(),
    };
    let source_binding_hash = format!(
        "sha256:{}",
        sigil_kernel::sha256_hex(&serde_json::to_vec(&call)?)
    );
    let mut assistant = ModelMessage::assistant(None, vec![call.clone()]);
    assistant.id = "http-root-input-assistant".to_owned();
    let continuation = UserInputContinuationBindingV1 {
        assistant_message_id: assistant.id.clone(),
        tool_call_id: call.id,
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
    };
    session.append_assistant_message(assistant)?;
    let requested = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: UserInputIdentityV1 {
            session_scope_id: SessionScopeId::new(session.session_scope_id())?,
            root_logical_run_id: LogicalRunId::new("http-root-input-run")?,
            source_thread_id: AgentThreadId::new("main")?,
            request_id: UserInputRequestId::new("http-root-input-request")?,
            generation: 1,
            source_binding_hash,
        },
        source: UserInputSourceV1::Agent,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "Select the migration mode".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "mode".to_owned(),
            question: "Which migration mode should be used?".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
        }],
        allowed_actions: vec![
            UserInputActionV1::Submit,
            UserInputActionV1::Decline,
            UserInputActionV1::CancelRun,
        ],
        requested_at_unix_ms: 10,
        continuation: Some(continuation),
    })?;
    // Seed only a legitimate pending request. The actual HTTP/runtime decision path must
    // prepare and commit its own operation; the fixture does not manufacture either marker.
    session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(Box::new(
        requested.clone(),
    ))])?;
    Ok(requested)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_user_input_receipt_replays_actual_root_decision_after_client_reconnect()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let driver = production_queue_driver(&fixture, "user-input-operation");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("commands-user-input-operation.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let owner = driver.application_operation_owner(&session)?;
    let requested = seed_root_input_request(&owner.owner)?;
    let reader = owner.owner.read_handle();
    assert!(reader.read_event_records()?.iter().all(|record| !matches!(
        record.session_log_entry().expect("valid seeded session"),
        Some(SessionLogEntry::Control(
            ControlEntry::ApplicationOperationPreparedV1(_)
                | ControlEntry::ApplicationOperationCommittedV1(_)
        ))
    )));

    let command_id = "http-root-input-cancel";
    let command = ApplicationCommand::UserInput(UserInputCommand::Resolve {
        binding: requested.request.identity.request_id.as_str().to_owned(),
        generation: requested.request.identity.generation,
        expected_request_hash: SafeText::new(requested.request_hash.clone())?,
        decision: UserInputDecisionV1::RunCancelled,
        permission_mode: None,
    });
    let client = registry.application_client(&session.id, "root-input-client")?;
    client.refresh()?;
    let journal = client.command_journal_binding()?;
    let receipt = client.execute_in_journal(command_id, Some(journal.clone()), command.clone())?;
    let ApplicationCommandReceipt::Settled(settled) = receipt else {
        panic!("the real decision must settle from its source event: {receipt:?}");
    };
    let records = reader.read_event_records()?;
    let committed_bytes = std::fs::read(&session.session_log_path)?;
    let source = records
        .iter()
        .find(|record| record.event_id() == settled.domain_commit.source_event_id)
        .expect("receipt references a real durable source event");
    assert_eq!(
        source.stream_sequence(),
        settled.domain_commit.source_sequence
    );
    assert_eq!(
        sigil_kernel::sha256_hex(source.record_checksum().as_bytes()),
        settled.domain_commit.source_digest
    );
    let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(commit))) =
        source.session_log_entry()?
    else {
        panic!("receipt must name the owning Session's causal commit marker");
    };
    assert!(matches!(
        &commit.binding.target,
        ApplicationOperationTargetV1::UserInputDecision {
            request_id,
            generation: 1,
            request_hash,
            command_id: bound_command,
        } if request_id == requested.request.identity.request_id.as_str()
            && request_hash == &requested.request_hash
            && bound_command == command_id
    ));
    let decisions = records
        .iter()
        .filter(|record| matches!(
            record.session_log_entry().expect("valid decision record"),
            Some(SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(ref decision)))
                if decision.identity == requested.request.identity
        ))
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1, "one actual accepted decision");
    assert!(
        commit
            .domain_events
            .iter()
            .any(|event| event.event_id == decisions[0].event_id())
    );
    assert!(
        sigil_kernel::session::reconcile_application_operation(&reader, &commit.binding)?.is_some(),
        "the actual owner validates the decision and commit as one contiguous batch"
    );

    client.refresh()?;
    assert_eq!(
        client.execute_in_journal(command_id, Some(journal.clone()), command.clone())?,
        ApplicationCommandReceipt::Replayed(settled.clone())
    );
    drop(client);
    // A replacement transport client has no in-memory prepared command. It retains the
    // original client ID, command ID and journal binding as a response-lost retry would.
    let reconnected = registry.application_client(&session.id, "root-input-client")?;
    reconnected.refresh()?;
    assert_eq!(
        reconnected.execute_in_journal(command_id, Some(journal.clone()), command.clone())?,
        ApplicationCommandReceipt::Replayed(settled)
    );
    let mut changed = command;
    if let ApplicationCommand::UserInput(UserInputCommand::Resolve { decision, .. }) = &mut changed
    {
        *decision = UserInputDecisionV1::Declined;
    }
    assert!(matches!(
        reconnected.execute_in_journal(command_id, Some(journal), changed)?,
        ApplicationCommandReceipt::PayloadConflict(_)
    ));
    assert_eq!(
        std::fs::read(&session.session_log_path)?,
        committed_bytes,
        "retries and a changed payload cannot append a second decision or continuation"
    );
    let reopened = owner.owner.attach_for_control()?;
    let projection = reopened.user_input_projection()?;
    let state = projection
        .request(&requested.request.identity)
        .expect("the request remains durably addressable");
    assert!(matches!(
        state.decision.as_ref().expect("accepted decision").decision,
        UserInputDurableDecisionV1::RunCancelled
    ));
    assert_eq!(
        state.public_view().resolution,
        Some(UserInputResolutionV1::RunCancelled)
    );
    assert!(state.claim.is_none());
    assert!(state.continuation.is_none());
    assert!(registry.get_session(&session.id)?.run_ids.is_empty());
    assert!(driver.active_runs.lock().expect("active runs").is_empty());
    Ok(())
}
