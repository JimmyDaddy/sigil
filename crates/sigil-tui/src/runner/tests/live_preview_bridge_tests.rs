use super::*;

#[test]
fn native_delta_partition_count_does_not_grow_worker_queue() -> Result<()> {
    for chunks in [1, 1_000, 100_000] {
        let temp = tempfile::tempdir()?;
        let store = sigil_kernel::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let mut session = Session::load_from_store("fixture", "model", store)?;
        let (sender, receiver) = mpsc::channel();
        let mut handler = ChannelEventHandler::new(sender);
        handler.start_public_run(&session, "run", "input")?;
        handler.begin_live_attempt("physical-attempt")?;
        let text = "x".repeat(100_000);
        for chunk in text.as_bytes().chunks(text.len() / chunks) {
            handler.handle(RunEvent::TextDelta(String::from_utf8(chunk.to_vec())?))?;
        }
        let messages = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(
            messages.len(),
            2,
            "only source and initial durable frontier are queued"
        );
        let source = messages
            .into_iter()
            .find_map(|message| match message {
                WorkerMessage::LivePreviewSource { source } => Some(source),
                _ => None,
            })
            .expect("source attachment");
        let updates = source.reader().poll_updates()?;
        assert_eq!(updates.len(), 1);
        assert_eq!(
            updates[0].preview.as_str(),
            &text[..sigil_application::MAX_SAFE_TEXT_BYTES]
        );
        let mut message = sigil_kernel::ModelMessage::assistant(Some(text.clone()), Vec::new());
        message.assistant_kind = Some(sigil_kernel::AssistantMessageKind::FinalAnswer);
        let id = message.id.clone();
        handler.commit_session_publications(
            &mut session,
            vec![SessionLogEntry::Assistant(message.clone())],
            vec![SessionPublicEventProjectionV1::assistant_message(
                0, message,
            )],
        )?;
        let delivered = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(delivered.len(), 2);
        assert!(
            matches!(&delivered[0], WorkerMessage::Event(event) if matches!(event.as_ref(), RunEvent::AssistantMessage(message) if message.content.as_deref() == Some(text.as_str())))
        );
        handler.finish_public_run(&Ok(sigil_kernel::AgentRunOutput {
            disposition: sigil_kernel::AgentRunDisposition::FinalAnswer,
            result: sigil_kernel::AgentRunResult {
                final_text: text,
                tool_calls: 0,
                final_message_id: Some(id),
            },
            outcome: sigil_kernel::AgentRunOutcome::default(),
        }))?;
        assert!(source.is_terminal());
        eprintln!(
            "tui delta_count={chunks} queued_live_messages=2 queued_final_messages={}",
            delivered.len()
        );
    }
    Ok(())
}
