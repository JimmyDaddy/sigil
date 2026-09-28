use super::*;

#[test]
fn append_projection_accepts_only_suffixes_and_keeps_unicode_boundaries() {
    let mut sent = String::new();
    assert_eq!(TextProjection::suffix(&mut sent, "中"), Some("中"));
    assert_eq!(TextProjection::suffix(&mut sent, "中文"), Some("文"));
    assert_eq!(TextProjection::suffix(&mut sent, "中文"), None);
    assert_eq!(TextProjection::suffix(&mut sent, "replacement"), None);
    assert_eq!(sent, "中文");
}

fn preview(
    attempt: &str,
    content: &str,
    kind: LiveRunUpdateKind,
) -> sigil_application::LiveRunUpdate {
    sigil_application::LiveRunUpdate {
        schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
        session_id: "session".to_owned(),
        run_id: "run".to_owned(),
        attempt_id: Some(attempt.to_owned()),
        slot_id: "content".to_owned(),
        live_revision: 1,
        base_durable_sequence: 0,
        kind,
        preview: sigil_application::SafeText::new(content).expect("bounded preview"),
        tool_progress: None,
        truncated: false,
    }
}

fn assistant(content: &str, reasoning: bool) -> PublicRunEvent {
    PublicRunEvent::new(
        "session",
        "run",
        2,
        PublicRunEventKind::AssistantMessage {
            message: sigil_kernel::PublicAssistantMessage {
                id: uuid::Uuid::new_v4().to_string(),
                content: Some(content.to_owned()),
                tool_calls: Vec::new(),
                assistant_kind: reasoning
                    .then_some(sigil_kernel::AssistantMessageKind::ReasoningTrace),
            },
        },
    )
}

/// The actual SDK delivers serialized session notifications to a client handler; assertions
/// below inspect those received notifications rather than an internal projection snapshot.
async fn received_updates(
    scenario: impl FnOnce(&mut EventProjection) -> Result<()> + Send + 'static,
) -> Vec<acp::SessionUpdate> {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let client = agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _connection| {
                sender
                    .send(notification.update)
                    .map_err(agent_client_protocol::Error::into_internal_error)
            },
            agent_client_protocol::on_receive_notification!(),
        );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent_client_protocol::Agent
            .builder()
            .connect_with(client, async move |connection| {
                let mut projection = EventProjection::new(
                    connection,
                    "public-session".to_owned(),
                    "session".to_owned(),
                    "run".to_owned(),
                );
                scenario(&mut projection).map_err(crate::acp::protocol_error)?;
                projection
                    .notice("fixture-end")
                    .map_err(crate::acp::protocol_error)?;
                let mut received = Vec::new();
                while let Some(update) = receiver.recv().await {
                    if let acp::SessionUpdate::AgentMessageChunk(chunk) = &update
                        && let acp::ContentBlock::Text(text) = &chunk.content
                        && text.text == "fixture-end"
                    {
                        return Ok(received);
                    }
                    received.push(update);
                }
                Err(acp::Error::new(
                    -32000,
                    "client notification channel closed",
                ))
            }),
    )
    .await
    .expect("SDK delivery deadline")
    .expect("SDK delivery")
}

fn message_chunk(update: &acp::SessionUpdate) -> (&str, &str) {
    let acp::SessionUpdate::AgentMessageChunk(chunk) = update else {
        panic!("expected assistant message chunk: {update:?}");
    };
    let acp::ContentBlock::Text(text) = &chunk.content else {
        panic!("expected text content");
    };
    (
        chunk
            .message_id
            .as_ref()
            .expect("message identity")
            .0
            .as_ref(),
        &text.text,
    )
}

#[tokio::test]
async fn sdk_retry_discard_separates_attempts_and_rejects_late_preview() {
    let updates = received_updates(|projection| {
        projection.apply_preview(preview("attempt-a", "wrong", LiveRunUpdateKind::Text))?;
        projection.event(PublicRunEvent::new(
            "session",
            "run",
            1,
            PublicRunEventKind::ProviderTurnPartialOutputDiscarded {
                output: sigil_kernel::PublicProviderTurnPartialOutputDiscardedViewV1 {
                    text_discarded: true,
                    reasoning_discarded: false,
                    tool_request_discarded: false,
                },
            },
        ))?;
        projection.apply_preview(preview("attempt-a", "wrong late", LiveRunUpdateKind::Text))?;
        projection.apply_preview(preview("attempt-b", "right", LiveRunUpdateKind::Text))?;
        projection.event(assistant("right answer", false))?;
        projection.apply_preview(preview(
            "attempt-b",
            "right answer late",
            LiveRunUpdateKind::Text,
        ))?;
        projection.event(PublicRunEvent::new(
            "session",
            "run",
            3,
            PublicRunEventKind::RunFinished {
                final_text: "right answer".to_owned(),
            },
        ))
    })
    .await;
    assert_eq!(updates.len(), 4);
    let (old_id, old) = message_chunk(&updates[0]);
    let (notice_id, notice) = message_chunk(&updates[1]);
    let (new_id, new) = message_chunk(&updates[2]);
    let (suffix_id, suffix) = message_chunk(&updates[3]);
    assert_eq!(old, "wrong");
    assert!(notice.contains("discarded"));
    assert_ne!(old_id, new_id);
    assert_ne!(notice_id, new_id);
    assert_eq!(new_id, suffix_id);
    assert_eq!(format!("{new}{suffix}"), "right answer");
}

#[tokio::test]
async fn sdk_normal_stream_preserves_reasoning_and_durable_unicode_suffix_once() {
    let updates = received_updates(|projection| {
        projection.apply_preview(preview("attempt", "考", LiveRunUpdateKind::Reasoning))?;
        projection.event(assistant("考虑", true))?;
        projection.apply_preview(preview("attempt", "中", LiveRunUpdateKind::Text))?;
        projection.event(assistant("中文", false))?;
        projection.event(PublicRunEvent::new(
            "session",
            "run",
            3,
            PublicRunEventKind::RunFinished {
                final_text: "中文".to_owned(),
            },
        ))
    })
    .await;
    assert_eq!(updates.len(), 4);
    let (acp::SessionUpdate::AgentThoughtChunk(first), acp::SessionUpdate::AgentThoughtChunk(last)) =
        (&updates[0], &updates[1])
    else {
        panic!("reasoning stays thought content")
    };
    assert_eq!(first.message_id, last.message_id);
    let (message_id, text) = message_chunk(&updates[2]);
    let (suffix_id, suffix) = message_chunk(&updates[3]);
    assert_eq!(message_id, suffix_id);
    assert_ne!(
        first
            .message_id
            .as_ref()
            .expect("thought identity")
            .0
            .as_ref(),
        message_id
    );
    assert_eq!(format!("{text}{suffix}"), "中文");
}

#[tokio::test]
async fn sdk_durable_replacement_without_discard_never_splices_into_provisional_message() {
    let updates = received_updates(|projection| {
        projection.apply_preview(preview("attempt", "provisional", LiveRunUpdateKind::Text))?;
        projection.event(assistant("replacement", false))
    })
    .await;
    assert_eq!(updates.len(), 2);
    assert_ne!(message_chunk(&updates[0]).0, message_chunk(&updates[1]).0);
    assert_eq!(message_chunk(&updates[1]).1, "replacement");
}
