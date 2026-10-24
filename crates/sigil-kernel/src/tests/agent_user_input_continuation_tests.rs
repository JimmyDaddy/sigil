use super::*;
use crate::{
    CompletionRequest, FrozenProviderRequestMaterial, JsonlSessionStore, ModelMessage,
    ProviderPhysicalAttemptOutcome, UserInputAnswerV1, UserInputAnswerValueV1, UserInputClaimId,
    UserInputCommandId, UserInputContinuationClaimedV1, UserInputDecisionCommandV1,
    UserInputDecisionV1, UserInputLifecycleEntryV1, UserInputStatusV1,
};

struct Handler;
impl EventHandler for Handler {
    fn handle(&mut self, _event: crate::RunEvent) -> Result<()> {
        Ok(())
    }
}

struct AtomicPublicHandler;
impl EventHandler for AtomicPublicHandler {
    fn handle(&mut self, _event: crate::RunEvent) -> Result<()> {
        Ok(())
    }
    fn commit_controls(
        &mut self,
        session: &mut Session,
        controls: Vec<ControlEntry>,
    ) -> Result<Vec<crate::StoredEvent>> {
        session
            .append_controls_with_public_outbox(controls, "continuation-public", 1)
            .map(|(events, _)| events)
    }
}

fn question(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: crate::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        args_json: r#"{"questions":[{"id":"scope","question":"Which scope?"}]}"#.to_owned(),
    }
}

fn context<'a>(thread: &'a AgentThreadId, logical_run: &'a str) -> RequestUserInputContext<'a> {
    RequestUserInputContext {
        logical_run_id: logical_run,
        root_logical_run_id: "question-root",
        source_thread_id: thread,
        provider_name: "fixture",
        model_name: "model",
        source: UserInputSourceV1::Agent,
    }
}

fn seed() -> Result<(tempfile::TempDir, Session, UserInputRequestRefV1)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("fixture", "model", store)?;
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&crate::ConversationRunStartedEntryV1::new(
            "continuation-public",
            1,
        )?)?;
    let call = question("question-one");
    session.append_assistant_message(ModelMessage::assistant(None, vec![call.clone()]))?;
    let reference = handle_request_user_input_call(
        &mut session,
        &mut Handler,
        &call,
        context(&AgentThreadId::new("main")?, "question-root"),
    )?;
    Ok((temp, session, reference))
}

fn accept(
    session: &mut Session,
    request: &UserInputRequestRefV1,
) -> Result<UserInputDecisionCommandV1> {
    let command = UserInputDecisionCommandV1 {
        identity: request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: UserInputCommandId::new("answer-one")?,
        decision: UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "narrow".to_owned(),
                },
            }],
        },
    };
    crate::accept_user_input_decision(session, command.clone(), 2)?;
    Ok(command)
}

fn start(session: &mut Session, request: &UserInputRequestRefV1) -> Result<String> {
    Ok(crate::prepare_user_input_continuation(
        session,
        &request.identity,
        &request.request_hash,
        "owner",
        "bound-physical",
        3,
    )?
    .continuation
    .continuation_logical_run_id
    .as_str()
    .to_owned())
}

