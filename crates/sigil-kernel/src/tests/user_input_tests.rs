use anyhow::Result;

use super::*;
use crate::{
    AgentRunDisposition, ControlEntry, DurableEventType, EventClass, JsonlSessionStore,
    ModelMessage, PublicConversationPhase, PublicRunEventKind, PublicTaskEventProjector, Session,
    ToolCall, ToolExecutionStatus, TypedDomainEvent,
};

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn identity(request_id: &str, generation: u32) -> Result<UserInputIdentityV1> {
    Ok(UserInputIdentityV1 {
        session_scope_id: SessionScopeId::new("session_scope")?,
        root_logical_run_id: LogicalRunId::new("root_run")?,
        source_thread_id: AgentThreadId::new("main")?,
        request_id: UserInputRequestId::new(request_id)?,
        generation,
        source_binding_hash: digest('a'),
    })
}

fn text_request(request_id: &str, generation: u32) -> Result<UserInputRequestedV1> {
    UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: identity(request_id, generation)?,
        source: UserInputSourceV1::Agent,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "I need one missing constraint before continuing.".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "scope".to_owned(),
            header: "Scope".to_owned(),
            question: "Which scope should I use?".to_owned(),
            description: Some("Choose the narrowest useful scope.".to_owned()),
            required: true,
            field: UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 256,
            },
        }],
        allowed_actions: vec![
            UserInputActionV1::Submit,
            UserInputActionV1::Decline,
            UserInputActionV1::CancelRun,
        ],
        requested_at_unix_ms: 10,
        continuation: Some(UserInputContinuationBindingV1 {
            assistant_message_id: "assistant_1".to_owned(),
            tool_call_id: "call_1".to_owned(),
            provider_name: "test".to_owned(),
            model_name: "model".to_owned(),
        }),
    })
}

fn text_request_for_session(
    session: &Session,
    request_id: &str,
    generation: u32,
) -> Result<UserInputRequestedV1> {
    let mut request = text_request(request_id, generation)?.request;
    request.identity.session_scope_id = SessionScopeId::new(session.session_scope_id())?;
    UserInputRequestedV1::new(request)
}

fn submitted_decision(request: &UserInputRequestedV1) -> Result<UserInputDecisionAcceptedV1> {
    UserInputDecisionAcceptedV1::new(
        request,
        UserInputCommandId::new("command_1")?,
        UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "kernel and runtime".to_owned(),
                },
            }],
        },
        20,
    )
}

#[test]
fn lifecycle_reduces_request_answer_claim_start_and_resolution() -> Result<()> {
    let request = text_request("request_1", 1)?;
    let decision = submitted_decision(&request)?;
    let claim = UserInputContinuationClaimedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: request.request.identity.clone(),
        request_hash: request.request_hash.clone(),
        claim_id: UserInputClaimId::new("claim_1")?,
        supervisor_instance_id: "supervisor_1".to_owned(),
        claimed_at_unix_ms: 30,
    };
    let started = UserInputContinuationStartedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: request.request.identity.clone(),
        request_hash: request.request_hash.clone(),
        claim_id: claim.claim_id.clone(),
        continuation_logical_run_id: LogicalRunId::new("continuation_1")?,
        physical_attempt_id: "physical_1".to_owned(),
        started_at_unix_ms: 40,
    };
    let resolved = UserInputResolvedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: request.request.identity.clone(),
        request_hash: request.request_hash.clone(),
        resolution: UserInputResolutionV1::Consumed,
        resolved_at_unix_ms: 50,
    };
    let mut projection = UserInputProjectionV1::default();
    projection.apply(UserInputLifecycleEntryV1::Requested(Box::new(
        request.clone(),
    )))?;
    projection.apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
        decision,
    )))?;
    projection.apply(UserInputLifecycleEntryV1::ContinuationClaimed(claim))?;
    projection.apply(UserInputLifecycleEntryV1::ContinuationStarted(started))?;
    projection.apply(UserInputLifecycleEntryV1::Resolved(resolved))?;

    let state = projection
        .request(&request.request.identity)
        .expect("request should be projected");
    assert_eq!(state.status, UserInputStatusV1::Resolved);
    assert!(state.is_terminal());
    assert_eq!(
        state
            .public_view()
            .answer_receipt
            .expect("submitted request should expose a safe answer receipt")
            .answered_question_ids,
        vec!["scope"]
    );
    let public_json = serde_json::to_string(&state.public_view())?;
    assert!(!public_json.contains("kernel and runtime"));

    let disposition = AgentRunDisposition::AwaitingUserInput((&request).into());
    assert!(matches!(
        disposition,
        AgentRunDisposition::AwaitingUserInput(reference)
            if reference.request_hash == request.request_hash
    ));
    Ok(())
}

