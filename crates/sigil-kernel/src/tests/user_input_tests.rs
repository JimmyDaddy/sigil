use anyhow::Result;

use super::*;
use crate::{
    AgentProfileId, AgentRunAttemptId, AgentRunDisposition, ControlEntry, DurableEventType,
    EventClass, JsonlSessionStore, ModelMessage, PublicConversationPhase, PublicRunEventKind,
    PublicTaskEventProjector, Session, TaskIsolationMode, ToolCall, ToolExecutionStatus,
    TypedDomainEvent,
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
            question: "Which scope should I use?".to_owned(),
            description: Some("Choose the narrowest useful scope.".to_owned()),
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

#[test]
fn request_user_input_options_allow_free_form_answers() -> Result<()> {
    let question: UserInputQuestionV1 = serde_json::from_value(serde_json::json!({
        "id": "scope",
        "question": "Which scope should I use?",
        "required": true,
        "options": [
            {"id": "narrow", "label": "Narrow"},
            {"id": "broad", "label": "Broad"}
        ],
        "multiple": false
    }))?;
    assert!(!question.multiple);
    assert_eq!(question.options.len(), 2);
    Ok(())
}

#[test]
fn durable_question_contract_defaults_needed_fields_and_ignores_unknown_fields() {
    let missing_normalized_field =
        serde_json::from_value::<UserInputQuestionV1>(serde_json::json!({
            "id": "scope",
            "question": "Which scope should I use?"
        }))
        .expect("known fields should be enough to project a question");
    assert!(missing_normalized_field.required);
    assert!(missing_normalized_field.options.is_empty());
    assert!(!missing_normalized_field.multiple);

    let unknown_field = serde_json::from_value::<UserInputQuestionV1>(serde_json::json!({
        "id": "scope",
        "question": "Which scope should I use?",
        "header": "Retired header",
        "field": {"kind": "text", "multiline": false},
        "future": true
    }))
    .expect("unknown and retired fields should be ignored");
    assert_eq!(unknown_field.id, "scope");
}

#[test]
fn user_input_nested_enums_ignore_unknown_fields_without_changing_optional_fields() {
    let text = serde_json::from_value::<UserInputAnswerValueV1>(serde_json::json!({
        "kind": "text",
        "value": "kernel",
        "future": true
    }))
    .expect("unknown answer fields should be ignored");
    assert_eq!(
        text,
        UserInputAnswerValueV1::Text {
            value: "kernel".to_owned()
        }
    );

    let submitted = serde_json::from_value::<UserInputDecisionV1>(serde_json::json!({
        "kind": "submitted",
        "answers": [],
        "future": true
    }))
    .expect("unknown decision fields should be ignored");
    assert_eq!(
        submitted,
        UserInputDecisionV1::Submitted { answers: vec![] }
    );

    let single_select = serde_json::from_value::<UserInputAnswerValueV1>(serde_json::json!({
        "kind": "single_select"
    }))
    .expect("optional single-select fields remain optional at decode time");
    assert_eq!(
        single_select,
        UserInputAnswerValueV1::SingleSelect {
            option_id: None,
            other: None,
        }
    );
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

fn agent_user_input_route(request: &UserInputRequestedV1) -> Result<AgentUserInputRouteEntryV1> {
    Ok(AgentUserInputRouteEntryV1 {
        schema_version: AGENT_USER_INPUT_ROUTE_SCHEMA_VERSION,
        route_id: AgentRouteId::new("agent_input_route_1")?,
        source_thread_id: request.request.identity.source_thread_id.clone(),
        source_attempt_id: AgentRunAttemptId::new("attempt_1")?,
        profile_id: AgentProfileId::new("explore")?,
        parent_thread_id: AgentThreadId::new("root")?,
        batch_id: None,
        budget_scope_id: TaskId::new("chat_scope_1")?,
        isolation: TaskIsolationMode::SharedReadOnly,
        child_session_ref: SessionRef::new_relative("children/agents/child.jsonl")?,
        request: UserInputRequestStateV1 {
            requested: request.clone(),
            status: UserInputStatusV1::Requested,
            decision: None,
            claim: None,
            continuation: None,
            resolution: None,
        }
        .public_view(),
        status: AgentRouteStatus::Requested,
        updated_at_unix_ms: 10,
    })
}

#[test]
fn agent_user_input_route_replay_preserves_binding_and_terminal_state() -> Result<()> {
    let request = text_request("child_request", 1)?;
    let mut route = agent_user_input_route(&request)?;
    route.request = UserInputRequestStateV1 {
        requested: request.clone(),
        status: UserInputStatusV1::DecisionAccepted,
        decision: Some(submitted_decision(&request)?),
        claim: None,
        continuation: None,
        resolution: None,
    }
    .public_view();
    let mut projection = AgentUserInputRouteProjectionV1::default();
    projection.apply(route.clone())?;
    assert_eq!(projection.pending().count(), 1);
    assert_eq!(projection.unresolved().count(), 1);

    let mut registered = route.clone();
    registered.status = AgentRouteStatus::Registered;
    registered.updated_at_unix_ms = 20;
    projection.apply(registered.clone())?;
    assert_eq!(projection.pending().count(), 0);
    assert_eq!(projection.unresolved().count(), 1);

    let mut resolved = registered;
    resolved.status = AgentRouteStatus::Resolved;
    resolved.updated_at_unix_ms = 30;
    projection.apply(resolved.clone())?;
    assert_eq!(projection.route(&resolved.route_id), Some(&resolved));
    assert_eq!(projection.unresolved().count(), 0);

    let mut changed_binding = resolved.clone();
    changed_binding.child_session_ref = SessionRef::new_relative("children/agents/other.jsonl")?;
    assert!(projection.apply(changed_binding).is_err());
    let mut reopened = resolved;
    reopened.status = AgentRouteStatus::Requested;
    assert!(projection.apply(reopened).is_err());
    Ok(())
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
fn accepted_answer_reconstructs_private_retry_command_without_public_values() -> Result<()> {
    let mut session = Session::new("test", "model");
    let request = text_request_for_session(&session, "recover_request", 1)?;
    session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(Box::new(
        request.clone(),
    ))])?;
    let command = UserInputDecisionCommandV1 {
        identity: request.request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: UserInputCommandId::new("recover_command")?,
        decision: UserInputDecisionV1::Submitted {
            answers: vec![UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: UserInputAnswerValueV1::Text {
                    value: "restore the exact continuation".to_owned(),
                },
            }],
        },
    };
    accept_user_input_decision(&mut session, command.clone(), 20)?;

    assert_eq!(recoverable_user_input_decision(&session)?, Some(command));
    let public = session
        .user_input_projection()?
        .request(&request.request.identity)
        .expect("request should remain pending")
        .public_view();
    assert!(!serde_json::to_string(&public)?.contains("restore the exact continuation"));
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
                value: UserInputAnswerValueV1::SingleSelect {
                    option_id: Some("narrow".to_owned()),
                    other: None,
                },
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
                    value: "x".repeat(MAX_USER_INPUT_TEXT_CHARS as usize + 1),
                },
            }],
        },
        20,
    );
    assert!(oversized.is_err());

    let mut optional_request = request.request.clone();
    optional_request.questions[0].required = false;
    let optional = UserInputRequestedV1::new(optional_request)?;
    let optional_decision = UserInputDecisionAcceptedV1::new(
        &optional,
        UserInputCommandId::new("optional")?,
        UserInputDecisionV1::Submitted {
            answers: Vec::new(),
        },
        20,
    );
    assert!(optional_decision.is_ok());
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
            question: "Which output format?".to_owned(),
            description: None,
            required: true,
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
            multiple: false,
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
        "physical_1",
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
fn continuation_recovery_releases_an_attempt_that_never_crossed_the_send_barrier() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("test", "model").with_store(store);
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
    let request = text_request_for_session(&session, "recover_before_send", 1)?;
    session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(Box::new(
        request.clone(),
    ))])?;
    accept_user_input_decision(
        &mut session,
        UserInputDecisionCommandV1 {
            identity: request.request.identity.clone(),
            request_hash: request.request_hash.clone(),
            command_id: UserInputCommandId::new("recover_before_send_command")?,
            decision: UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "scope".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "safe retry".to_owned(),
                    },
                }],
            },
        },
        20,
    )?;
    let first = prepare_user_input_continuation(
        &mut session,
        &request.request.identity,
        &request.request_hash,
        "supervisor_1",
        "provider-attempt-before-crash",
        30,
    )?;
    assert_eq!(first.request.status, UserInputStatusV1::ContinuationStarted);
    assert!(recoverable_user_input_decision(&session)?.is_some());

    let released = reconcile_user_input_continuation_after_failed_run(
        &mut session,
        &request.request.identity,
        &request.request_hash,
        35,
    )?;
    assert_eq!(released.status, UserInputStatusV1::ContinuationClaimed);
    assert!(recoverable_user_input_decision(&session)?.is_some());

    let recovered = prepare_user_input_continuation(
        &mut session,
        &request.request.identity,
        &request.request_hash,
        "supervisor_2",
        "provider-attempt-after-restart",
        40,
    )?;
    assert!(!recovered.already_started);
    assert_eq!(
        recovered.continuation.physical_attempt_id,
        "provider-attempt-after-restart"
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::ToolResultV3(result) if result.call_id == "call_1"))
            .count(),
        1
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::UserInputContinuationReleased(released))
            if released.physical_attempt_id == "provider-attempt-before-crash"
                && released.reason
                    == UserInputContinuationReleaseReasonV1::AttemptNotDispatched
    )));
    Ok(())
}