async fn provider_terminal(
    session: &Session,
    logical_run: &str,
    outcome: ProviderPhysicalAttemptOutcome,
) -> Result<()> {
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "fixture".to_owned(),
            model_name: "model".to_owned(),
            messages: session.messages(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(128),
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let mut attempt = crate::session::ProviderPhysicalAttemptAudit::start_with_id(
        session,
        logical_run,
        &frozen,
        "bound-physical",
    )
    .await?;
    attempt.finish(session, outcome, None).await
}

fn append_second_call(session: &mut Session) -> Result<ToolCall> {
    let call = question("question-two");
    session.append_assistant_message(ModelMessage::assistant(None, vec![call.clone()]))?;
    Ok(call)
}

#[tokio::test]
async fn sequential_user_input_requires_the_exact_consumed_continuation_boundary() -> Result<()> {
    for boundary in [
        "requested",
        "accepted",
        "declined",
        "claimed",
        "not-dispatched",
        "no-consumption",
        "wrong-logical",
        "wrong-root",
        "wrong-source",
    ] {
        let (temp, mut session, first) = seed()?;
        if boundary == "declined" {
            let requested = session
                .user_input_projection()?
                .request(&first.identity)
                .expect("initial request")
                .requested
                .clone();
            let declined = crate::UserInputDecisionAcceptedV1::new(
                &requested,
                UserInputCommandId::new("declined-answer")?,
                UserInputDecisionV1::Declined,
                2,
            )?;
            session.append_user_input_lifecycle(vec![
                UserInputLifecycleEntryV1::DecisionAccepted(Box::new(declined)),
            ])?;
            assert!(
                session
                    .append_user_input_lifecycle(vec![
                        UserInputLifecycleEntryV1::ContinuationClaimed(
                            UserInputContinuationClaimedV1 {
                                schema_version: USER_INPUT_SCHEMA_VERSION,
                                identity: first.identity.clone(),
                                request_hash: first.request_hash.clone(),
                                claim_id: UserInputClaimId::new("declined-claim")?,
                                supervisor_instance_id: "owner".to_owned(),
                                claimed_at_unix_ms: 3,
                            }
                        ),
                    ])
                    .is_err(),
                "a declined decision must never claim a continuation"
            );
        } else if boundary != "requested" {
            accept(&mut session, &first)?;
        }
        let mut logical = "not-a-continuation".to_owned();
        if boundary == "claimed" {
            session.append_user_input_lifecycle(vec![
                UserInputLifecycleEntryV1::ContinuationClaimed(UserInputContinuationClaimedV1 {
                    schema_version: USER_INPUT_SCHEMA_VERSION,
                    identity: first.identity.clone(),
                    request_hash: first.request_hash.clone(),
                    claim_id: UserInputClaimId::new("claim")?,
                    supervisor_instance_id: "owner".to_owned(),
                    claimed_at_unix_ms: 3,
                }),
            ])?;
        } else if !matches!(boundary, "requested" | "accepted" | "declined") {
            logical = start(&mut session, &first)?;
            if boundary != "not-dispatched" {
                provider_terminal(
                    &session,
                    &logical,
                    if boundary == "no-consumption" {
                        ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
                    } else {
                        ProviderPhysicalAttemptOutcome::Completed
                    },
                )
                .await?;
            }
        }
        let call = append_second_call(&mut session)?;
        let thread = AgentThreadId::new("main")?;
        let mut request_context = context(&thread, &logical);
        if boundary == "wrong-logical" {
            request_context.logical_run_id = "wrong-continuation";
        }
        if boundary == "wrong-root" {
            request_context.root_logical_run_id = "different-root";
        }
        if boundary == "wrong-source" {
            request_context.source = UserInputSourceV1::PlanReviewResearch {
                plan_review_id: crate::PlanReviewId::new("different-review")?,
                attempt_id: crate::PlanReviewAttemptId::new("different-attempt")?,
            };
        }
        let before = std::fs::read(temp.path().join("session.jsonl"))?;
        assert!(
            handle_request_user_input_call(&mut session, &mut Handler, &call, request_context)
                .is_err(),
            "{boundary} cannot consume the old request or admit another question"
        );
        assert_eq!(
            std::fs::read(temp.path().join("session.jsonl"))?,
            before,
            "{boundary}"
        );
        assert_eq!(
            session.user_input_projection()?.pending().count(),
            1,
            "{boundary}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn sequential_user_input_atomic_publication_survives_restart_without_answer_replay()
-> Result<()> {
    let (temp, mut session, first) = seed()?;
    let command = accept(&mut session, &first)?;
    let logical = start(&mut session, &first)?;
    provider_terminal(
        &session,
        &logical,
        ProviderPhysicalAttemptOutcome::Completed,
    )
    .await?;
    let call = append_second_call(&mut session)?;
    let thread = AgentThreadId::new("main")?;
    let second = handle_request_user_input_call(
        &mut session,
        &mut AtomicPublicHandler,
        &call,
        context(&thread, &logical),
    )?;
    let projection = session.user_input_projection()?;
    assert_eq!(
        projection
            .request(&first.identity)
            .expect("old request")
            .status,
        UserInputStatusV1::Resolved
    );
    assert_eq!(
        projection
            .pending()
            .next()
            .expect("new pending")
            .requested
            .request
            .identity,
        second.identity
    );
    let before = std::fs::read(temp.path().join("session.jsonl"))?;
    let replay = handle_request_user_input_call(
        &mut session,
        &mut AtomicPublicHandler,
        &call,
        context(&thread, &logical),
    )?;
    assert_eq!(replay, second);
    let receipt = crate::accept_user_input_decision(&mut session, command, 9)?;
    assert!(receipt.idempotent_replay);
    assert!(!receipt.continuation_required);
    assert_eq!(std::fs::read(temp.path().join("session.jsonl"))?, before);
    drop(session);
    let reopened = Session::load_from_store(
        "fixture",
        "model",
        JsonlSessionStore::open_existing(temp.path().join("session.jsonl"))?,
    )?;
    assert!(crate::recoverable_user_input_decision(&reopened)?.is_none());
    assert_eq!(reopened.user_input_projection()?.pending().count(), 1);
    let records = JsonlSessionStore::read_event_records(temp.path().join("session.jsonl"))?;
    let public = crate::PublicEventOutboxProjectionV1::from_records(&records)?;
    assert_eq!(public.events_in_order().len(), 3);
    assert!(matches!(
        &public.events_in_order()[0].event.event,
        crate::PublicRunEventKind::UserInputChanged {
            request_id,
            request_hash,
            status: UserInputStatusV1::Resolved,
            ..
        } if request_id == first.identity.request_id.as_str()
            && request_hash == &first.request_hash
    ));
    let crate::PublicRunEventKind::Control { control } = &public.events_in_order()[1].event.event
    else {
        bail!("the new question must publish its tool execution before the request");
    };
    assert_eq!(control.kind, "tool_execution");
    assert!(matches!(
        serde_json::from_value::<ControlEntry>(
            control.payload.clone().context("tool execution payload")?
        )?,
        ControlEntry::ToolExecution(execution)
            if execution.call_id == call.id
                && execution.tool_name == call.name
                && execution.status == ToolExecutionStatus::Started
    ));
    assert!(matches!(
        &public.events_in_order()[2].event.event,
        crate::PublicRunEventKind::UserInputChanged {
            request_id,
            request_hash,
            status: UserInputStatusV1::Requested,
            ..
        } if request_id == second.identity.request_id.as_str()
            && request_hash == &second.request_hash
    ));
    Ok(())
}

#[tokio::test]
async fn sequential_user_input_writer_fault_never_leaves_consumed_without_the_next_request()
-> Result<()> {
    for fault in [
        crate::session::SessionWriterFault::BeforeWrite,
        crate::session::SessionWriterFault::PartialFirstRecord,
        crate::session::SessionWriterFault::PartialSecondRecord,
        crate::session::SessionWriterFault::BeforeSync,
    ] {
        let (temp, mut session, first) = seed()?;
        accept(&mut session, &first)?;
        let logical = start(&mut session, &first)?;
        provider_terminal(
            &session,
            &logical,
            ProviderPhysicalAttemptOutcome::Completed,
        )
        .await?;
        let call = append_second_call(&mut session)?;
        session
            .durable_store()
            .expect("durable fixture")
            .inject_writer_fault(fault)?;
        let result = handle_request_user_input_call(
            &mut session,
            &mut AtomicPublicHandler,
            &call,
            context(&AgentThreadId::new("main")?, &logical),
        );
        if let Err(error) = result {
            assert!(
                error
                    .downcast_ref::<RequestUserInputPublicationFailure>()
                    .is_some()
            );
            assert_eq!(
                session
                    .user_input_projection()?
                    .request(&first.identity)
                    .expect("old request")
                    .status,
                UserInputStatusV1::ContinuationStarted
            );
        }
        drop(session);
        let reopened = Session::load_from_store(
            "fixture",
            "model",
            JsonlSessionStore::open_existing(temp.path().join("session.jsonl"))?,
        )?;
        let projection = reopened.user_input_projection()?;
        let consumed = projection
            .request(&first.identity)
            .expect("old request")
            .status
            == UserInputStatusV1::Resolved;
        let requested = projection
            .public_requests()
            .iter()
            .any(|request| request.identity != first.identity);
        assert_eq!(
            consumed, requested,
            "{fault:?}: old answer and new question are one durable batch"
        );
        assert_eq!(projection.pending().count(), 1);
    }
    Ok(())
}