#[test]
fn reducer_rejects_duplicate_decision_stale_hash_and_invalid_order() -> Result<()> {
    let request = text_request("request_1", 1)?;
    let decision = submitted_decision(&request)?;
    let mut projection = UserInputProjectionV1::default();
    projection.apply(UserInputLifecycleEntryV1::Requested(Box::new(
        request.clone(),
    )))?;

    let mut stale = decision.clone();
    stale.request_hash = digest('b');
    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(stale)))
            .is_err()
    );
    projection.apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
        decision.clone(),
    )))?;
    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
                decision
            )))
            .is_err()
    );

    let started_without_claim = UserInputContinuationStartedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: request.request.identity,
        request_hash: request.request_hash,
        claim_id: UserInputClaimId::new("missing_claim")?,
        continuation_logical_run_id: LogicalRunId::new("continuation")?,
        physical_attempt_id: "physical".to_owned(),
        started_at_unix_ms: 30,
    };
    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::ContinuationStarted(
                started_without_claim
            ))
            .is_err()
    );
    Ok(())
}

#[test]
fn reducer_requires_one_pending_request_per_agent_and_contiguous_generations() -> Result<()> {
    let mut empty = UserInputProjectionV1::default();
    assert!(
        empty
            .apply(UserInputLifecycleEntryV1::Requested(Box::new(
                text_request("starts_at_two", 2)?
            )))
            .is_err()
    );
    let first = text_request("stable_request", 1)?;
    let mut projection = UserInputProjectionV1::default();
    projection.apply(UserInputLifecycleEntryV1::Requested(Box::new(
        first.clone(),
    )))?;
    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::Requested(Box::new(
                text_request("other_request", 1)?
            )))
            .is_err()
    );

    let declined = UserInputDecisionAcceptedV1::new(
        &first,
        UserInputCommandId::new("decline_1")?,
        UserInputDecisionV1::Declined,
        20,
    )?;
    projection.apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
        declined,
    )))?;
    projection.apply(UserInputLifecycleEntryV1::Resolved(UserInputResolvedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: first.request.identity.clone(),
        request_hash: first.request_hash,
        resolution: UserInputResolutionV1::Declined,
        resolved_at_unix_ms: 21,
    }))?;

    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::Requested(Box::new(
                text_request("stable_request", 3)?
            )))
            .is_err()
    );
    projection.apply(UserInputLifecycleEntryV1::Requested(Box::new(
        text_request("stable_request", 2)?,
    )))?;
    Ok(())
}

#[test]
fn root_run_agent_request_budget_is_durable_and_bounded() -> Result<()> {
    let mut projection = UserInputProjectionV1::default();
    for ordinal in 1..=MAX_USER_INPUT_REQUESTS_PER_ROOT_RUN {
        let request = text_request(&format!("request_{ordinal}"), 1)?;
        projection.apply(UserInputLifecycleEntryV1::Requested(Box::new(
            request.clone(),
        )))?;
        let decision = UserInputDecisionAcceptedV1::new(
            &request,
            UserInputCommandId::new(format!("decline_{ordinal}"))?,
            UserInputDecisionV1::Declined,
            20,
        )?;
        projection.apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
            decision,
        )))?;
        projection.apply(UserInputLifecycleEntryV1::Resolved(UserInputResolvedV1 {
            schema_version: USER_INPUT_SCHEMA_VERSION,
            identity: request.request.identity,
            request_hash: request.request_hash,
            resolution: UserInputResolutionV1::Declined,
            resolved_at_unix_ms: 21,
        }))?;
    }
    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::Requested(Box::new(
                text_request("request_4", 1)?
            )))
            .is_err()
    );
    Ok(())
}