#[test]
fn public_projector_emits_full_request_without_answer_values() -> Result<()> {
    let request = text_request("request_1", 1)?;
    let decision = submitted_decision(&request)?;
    let mut projector = PublicTaskEventProjector::default();
    let requested =
        projector.project_control(&ControlEntry::UserInputRequested(Box::new(request.clone())))?;
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
        projector.project_control(&ControlEntry::UserInputDecisionAccepted(Box::new(decision)))?;
    let serialized = serde_json::to_string(&accepted)?;
    assert!(serialized.contains("decision_accepted"));
    assert!(!serialized.contains("kernel and runtime"));
    assert_eq!(
        PublicConversationPhase::AwaitingUserInput.as_str(),
        "awaiting_user_input"
    );
    Ok(())
}

#[test]
fn public_control_commit_rehydrates_user_input_without_publishing_answer_values() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&crate::ConversationRunStartedEntryV1::new("root_run", 1)?)?;
    let request = text_request_for_session(&session, "request_1", 1)?;
    let decision = submitted_decision(&request)?;
    session.append_controls_with_public_outbox(
        vec![ControlEntry::UserInputRequested(Box::new(request))],
        "root_run",
        1,
    )?;
    drop(session);
    let mut resumed = Session::load_from_store("test", "model", store.clone())?;
    let (_, public) = resumed.append_controls_with_public_outbox(
        vec![ControlEntry::UserInputDecisionAccepted(Box::new(decision))],
        "root_run",
        2,
    )?;
    assert!(matches!(
        &public[0].event.event,
        PublicRunEventKind::UserInputChanged {
            status: UserInputStatusV1::DecisionAccepted,
            ..
        }
    ));
    let projection =
        crate::PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
    let encoded = serde_json::to_string(&projection.events_in_order())?;
    assert!(!encoded.contains("kernel and runtime"));
    assert_eq!(projection.events_in_order().len(), 2);
    Ok(())
}

