use super::*;

fn request_at_stage(
    stage: sigil_kernel::UserInputStatusV1,
) -> anyhow::Result<(
    tempfile::TempDir,
    Session,
    sigil_kernel::UserInputContinuationStartedV1,
)> {
    let temp = tempfile::tempdir()?;
    let mut session = Session::load_from_store(
        "fixture",
        "model",
        JsonlSessionStore::new(temp.path().join("settlement.jsonl"))?,
    )?;
    let assistant = ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "settlement-call".to_owned(),
            name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            args_json: r#"{"questions":[{"id":"scope","question":"Which scope?"}]}"#.to_owned(),
        }],
    );
    let assistant_message_id = assistant.id.clone();
    session.append_assistant_message(assistant)?;
    let identity = sigil_kernel::UserInputIdentityV1 {
        session_scope_id: sigil_kernel::SessionScopeId::new(session.session_scope_id())?,
        root_logical_run_id: sigil_kernel::LogicalRunId::new("settlement-root")?,
        source_thread_id: AgentThreadId::new("main")?,
        request_id: sigil_kernel::UserInputRequestId::new("settlement-question")?,
        generation: 1,
        source_binding_hash: format!("sha256:{}", "a".repeat(64)),
    };
    let requested = sigil_kernel::UserInputRequestedV1::new(sigil_kernel::UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: identity.clone(),
        source: sigil_kernel::UserInputSourceV1::Agent,
        purpose: sigil_kernel::UserInputPurposeV1::Clarification,
        prompt: "Which scope?".to_owned(),
        prompt_origin: None,
        questions: vec![sigil_kernel::UserInputQuestionV1 {
            id: "scope".to_owned(),
            question: "Which scope?".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
        }],
        allowed_actions: vec![sigil_kernel::UserInputActionV1::Submit],
        requested_at_unix_ms: 1,
        continuation: Some(sigil_kernel::UserInputContinuationBindingV1 {
            assistant_message_id,
            tool_call_id: "settlement-call".to_owned(),
            provider_name: "fixture".to_owned(),
            model_name: "model".to_owned(),
        }),
    })?;
    session.append_user_input_lifecycle(vec![
        sigil_kernel::UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())),
    ])?;
    if stage != sigil_kernel::UserInputStatusV1::Requested {
        sigil_kernel::accept_user_input_decision(
            &mut session,
            sigil_kernel::UserInputDecisionCommandV1 {
                identity: identity.clone(),
                request_hash: requested.request_hash.clone(),
                command_id: sigil_kernel::UserInputCommandId::new("settlement-answer")?,
                decision: sigil_kernel::UserInputDecisionV1::Submitted {
                    answers: vec![sigil_kernel::UserInputAnswerV1 {
                        question_id: "scope".to_owned(),
                        value: sigil_kernel::UserInputAnswerValueV1::Text {
                            value: "narrow".to_owned(),
                        },
                    }],
                },
            },
            2,
        )?;
    }
    if stage != sigil_kernel::UserInputStatusV1::ContinuationStarted {
        let claim_id = sigil_kernel::UserInputClaimId::new("fixture-claim")?;
        if stage == sigil_kernel::UserInputStatusV1::ContinuationClaimed {
            session.append_user_input_lifecycle(vec![
                sigil_kernel::UserInputLifecycleEntryV1::ContinuationClaimed(
                    sigil_kernel::UserInputContinuationClaimedV1 {
                        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                        identity: identity.clone(),
                        request_hash: requested.request_hash.clone(),
                        claim_id: claim_id.clone(),
                        supervisor_instance_id: "sigil-tui".to_owned(),
                        claimed_at_unix_ms: 3,
                    },
                ),
            ])?;
        }
        return Ok((
            temp,
            session,
            sigil_kernel::UserInputContinuationStartedV1 {
                schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                continuation_logical_run_id: sigil_kernel::user_input_continuation_logical_run_id(
                    &identity,
                    &requested.request_hash,
                )?,
                identity,
                request_hash: requested.request_hash,
                claim_id,
                physical_attempt_id: "settlement-physical".to_owned(),
                started_at_unix_ms: 3,
            },
        ));
    }
    let preparation = sigil_kernel::prepare_user_input_continuation(
        &mut session,
        &identity,
        &requested.request_hash,
        "sigil-tui",
        "settlement-physical",
        3,
    )?;
    Ok((temp, session, preparation.continuation))
}

#[test]
fn tui_user_input_settlement_exact_consumed_attempt_is_read_only() -> anyhow::Result<()> {
    let (temp, mut session, continuation) =
        request_at_stage(sigil_kernel::UserInputStatusV1::ContinuationStarted)?;
    assert!(settle_tui_user_input_continuation(
        &mut session,
        &continuation,
        sigil_kernel::UserInputResolutionV1::Consumed,
    )?);
    let before = std::fs::read(temp.path().join("settlement.jsonl"))?;
    assert!(!settle_tui_user_input_continuation(
        &mut session,
        &continuation,
        sigil_kernel::UserInputResolutionV1::Consumed,
    )?);
    // A later failure remains a failed run, but cannot overwrite or republish this receipt.
    assert!(!settle_tui_user_input_continuation(
        &mut session,
        &continuation,
        sigil_kernel::UserInputResolutionV1::Failed {
            failure_class: "later_tool_failed".to_owned(),
            retryable: false,
        },
    )?);
    for mismatch in ["identity", "hash", "physical", "logical", "claim"] {
        let mut wrong = continuation.clone();
        match mismatch {
            "identity" => wrong.identity.generation += 1,
            "hash" => wrong.request_hash = format!("sha256:{}", "b".repeat(64)),
            "physical" => wrong.physical_attempt_id = "different-physical".to_owned(),
            "logical" => {
                wrong.continuation_logical_run_id =
                    sigil_kernel::LogicalRunId::new("different-logical")?;
            }
            "claim" => wrong.claim_id = sigil_kernel::UserInputClaimId::new("different-claim")?,
            _ => unreachable!("closed fixture matrix"),
        }
        assert!(
            settle_tui_user_input_continuation(
                &mut session,
                &wrong,
                sigil_kernel::UserInputResolutionV1::Consumed,
            )
            .is_err(),
            "{mismatch}"
        );
    }
    for resolution in [
        sigil_kernel::UserInputResolutionV1::Declined,
        sigil_kernel::UserInputResolutionV1::RunCancelled,
    ] {
        assert!(
            settle_tui_user_input_continuation(&mut session, &continuation, resolution,).is_err()
        );
    }
    assert_eq!(std::fs::read(temp.path().join("settlement.jsonl"))?, before);
    Ok(())
}