#[test]
fn answer_validation_is_typed_bounded_and_complete() -> Result<()> {
    let request = text_request("request_1", 1)?;
    let missing = UserInputDecisionAcceptedV1::new(
        &request,
        UserInputCommandId::new("missing")?,
        UserInputDecisionV1::Submitted {
            answers: Vec::new(),
        },
        20,
    );
    assert!(missing.is_err());

    let wrong_kind = UserInputDecisionAcceptedV1::new(
        &request,
        UserInputCommandId::new("wrong")?,
        UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Boolean { value: true },
            }],
        },
        20,
    );
    assert!(wrong_kind.is_err());

    let oversized = UserInputDecisionAcceptedV1::new(
        &request,
        UserInputCommandId::new("oversized")?,
        UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "x".repeat(257),
                },
            }],
        },
        20,
    );
    assert!(oversized.is_err());
    Ok(())
}

#[test]
fn mcp_decision_persists_only_answer_hash_and_cannot_claim_continuation() -> Result<()> {
    let requested = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: identity("mcp_request", 1)?,
        source: UserInputSourceV1::Mcp {
            server_id: "server".to_owned(),
            call_id: "mcp_call".to_owned(),
        },
        purpose: UserInputPurposeV1::ExternalElicitation,
        prompt: "Choose a format.".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "format".to_owned(),
            header: "Format".to_owned(),
            question: "Which output format?".to_owned(),
            description: None,
            required: true,
            field: UserInputFieldKindV1::SingleSelect {
                options: vec![
                    UserInputOptionV1 {
                        id: "json".to_owned(),
                        label: "JSON".to_owned(),
                        description: None,
                    },
                    UserInputOptionV1 {
                        id: "yaml".to_owned(),
                        label: "YAML".to_owned(),
                        description: None,
                    },
                ],
                allow_other: false,
            },
        }],
        allowed_actions: vec![UserInputActionV1::Submit, UserInputActionV1::Decline],
        requested_at_unix_ms: 10,
        continuation: None,
    })?;
    let decision = UserInputDecisionAcceptedV1::new(
        &requested,
        UserInputCommandId::new("mcp_decision")?,
        UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "format".to_owned(),
                value: UserInputAnswerValueV1::SingleSelect {
                    option_id: Some("json".to_owned()),
                    other: None,
                },
            }],
        },
        20,
    )?;
    assert!(matches!(
        &decision.decision,
        UserInputDurableDecisionV1::Submitted { answers: None, .. }
    ));
    assert!(!serde_json::to_string(&decision)?.contains("json"));

    let mut projection = UserInputProjectionV1::default();
    projection.apply(UserInputLifecycleEntryV1::Requested(Box::new(
        requested.clone(),
    )))?;
    projection.apply(UserInputLifecycleEntryV1::DecisionAccepted(Box::new(
        decision,
    )))?;
    assert!(
        projection
            .apply(UserInputLifecycleEntryV1::ContinuationClaimed(
                UserInputContinuationClaimedV1 {
                    schema_version: USER_INPUT_SCHEMA_VERSION,
                    identity: requested.request.identity.clone(),
                    request_hash: requested.request_hash.clone(),
                    claim_id: UserInputClaimId::new("claim")?,
                    supervisor_instance_id: "supervisor".to_owned(),
                    claimed_at_unix_ms: 30,
                }
            ))
            .is_err()
    );
    projection.apply(UserInputLifecycleEntryV1::Resolved(UserInputResolvedV1 {
        schema_version: USER_INPUT_SCHEMA_VERSION,
        identity: requested.request.identity,
        request_hash: requested.request_hash,
        resolution: UserInputResolutionV1::Consumed,
        resolved_at_unix_ms: 30,
    }))?;
    Ok(())
}

