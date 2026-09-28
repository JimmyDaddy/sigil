use super::*;

#[test]
fn ordinary_protocol_failure_does_not_request_authentication() {
    let error = protocol_error(anyhow!("execution owner failed"));
    assert_eq!(error.code, acp::ErrorCode::InternalError);
    assert_eq!(error.message, "execution owner failed");
}

#[test]
fn text_prompt_preserves_block_order_and_rejects_unsupported_content() {
    let prompt = prompt_text(vec![
        acp::ContentBlock::Text(acp::TextContent::new("first")),
        acp::ContentBlock::Text(acp::TextContent::new("second")),
    ])
    .expect("text prompt");
    assert_eq!(prompt, "first\nsecond");
    assert!(prompt_text(Vec::new()).is_err());
}

#[tokio::test]
async fn disconnect_joins_owned_work_and_closes_admission() {
    let adapter = Adapter::new(PathBuf::from("unused.toml"));
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    adapter
        .spawn(async move {
            let _ = entered.send(());
            let _ = gate.await;
            Ok(())
        })
        .expect("owned task");
    wait.await.expect("entered");
    let shutdown = adapter.shutdown();
    tokio::pin!(shutdown);
    assert!(futures::poll!(&mut shutdown).is_pending());
    assert!(adapter.spawn(async { Ok(()) }).is_err());
    release.send(()).expect("release owned task");
    shutdown.await.expect("joined");
}

#[test]
fn confirmed_cancellation_overrides_provider_abort_but_not_unconfirmed_cleanup() {
    use sigil_kernel::RunCancellationTerminalOutcome as Outcome;
    let reason = settled_stop_reason(
        Err(anyhow!("provider wait aborted")),
        Ok(Some(Outcome::Cancelled)),
    )
    .expect("durably confirmed cancellation");
    assert_eq!(reason, acp::StopReason::Cancelled);
    let interrupted = settled_stop_reason(
        Ok(ApplicationRunTerminalStatus::Interrupted),
        Ok(Some(Outcome::Cancelled)),
    )
    .map(acp::PromptResponse::new);
    assert!(
        prompt_settlement(&interrupted).is_err(),
        "interrupted execution is never hidden"
    );
    for cancellation in [
        Ok(Some(Outcome::Interrupted)),
        Err(anyhow!("cancellation audit failed")),
    ] {
        let result = settled_stop_reason(Err(anyhow!("provider wait aborted")), cancellation)
            .map(acp::PromptResponse::new);
        assert!(result.is_err());
        assert!(prompt_settlement(&result).is_err());
    }
}

#[test]
fn ordinary_failed_run_is_a_prompt_error_and_does_not_poison_disconnect() {
    for execution in [
        Err(anyhow!("provider unavailable")),
        Ok(ApplicationRunTerminalStatus::Failed),
    ] {
        let result = settled_stop_reason(execution, Ok(None)).map(acp::PromptResponse::new);
        assert!(result.is_err());
        assert!(prompt_settlement(&result).is_ok());
    }
    let interrupted = settled_stop_reason(Ok(ApplicationRunTerminalStatus::Interrupted), Ok(None))
        .map(acp::PromptResponse::new);
    assert!(prompt_settlement(&interrupted).is_err());
    assert_eq!(
        settled_stop_reason(Ok(ApplicationRunTerminalStatus::Succeeded), Ok(None))
            .expect("successful run"),
        acp::StopReason::EndTurn
    );
}

#[tokio::test]
async fn disconnect_retains_foreground_failure_after_reply_is_dropped_and_task_is_reaped() {
    let adapter = Adapter::new(PathBuf::from("unused.toml"));
    let (reply, receiver) = tokio::sync::oneshot::channel::<Result<acp::PromptResponse>>();
    drop(receiver);
    adapter
        .spawn(async move {
            let result = settled_stop_reason(
                Err(anyhow!("provider wait aborted")),
                Ok(Some(
                    sigil_kernel::RunCancellationTerminalOutcome::Interrupted,
                )),
            )
            .map(acp::PromptResponse::new);
            let settlement = prompt_settlement(&result);
            assert!(reply.send(result).is_err(), "peer already disconnected");
            settlement
        })
        .expect("owned foreground");
    // Wait for the actual retained task to finish; the next admission exercises its reaper.
    loop {
        if adapter
            .tasks
            .lock()
            .expect("tasks")
            .iter()
            .all(tokio::task::JoinHandle::is_finished)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (entered, observed) = tokio::sync::oneshot::channel();
    adapter
        .spawn(async move {
            let _ = entered.send(());
            Ok(())
        })
        .expect("later independent request");
    observed
        .await
        .expect("later request was not gated by previous failure");
    let error = adapter
        .shutdown()
        .await
        .expect_err("cleanup failure survives responder and reaper");
    assert!(format!("{error:#}").contains("cancellation cleanup could not be confirmed"));
}

#[test]
fn resource_link_is_structured_reference_without_reading_or_granting_access() {
    let reference =
        acp::ResourceLink::new("quoted \"resource\"", "file:///unavailable/acp-reference")
            .description("client description")
            .mime_type("text/plain")
            .size(17);
    let prompt = prompt_text(vec![
        acp::ContentBlock::Text(acp::TextContent::new("before")),
        acp::ContentBlock::ResourceLink(reference),
        acp::ContentBlock::Text(acp::TextContent::new("after")),
    ])
    .expect("baseline resource reference is accepted without filesystem access");
    let lines = prompt.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], "before");
    assert!(lines[1].contains("not retrieved"));
    let resource: serde_json::Value = serde_json::from_str(lines[2]).expect("structured reference");
    assert_eq!(resource["uri"], "file:///unavailable/acp-reference");
    assert_eq!(resource["name"], "quoted \"resource\"");
    assert_eq!(resource["size"], 17);
    assert_eq!(lines[3], "after");
    assert!(
        prompt_text(vec![acp::ContentBlock::ResourceLink(
            acp::ResourceLink::new("remote", "https://invalid.invalid/not-fetched")
        )])
        .is_ok()
    );
    assert!(
        prompt_text(vec![acp::ContentBlock::Image(acp::ImageContent::new(
            "",
            "image/png"
        ))])
        .is_err()
    );
}