#[test]
fn tui_user_input_settlement_never_reopens_a_failed_attempt() -> anyhow::Result<()> {
    let (temp, mut session, continuation) =
        request_at_stage(sigil_kernel::UserInputStatusV1::ContinuationStarted)?;
    session.append_user_input_lifecycle(vec![
        sigil_kernel::UserInputLifecycleEntryV1::Resolved(sigil_kernel::UserInputResolvedV1 {
            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
            identity: continuation.identity.clone(),
            request_hash: continuation.request_hash.clone(),
            resolution: sigil_kernel::UserInputResolutionV1::Failed {
                failure_class: "provider_output_uncertain".to_owned(),
                retryable: false,
            },
            resolved_at_unix_ms: 4,
        }),
    ])?;
    let before = std::fs::read(temp.path().join("settlement.jsonl"))?;
    assert!(
        settle_tui_user_input_continuation(
            &mut session,
            &continuation,
            sigil_kernel::UserInputResolutionV1::Consumed,
        )
        .is_err()
    );
    assert_eq!(std::fs::read(temp.path().join("settlement.jsonl"))?, before);
    Ok(())
}

#[test]
fn tui_user_input_settlement_cannot_consume_an_unstarted_request() -> anyhow::Result<()> {
    for stage in [
        sigil_kernel::UserInputStatusV1::Requested,
        sigil_kernel::UserInputStatusV1::DecisionAccepted,
        sigil_kernel::UserInputStatusV1::ContinuationClaimed,
    ] {
        let (temp, mut session, continuation) = request_at_stage(stage)?;
        let before = std::fs::read(temp.path().join("settlement.jsonl"))?;
        assert!(
            settle_tui_user_input_continuation(
                &mut session,
                &continuation,
                sigil_kernel::UserInputResolutionV1::Consumed,
            )
            .is_err(),
            "{stage:?}"
        );
        assert_eq!(std::fs::read(temp.path().join("settlement.jsonl"))?, before);
        assert_eq!(
            session
                .user_input_projection()?
                .request(&continuation.identity)
                .context("original unstarted request")?
                .status,
            stage,
        );
    }
    Ok(())
}

#[test]
fn tui_user_input_settlement_preserves_typed_publication_failure_without_more_appends()
-> anyhow::Result<()> {
    let (temp, mut session, continuation) =
        request_at_stage(sigil_kernel::UserInputStatusV1::ContinuationStarted)?;
    let (sender, _receiver) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(sender);
    handler.start_public_run(
        &session,
        continuation.continuation_logical_run_id.as_str(),
        "",
    )?;
    let mut next = session
        .user_input_projection()?
        .request(&continuation.identity)
        .context("original request")?
        .requested
        .request
        .clone();
    next.identity.request_id = sigil_kernel::UserInputRequestId::new("overlapping-question")?;
    let next = sigil_kernel::UserInputRequestedV1::new(next)?;
    let error = sigil_kernel::EventHandler::commit_controls(
        &mut handler,
        &mut session,
        vec![ControlEntry::UserInputRequested(Box::new(next))],
    )
    .expect_err("the real public bridge must reject an overlapping request");
    let output = Err(error.context("typed request-publication context"));
    assert!(
        output
            .as_ref()
            .is_err_and(sigil_runtime::application_run::is_application_public_outbox_append_error)
    );
    let before = std::fs::read(temp.path().join("settlement.jsonl"))?;
    assert!(!settle_tui_user_input_continuation_after_output(
        &mut session,
        &continuation,
        &output,
    )?);
    handler.finish_public_run(&output)?;
    assert_eq!(std::fs::read(temp.path().join("settlement.jsonl"))?, before);
    assert_eq!(
        session
            .user_input_projection()?
            .request(&continuation.identity)
            .context("unchanged request")?
            .status,
        sigil_kernel::UserInputStatusV1::ContinuationStarted,
        "an uncertain publication cannot release or consume the answer during tail settlement"
    );
    Ok(())
}

#[test]
fn tui_user_input_settlement_ordinary_failed_run_keeps_the_existing_retry_boundary()
-> anyhow::Result<()> {
    let (_temp, mut session, continuation) =
        request_at_stage(sigil_kernel::UserInputStatusV1::ContinuationStarted)?;
    assert!(settle_tui_user_input_continuation_after_output(
        &mut session,
        &continuation,
        &Err(anyhow::anyhow!("provider failed before dispatch")),
    )?);
    assert_eq!(
        session
            .user_input_projection()?
            .request(&continuation.identity)
            .context("released request")?
            .status,
        sigil_kernel::UserInputStatusV1::ContinuationClaimed,
    );
    Ok(())
}
