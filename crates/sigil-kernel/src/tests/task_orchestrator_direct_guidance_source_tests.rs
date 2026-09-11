use super::*;

#[tokio::test]
async fn direct_guidance_rejects_foreign_source_before_dispatch() -> Result<()> {
    let (_temp, mut session, request, options) = direct_completion_fixture()?;
    let runner = CapturingDirectContinuationRunner::default();
    let captured = Arc::clone(&runner.inputs);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let source = crate::ConversationTurnRef::new("foreign-session", "guidance", "real-run")?;
    let before = serde_json::to_value(session.entries())?;
    let error = orchestrator
        .continue_direct_run(
            &mut session,
            request,
            options,
            Some(("A new user constraint", &source)),
            &mut RecordingEventHandler::default(),
            &mut AutoApproveHandler,
        )
        .await
        .expect_err("foreign source guidance must not reset execution progress");
    assert!(error.to_string().contains("different session"));
    assert!(captured.lock().expect("captured inputs").is_empty());
    assert_eq!(serde_json::to_value(session.entries())?, before);
    Ok(())
}

#[tokio::test]
async fn direct_guidance_is_not_accepted_during_unsettled_attempt_recovery() -> Result<()> {
    let (_temp, mut session, request, options) = direct_completion_fixture()?;
    let admission = session
        .task_state_projection()
        .tasks
        .get(&request.task_id)
        .and_then(|task| task.direct_execution_admission.clone())
        .expect("Direct admission");
    session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(
        crate::TaskDirectExecutionAttemptV1::started(&admission, 1),
    ))?;
    let source = crate::ConversationTurnRef::new(
        session.session_scope_id(),
        "new-guidance-user",
        "real-root-run",
    )?;
    let runner = CapturingDirectContinuationRunner::default();
    let captured = Arc::clone(&runner.inputs);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let before = serde_json::to_value(session.entries())?;
    let error = orchestrator
        .continue_direct_run(
            &mut session,
            request,
            options,
            Some(("A new user constraint", &source)),
            &mut RecordingEventHandler::default(),
            &mut AutoApproveHandler,
        )
        .await
        .expect_err("unsettled attempt retains exact recovery authority");
    assert!(error.to_string().contains("unsettled attempt"));
    assert!(captured.lock().expect("captured inputs").is_empty());
    assert_eq!(serde_json::to_value(session.entries())?, before);
    Ok(())
}