#[test]
fn invalid_public_control_projection_is_error_not_a_fabricated_run_failure() -> Result<()> {
    let request = text_request("request_1", 1)?;
    let decision = submitted_decision(&request)?;
    assert!(
        PublicTaskEventProjector::default()
            .project_control(&ControlEntry::UserInputDecisionAccepted(Box::new(decision)),)
            .is_err()
    );
    Ok(())
}

#[test]
fn public_input_projector_retires_bodies_without_losing_generation_or_root_budget() -> Result<()> {
    let mut full = UserInputProjectionV1::default();
    let mut compact = PublicUserInputProjector::default();
    for generation in 1..=MAX_USER_INPUT_REQUESTS_PER_ROOT_RUN as u32 {
        let requested = text_request("retired", generation)?;
        let decision = UserInputDecisionAcceptedV1::new(
            &requested,
            UserInputCommandId::new(format!("decision_{generation}"))?,
            UserInputDecisionV1::Declined,
            20,
        )?;
        let resolved = UserInputResolvedV1 {
            schema_version: USER_INPUT_SCHEMA_VERSION,
            identity: requested.request.identity.clone(),
            request_hash: requested.request_hash.clone(),
            resolution: UserInputResolutionV1::Declined,
            resolved_at_unix_ms: 30,
        };
        for entry in [
            UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())),
            UserInputLifecycleEntryV1::DecisionAccepted(Box::new(decision)),
            UserInputLifecycleEntryV1::Resolved(resolved.clone()),
        ] {
            full.apply(entry.clone())?;
            assert_eq!(
                compact.apply(entry)?,
                full.request(&requested.request.identity)
                    .expect("request")
                    .public_view()
            );
        }
        assert!(compact.active.requests.is_empty());
        assert_eq!(compact.retired.len(), generation as usize);
        assert!(
            compact
                .apply(UserInputLifecycleEntryV1::Resolved(resolved.clone()))
                .is_err()
        );
        assert!(
            full.apply(UserInputLifecycleEntryV1::Resolved(resolved))
                .is_err()
        );
        let gap = UserInputLifecycleEntryV1::Requested(Box::new(text_request(
            "retired",
            generation + 2,
        )?));
        assert!(compact.clone().apply(gap.clone()).is_err());
        assert!(full.clone().apply(gap).is_err());
    }
    let exhausted = UserInputLifecycleEntryV1::Requested(Box::new(text_request(
        "retired",
        MAX_USER_INPUT_REQUESTS_PER_ROOT_RUN as u32 + 1,
    )?));
    assert!(compact.apply(exhausted.clone()).is_err());
    assert!(full.apply(exhausted).is_err());
    Ok(())
}