#[test]
fn session_batch_validates_before_append_and_persists_recovery_critical_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    crate::session::append_current_test_session_identity(&store)?;
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    let request = text_request_for_session(&session, "request_1", 1)?;
    let decision = submitted_decision(&request)?;

    session.append_user_input_lifecycle(vec![
        UserInputLifecycleEntryV1::Requested(Box::new(request.clone())),
        UserInputLifecycleEntryV1::DecisionAccepted(Box::new(decision.clone())),
    ])?;
    assert_eq!(
        session
            .user_input_projection()?
            .request(&request.request.identity)
            .expect("appended user input request should project")
            .status,
        UserInputStatusV1::DecisionAccepted
    );
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(records.len(), 3);
    assert!(records.iter().skip(1).all(|record| {
        record.stored_event().event_type == DurableEventType::UserInputLifecycleChanged.as_str()
            && record.stored_event().event_class == EventClass::Critical
    }));
    assert!(matches!(
        records[1]
            .typed_domain_event_record()?
            .expect("known user input event should decode")
            .event,
        TypedDomainEvent::UserInputLifecycleChanged(ControlEntry::UserInputRequested(_))
    ));
    let restored = Session::load_from_store("test", "model", store.clone())?;
    assert_eq!(
        restored
            .user_input_projection()?
            .request(&request.request.identity)
            .expect("restored user input request should project")
            .status,
        UserInputStatusV1::DecisionAccepted
    );

    let before = session.entries().len();
    let invalid = text_request_for_session(&session, "request_2", 1)?;
    assert!(
        session
            .append_user_input_lifecycle(vec![
                UserInputLifecycleEntryV1::Requested(Box::new(invalid.clone())),
                UserInputLifecycleEntryV1::Requested(Box::new(invalid)),
            ])
            .is_err()
    );
    assert_eq!(session.entries().len(), before);
    assert_eq!(
        JsonlSessionStore::read_event_records(store.path())?.len(),
        3
    );

    assert!(
        session
            .append(SessionLogEntry::Control(
                ControlEntry::UserInputDecisionAccepted(Box::new(decision))
            ))
            .is_err()
    );
    assert_eq!(session.entries().len(), before);
    assert_eq!(
        JsonlSessionStore::read_event_records(store.path())?.len(),
        3
    );
    Ok(())
}

#[test]
fn decision_and_continuation_are_idempotent_and_settle_the_exact_tool_call() -> Result<()> {
    let mut session = Session::new("test", "model");
    let mut assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call_1".to_owned(),
            name: REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    assistant.id = "assistant_1".to_owned();
    session.append_assistant_message(assistant)?;
    let request = text_request_for_session(&session, "request_1", 1)?;
    session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(Box::new(
        request.clone(),
    ))])?;
    let command = UserInputDecisionCommandV1 {
        identity: request.request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: UserInputCommandId::new("command_1")?,
        decision: UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "kernel and runtime".to_owned(),
                },
            }],
        },
    };

    let accepted = accept_user_input_decision(&mut session, command.clone(), 20)?;
    assert!(!accepted.idempotent_replay);
    assert!(accepted.continuation_required);
    let replay = accept_user_input_decision(&mut session, command, 99)?;
    assert!(replay.idempotent_replay);

    let prepared = prepare_user_input_continuation(
        &mut session,
        &request.request.identity,
        &request.request_hash,
        "supervisor_1",
        "physical_1",
        30,
    )?;
    assert!(!prepared.already_started);
    let replayed = prepare_user_input_continuation(
        &mut session,
        &request.request.identity,
        &request.request_hash,
        "supervisor_2",
        "physical_2",
        40,
    )?;
    assert!(replayed.already_started);
    assert_eq!(replayed.continuation, prepared.continuation);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::ToolResultV3(result)
                    if result.call_id == "call_1"
                        && result.tool_name == REQUEST_USER_INPUT_TOOL_NAME
            ))
            .count(),
        1
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call_1"
                && execution.status == ToolExecutionStatus::Completed
    )));
    Ok(())
}

#[test]
fn public_projector_emits_full_request_without_answer_values() -> Result<()> {
    let request = text_request("request_1", 1)?;
    let decision = submitted_decision(&request)?;
    let mut projector = PublicTaskEventProjector::default();
    let requested =
        projector.project_control(&ControlEntry::UserInputRequested(Box::new(request.clone())));
    assert!(matches!(
        &requested[..],
        [PublicRunEventKind::UserInputChanged {
            request_id,
            generation: 1,
            status: UserInputStatusV1::Requested,
            request: public,
            ..
        }] if request_id == "request_1" && public.questions.len() == 1
    ));

    let accepted =
        projector.project_control(&ControlEntry::UserInputDecisionAccepted(Box::new(decision)));
    let serialized = serde_json::to_string(&accepted)?;
    assert!(serialized.contains("decision_accepted"));
    assert!(!serialized.contains("kernel and runtime"));
    assert_eq!(
        PublicConversationPhase::AwaitingUserInput.as_str(),
        "awaiting_user_input"
    );
    Ok(())
}
