use super::*;

#[derive(Default)]
struct Handler;

impl EventHandler for Handler {
    fn handle(&mut self, _event: crate::RunEvent) -> Result<()> {
        Ok(())
    }
}

fn call(group: bool) -> ToolCall {
    ToolCall {
        id: "origin-call".to_owned(),
        name: crate::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        args_json: if group {
            r#"{"questions":[{"id":"scope","question":"保留哪个范围？"},{"id":"note","question":"还有哪些限制？"}]}"#
        } else {
            r#"{"questions":[{"id":"scope","question":"Please answer the following questions before continuing."}]}"#
        }.to_owned(),
    }
}

#[test]
fn prompt_origin_uses_question_structure_without_matching_question_words() -> Result<()> {
    for (group, expected) in [
        (false, UserInputPromptOriginV1::QuestionText),
        (true, UserInputPromptOriginV1::HostQuestionGroup),
    ] {
        let mut session = Session::new("fixture", "model");
        let call = call(group);
        session
            .append_assistant_message(crate::ModelMessage::assistant(None, vec![call.clone()]))?;
        let thread = AgentThreadId::new("main")?;
        let reference = handle_request_user_input_call(
            &mut session,
            &mut Handler,
            &call,
            RequestUserInputContext {
                logical_run_id: "origin-root",
                root_logical_run_id: "origin-root",
                source_thread_id: &thread,
                provider_name: "fixture",
                model_name: "model",
                source: UserInputSourceV1::Agent,
            },
        )?;
        let projection = session.user_input_projection()?;
        let state = projection
            .request(&reference.identity)
            .expect("pending request");
        assert_eq!(state.requested.request.prompt_origin, Some(expected));
        assert_eq!(state.public_view().prompt_origin, Some(expected));
    }
    Ok(())
}

#[test]
fn prompt_origin_replay_retains_historical_absence_and_frozen_current_origin() -> Result<()> {
    for origin in [None, Some(UserInputPromptOriginV1::HostQuestionGroup)] {
        let mut session = Session::new("fixture", "model");
        let call = call(true);
        let assistant = crate::ModelMessage::assistant(None, vec![call.clone()]);
        let message_id = assistant.id.clone();
        session.append_assistant_message(assistant)?;
        let thread = AgentThreadId::new("main")?;
        let root = LogicalRunId::new("origin-root")?;
        let args = parse_request_user_input_args(&call.args_json)?;
        let requested = UserInputRequestedV1::new(UserInputRequestV1 {
            schema_version: USER_INPUT_SCHEMA_VERSION,
            identity: UserInputIdentityV1 {
                session_scope_id: crate::SessionScopeId::new(session.session_scope_id())?,
                request_id: request_id_for_call(
                    session.session_scope_id(),
                    &root,
                    &thread,
                    &call.id,
                )?,
                source_binding_hash: source_binding_hash(
                    session.session_scope_id(),
                    &root,
                    &thread,
                    &message_id,
                    &call,
                )?,
                root_logical_run_id: root,
                source_thread_id: thread.clone(),
                generation: 1,
            },
            source: UserInputSourceV1::Agent,
            purpose: UserInputPurposeV1::Clarification,
            prompt: args.prompt,
            prompt_origin: origin,
            questions: args.questions,
            allowed_actions: vec![
                UserInputActionV1::Submit,
                UserInputActionV1::Decline,
                UserInputActionV1::CancelRun,
            ],
            requested_at_unix_ms: 7,
            continuation: Some(UserInputContinuationBindingV1 {
                assistant_message_id: message_id,
                tool_call_id: call.id.clone(),
                provider_name: "fixture".to_owned(),
                model_name: "model".to_owned(),
            }),
        })?;
        session.append_user_input_lifecycle(vec![crate::UserInputLifecycleEntryV1::Requested(
            Box::new(requested.clone()),
        )])?;
        let before = session.entries().len();
        let reference = handle_request_user_input_call(
            &mut session,
            &mut Handler,
            &call,
            RequestUserInputContext {
                logical_run_id: "origin-root",
                root_logical_run_id: "origin-root",
                source_thread_id: &thread,
                provider_name: "fixture",
                model_name: "model",
                source: UserInputSourceV1::Agent,
            },
        )?;
        assert_eq!(reference.request_hash, requested.request_hash);
        assert_eq!(
            session.entries().len(),
            before,
            "replay does not rewrite the request"
        );
        let projection = session.user_input_projection()?;
        assert_eq!(
            projection
                .request(&reference.identity)
                .expect("same request")
                .requested,
            requested
        );
    }
    Ok(())
}