#[test]
fn application_continuation_reopens_with_its_own_causal_batch_without_another_answer() -> Result<()>
{
    use crate::{ApplicationOperationBindingV1, ApplicationOperationTargetV1};
    let dir = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(dir.path().join("continuation.jsonl"))?;
    let mut session = Session::new("test", "model").with_store(store.clone());
    session.ensure_identity_entry()?;
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
    let original = ApplicationOperationBindingV1::new(
        session.session_scope_id().to_owned(),
        "a".repeat(64),
        "b".repeat(64),
        ApplicationOperationTargetV1::UserInputDecision {
            request_id: "request_1".to_owned(),
            generation: 1,
            request_hash: request.request_hash.clone(),
            command_id: "command_1".to_owned(),
        },
    )?;
    session.application_operation_owner()?.prepare(&original)?;
    session.bind_application_operation(original.clone())?;
    accept_user_input_decision(
        &mut session,
        UserInputDecisionCommandV1 {
            identity: request.request.identity.clone(),
            request_hash: request.request_hash.clone(),
            command_id: UserInputCommandId::new("command_1")?,
            decision: UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "scope".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "kernel".to_owned(),
                    },
                }],
            },
        },
        20,
    )?;
    let continuation = ApplicationOperationBindingV1::new(
        session.session_scope_id().to_owned(),
        "c".repeat(64),
        "d".repeat(64),
        ApplicationOperationTargetV1::UserInputContinuation {
            original_operation_id: original.operation_id.clone(),
            request_id: "request_1".to_owned(),
            generation: 1,
            request_hash: request.request_hash.clone(),
        },
    )?;
    session
        .application_operation_owner()?
        .prepare(&continuation)?;
    session.bind_application_operation(continuation.clone())?;
    store.inject_writer_fault(crate::session::SessionWriterFault::PartialSecondRecord)?;
    let _unconfirmed = prepare_user_input_continuation(
        &mut session,
        &request.request.identity,
        &request.request_hash,
        "supervisor",
        "physical_1",
        30,
    );
    drop(session);
    let reopened = Session::load_from_store_for_control(store)?;
    let owner = reopened.application_operation_owner()?;
    assert!(
        crate::session::reconcile_application_operation(&owner.read_handle(), &original)?.is_some()
    );
    assert!(
        crate::session::reconcile_application_operation(&owner.read_handle(), &continuation)?
            .is_some()
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::UserInputContinuationStarted(_))
            ))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn routed_application_answer_and_resume_reopen_only_the_actual_child_batch() -> Result<()> {
    use crate::{ApplicationOperationBindingV1, ApplicationOperationTargetV1};
    for fault in [
        crate::session::SessionWriterFault::PartialFirstRecord,
        crate::session::SessionWriterFault::PartialSecondRecord,
        crate::session::SessionWriterFault::BeforeSync,
    ] {
        let dir = tempfile::tempdir()?;
        let parent_store = JsonlSessionStore::new(dir.path().join("parent.jsonl"))?;
        let mut parent = Session::new("test", "model").with_store(parent_store.clone());
        parent.ensure_identity_entry()?;
        let child_ref = SessionRef::new_relative("children/agents/child.jsonl")?;
        let child_store = JsonlSessionStore::new(child_ref.resolve(dir.path()))?;
        let mut child = Session::new("test", "model").with_store(child_store.clone());
        child.ensure_identity_entry()?;
        let mut assistant = ModelMessage::assistant(
            None,
            vec![ToolCall {
                id: "call_1".to_owned(),
                name: REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                args_json: "{}".to_owned(),
            }],
        );
        assistant.id = "assistant_1".to_owned();
        child.append_assistant_message(assistant)?;
        let request = text_request_for_session(&child, "routed_request", 1)?;
        child.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(Box::new(
            request.clone(),
        ))])?;
        let route = agent_user_input_route(&request)?;
        parent.append_control(ControlEntry::AgentUserInputRoute(route))?;
        let original = ApplicationOperationBindingV1::new(
            parent.session_scope_id().to_owned(),
            "a".repeat(64),
            "b".repeat(64),
            ApplicationOperationTargetV1::UserInputDecision {
                request_id: "routed_request".to_owned(),
                generation: 1,
                request_hash: request.request_hash.clone(),
                command_id: "command_1".to_owned(),
            },
        )?;
        let owner = parent.application_operation_owner()?;
        owner.prepare(&original)?;
        parent.bind_application_operation(original.clone())?;
        drop(child);
        let mut child = parent
            .application_operation_child(&child_ref)?
            .expect("actual delegated child");
        child_store.inject_writer_fault(fault)?;
        let _unconfirmed = accept_user_input_decision(
            &mut child,
            UserInputDecisionCommandV1 {
                identity: request.request.identity.clone(),
                request_hash: request.request_hash.clone(),
                command_id: UserInputCommandId::new("command_1")?,
                decision: UserInputDecisionV1::Submitted {
                    answers: vec![UserInputAnswerV1 {
                        question_id: "scope".to_owned(),
                        value: UserInputAnswerValueV1::Text {
                            value: "kernel".to_owned(),
                        },
                    }],
                },
            },
            20,
        );
        drop(child);
        drop(parent);
        let mut parent = Session::load_from_store_for_control(parent_store.clone())?;
        let owner = parent.application_operation_owner()?;
        // Reopening the actual child owner performs its explicit bundle recovery; query itself
        // must remain observational and cannot take that ownership implicitly.
        let _reopened_child = Session::load_from_store_for_control(child_store.clone())?;
        let proof = owner
            .reconcile(&original)?
            .expect("answer causal proof after child recovery");
        assert_eq!(
            proof.source_session_scope_id(),
            request.request.identity.session_scope_id.as_str()
        );
        assert_ne!(proof.source_session_scope_id(), parent.session_scope_id());
        let child_records = child_store.read_event_records_writer()?;
        let resolved = owner.bind_child_records(&child_records, &original)?;
        let from_records =
            crate::session::reconcile_application_operation_records(&child_records, &resolved)?
                .expect("managed read snapshot uses same proof rules");
        assert_eq!(from_records.event_id(), proof.event_id());
        from_records.validate_binding(&resolved)?;

        let mut drift = original.clone();
        drift.domain_session_scope_id = Some(parent.session_scope_id().to_owned());
        assert!(owner.prepare(&drift).is_err());
        let continuation = ApplicationOperationBindingV1::new(
            parent.session_scope_id().to_owned(),
            "c".repeat(64),
            "d".repeat(64),
            ApplicationOperationTargetV1::UserInputContinuation {
                original_operation_id: original.operation_id.clone(),
                request_id: "routed_request".to_owned(),
                generation: 1,
                request_hash: request.request_hash.clone(),
            },
        )?;
        owner.prepare(&continuation)?;
        parent.bind_application_operation(continuation.clone())?;
        let mut child = parent
            .application_operation_child(&child_ref)?
            .expect("same actual child continuation");
        child_store.inject_writer_fault(fault)?;
        let _unconfirmed = prepare_user_input_continuation(
            &mut child,
            &request.request.identity,
            &request.request_hash,
            "supervisor",
            "physical_1",
            30,
        );
        drop(child);
        drop(parent);
        let parent = Session::load_from_store_for_control(parent_store)?;
        let _reopened_child = Session::load_from_store_for_control(child_store.clone())?;
        assert!(
            parent
                .application_operation_owner()?
                .reconcile(&continuation)?
                .is_some()
        );
        assert!(!parent.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(
                ControlEntry::ApplicationOperationPreparedV1(_)
                    | ControlEntry::ApplicationOperationCommittedV1(_)
            )
        )));
        let child = Session::load_from_store_for_control(child_store)?;
        assert_eq!(
            child
                .entries()
                .iter()
                .filter(|entry| matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(_))
                ))
                .count(),
            1
        );
        assert_eq!(
            child
                .entries()
                .iter()
                .filter(|entry| matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::UserInputContinuationStarted(_))
                ))
                .count(),
            1
        );
    }
    Ok(())
}
