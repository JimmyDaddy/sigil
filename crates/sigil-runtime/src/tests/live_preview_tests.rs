use super::*;
use sigil_kernel::{ModelMessage, ToolExecutionId, ToolProgressEvent};

#[test]
fn durable_publication_count_and_final_bytes_are_independent_of_delta_partition() -> Result<()> {
    use sigil_kernel::{
        EventHandler, JsonlSessionStore, Session, SessionLogEntry, SessionPublicEventProjectionV1,
    };
    let full_text = "x".repeat(100_000);
    let mut counts = Vec::new();
    for chunks in [1, 1_000, 100_000] {
        let temp = tempfile::tempdir()?;
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let mut session = Session::load_from_store("fixture", "model", store.clone())?;
        let mut recorder =
            super::super::ApplicationRunEventRecorder::start(&session, "run", "input")?;
        recorder.begin_live_attempt("actual-fixture-attempt")?;
        for chunk in full_text.as_bytes().chunks(full_text.len() / chunks) {
            recorder.handle(sigil_kernel::RunEvent::TextDelta(String::from_utf8(
                chunk.to_vec(),
            )?))?;
        }
        let preview = recorder.live_preview_source().reader().poll_updates()?;
        assert_eq!(preview.len(), 1);
        assert_eq!(
            preview[0].preview.as_str(),
            &full_text[..MAX_SAFE_TEXT_BYTES]
        );
        let mut message = ModelMessage::assistant(Some(full_text.clone()), Vec::new());
        message.assistant_kind = Some(sigil_kernel::AssistantMessageKind::FinalAnswer);
        let message_id = message.id.clone();
        recorder.commit_session_publications(
            &mut session,
            vec![SessionLogEntry::Assistant(message.clone())],
            vec![SessionPublicEventProjectionV1::assistant_message(
                0, message,
            )],
        )?;
        recorder.finish_output(&sigil_kernel::AgentRunOutput {
            disposition: sigil_kernel::AgentRunDisposition::FinalAnswer,
            result: sigil_kernel::AgentRunResult {
                final_text: full_text.clone(),
                tool_calls: 0,
                final_message_id: Some(message_id),
            },
            outcome: sigil_kernel::AgentRunOutcome::default(),
        })?;
        let records = store.read_event_records_writer()?;
        let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
        let public = outbox.events_in_order();
        assert_eq!(public.len(), 3);
        assert_eq!(
            public
                .iter()
                .map(|entry| entry.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(
            public
                .iter()
                .all(|entry| !sigil_kernel::is_transient_public_run_event(&entry.event.event))
        );
        assert_eq!(
            outbox.pending_for_adapter("tui").len(),
            3,
            "live delivery cannot create receipts"
        );
        let final_bytes = public
            .iter()
            .find_map(|entry| match &entry.event.event {
                PublicRunEventKind::AssistantMessage { message } => message.content.as_deref(),
                _ => None,
            })
            .expect("complete assistant publication");
        assert_eq!(final_bytes.as_bytes(), full_text.as_bytes());
        assert!(recorder.live_preview_source().is_terminal());
        assert!(
            recorder
                .live_preview_source()
                .reader()
                .poll_updates()?
                .is_empty()
        );
        counts.push(records.len());
        eprintln!(
            "durable chunks={chunks} records={} outbox={} final_bytes={}",
            records.len(),
            public.len(),
            final_bytes.len()
        );
    }
    assert!(counts.iter().all(|count| *count == counts[0]));
    Ok(())
}

#[test]
fn preview_partition_count_does_not_change_retained_bytes_or_frame_count() -> Result<()> {
    let text = "x".repeat(100_000);
    for chunks in [1, 1_000, 100_000] {
        let source = RuntimeLivePreviewSource::new("session", "run", false);
        source.begin_attempt("physical-attempt")?;
        let mut reader = source.reader();
        let now = Instant::now();
        let mut frames = 0;
        for chunk in text.as_bytes().chunks(text.len() / chunks) {
            source.apply_delta(
                &PublicRunEventKind::TextDelta {
                    text: String::from_utf8(chunk.to_vec())?,
                },
                3,
            )?;
            frames += reader.poll_at(now)?.len();
        }
        let final_frame = reader.poll_at(now + LIVE_PREVIEW_FRAME_INTERVAL)?;
        let reconnect = source.reader().poll_at(now)?;
        assert_eq!(reconnect.len(), 1);
        assert_eq!(reconnect[0].preview.as_str(), &text[..MAX_SAFE_TEXT_BYTES]);
        assert!(reconnect[0].truncated);
        assert_eq!(reconnect[0].base_durable_sequence, 3);
        assert_eq!(reconnect[0].live_revision, chunks as u64);
        if chunks > 1 {
            assert_eq!(final_frame, reconnect);
        }
        assert_eq!(frames, 1, "no delta queue or per-delta publication");
        let state = source
            .state
            .lock()
            .map_err(|_| anyhow!("test source poisoned"))?;
        assert_eq!(state.slots.len(), 1);
        assert!(
            state
                .slots
                .values()
                .map(|slot| slot.text.capacity())
                .sum::<usize>()
                <= MAX_SAFE_TEXT_BYTES
        );
        eprintln!(
            "live chunks={chunks} slots={} bytes={} frames={}",
            state.slots.len(),
            state
                .slots
                .values()
                .map(|slot| slot.text.len())
                .sum::<usize>(),
            frames + final_frame.len()
        );
    }
    Ok(())
}

#[test]
fn preview_slots_and_utf8_are_bounded_before_copy_and_do_not_admit_suffixes() -> Result<()> {
    let source = RuntimeLivePreviewSource::new("session", "run", false);
    source.begin_attempt("attempt")?;
    for slot in 0..100_000 {
        source.apply_delta(
            &PublicRunEventKind::ToolCallArgsDelta {
                id: format!("call-{slot}"),
                delta: "你".repeat(2),
            },
            1,
        )?;
    }
    for _ in 0..20_000 {
        source.apply_delta(
            &PublicRunEventKind::ToolCallArgsDelta {
                id: "call-0".to_owned(),
                delta: "你你".to_owned(),
            },
            1,
        )?;
    }
    let updates = source.reader().poll_updates()?;
    assert_eq!(updates.len(), MAX_LIVE_PREVIEW_SLOTS);
    assert!(
        updates
            .iter()
            .all(|update| update.preview.as_str().len() <= MAX_SAFE_TEXT_BYTES)
    );
    assert!(updates[0].truncated);
    source.apply_committed(
        &PublicRunEventKind::ToolCallCompleted {
            call: sigil_kernel::ToolCall {
                id: "call-0".to_owned(),
                name: "tool".to_owned(),
                args_json: "{}".to_owned(),
            },
        },
        false,
    );
    source.apply_delta(
        &PublicRunEventKind::ToolCallArgsDelta {
            id: "call-100".to_owned(),
            delta: "late suffix".to_owned(),
        },
        2,
    )?;
    assert!(
        !source
            .reader()
            .poll_updates()?
            .iter()
            .any(|update| update.slot_id == "call-100")
    );
    Ok(())
}

#[test]
fn preview_final_replaces_slot_new_attempt_resets_content_and_terminal_rejects_late_output()
-> Result<()> {
    let source = RuntimeLivePreviewSource::new("session", "run", false);
    source.begin_attempt("attempt-1")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "old".to_owned(),
        },
        1,
    )?;
    source.apply_committed(
        &PublicRunEventKind::AssistantMessage {
            message: ModelMessage::assistant(Some("old full".to_owned()), Vec::new()).into(),
        },
        false,
    );
    assert!(source.reader().poll_updates()?.is_empty());
    source.begin_attempt("attempt-1")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "late same attempt".into(),
        },
        2,
    )?;
    assert!(
        source.reader().poll_updates()?.is_empty(),
        "a newer durable base cannot reopen committed text"
    );
    source.begin_attempt("attempt-2")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "new".to_owned(),
        },
        2,
    )?;
    let current = source.reader().poll_updates()?;
    assert_eq!(current[0].preview.as_str(), "new");
    assert_eq!(current[0].attempt_id, "attempt-2");
    assert_eq!(current[0].live_revision, 2);
    source.apply_committed(
        &PublicRunEventKind::RunFinished {
            final_text: "new full".to_owned(),
        },
        true,
    );
    assert!(source.is_terminal());
    assert!(source.reader().poll_updates()?.is_empty());
    assert!(source.begin_attempt("attempt-3").is_err());
    assert!(
        source
            .apply_delta(
                &PublicRunEventKind::TextDelta {
                    text: "late".to_owned()
                },
                3
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn tool_progress_keeps_latest_state_and_execution_identity() -> Result<()> {
    let source = RuntimeLivePreviewSource::new("session", "run", false);
    source.begin_attempt("attempt")?;
    for sequence in 1..=1_000 {
        source.apply_delta(
            &PublicRunEventKind::ToolProgress {
                progress: ToolProgressEvent {
                    execution_id: ToolExecutionId::new("execution-1")
                        .map_err(anyhow::Error::msg)?,
                    call_id: "call-1".to_owned(),
                    tool_name: "bash".to_owned(),
                    sequence,
                    status: "running".to_owned(),
                    message: None,
                    output_preview: Some(sequence.to_string()),
                    output_log_ref: None,
                    total_bytes: Some(sequence),
                    updated_at_ms: Some(sequence),
                    details: serde_json::Value::Null,
                },
            },
            2,
        )?;
    }
    let updates = source.reader().poll_updates()?;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].preview.as_str(), "1000");
    let progress = updates[0].tool_progress.as_ref().expect("tool metadata");
    assert_eq!(progress.execution_id, "execution-1");
    assert_eq!(progress.call_id, "call-1");
    assert_eq!(progress.total_bytes, Some(1_000));
    source.apply_committed(
        &PublicRunEventKind::ToolResult {
            result: sigil_kernel::ToolResult::ok(
                "call-1",
                "bash",
                "finished",
                sigil_kernel::ToolResultMeta::default(),
            ),
        },
        false,
    );
    source.apply_delta(
        &PublicRunEventKind::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: "late tool output".to_owned(),
        },
        3,
    )?;
    assert!(source.reader().poll_updates()?.is_empty());
    Ok(())
}

#[test]
fn revision_input_wait_retires_previews_without_closing_the_resumable_source() -> Result<()> {
    let source = RuntimeLivePreviewSource::new("session", "run", false);
    source.begin_attempt("before-input")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "partial".to_owned(),
        },
        1,
    )?;
    let awaiting = PublicRunEventKind::RunAwaitingUserInput {
        request_id: "input".to_owned(),
        generation: 1,
        request_hash: "a".repeat(64),
    };
    source.apply_committed(&awaiting, false);
    assert!(!source.is_terminal());
    assert!(source.reader().poll_updates()?.is_empty());
    source.begin_attempt("before-input")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "late before input".into(),
        },
        2,
    )?;
    assert!(
        source.reader().poll_updates()?.is_empty(),
        "Waiting closes the current actual attempt"
    );
    source.begin_attempt("after-input")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "resumed".to_owned(),
        },
        2,
    )?;
    assert_eq!(
        source.reader().poll_updates()?[0].preview.as_str(),
        "resumed"
    );
    source.apply_committed(&awaiting, true);
    assert!(
        source.is_terminal(),
        "root awaiting uses the committed terminal authority"
    );
    Ok(())
}

#[test]
fn restored_terminal_never_admits_a_new_live_attempt() {
    let source = RuntimeLivePreviewSource::new("session", "run", true);
    assert!(source.is_terminal());
    assert!(source.begin_attempt("provider-attempt").is_err());
    assert!(
        source
            .reader()
            .poll_updates()
            .expect("empty preview")
            .is_empty()
    );
}

#[test]
fn completed_tool_arguments_cannot_reopen_but_execution_progress_can() -> Result<()> {
    let source = RuntimeLivePreviewSource::new("session", "run", false);
    source.begin_attempt("attempt")?;
    let args = PublicRunEventKind::ToolCallArgsDelta {
        id: "call".into(),
        delta: "{}".into(),
    };
    source.apply_delta(&args, 1)?;
    source.apply_committed(
        &PublicRunEventKind::ToolCallCompleted {
            call: sigil_kernel::ToolCall {
                id: "call".into(),
                name: "bash".into(),
                args_json: "{}".into(),
            },
        },
        false,
    );
    source.apply_delta(&args, 2)?;
    assert!(source.reader().poll_updates()?.is_empty());
    source.apply_delta(
        &PublicRunEventKind::ToolProgress {
            progress: ToolProgressEvent {
                execution_id: ToolExecutionId::new("execution").map_err(anyhow::Error::msg)?,
                call_id: "call".into(),
                tool_name: "bash".into(),
                sequence: 1,
                status: "running".into(),
                message: Some("executing".into()),
                output_preview: None,
                output_log_ref: None,
                total_bytes: None,
                updated_at_ms: None,
                details: serde_json::Value::Null,
            },
        },
        2,
    )?;
    let update = source.reader().poll_updates()?;
    assert_eq!(update[0].kind, LiveRunUpdateKind::ToolProgress);
    assert_eq!(update[0].preview.as_str(), "executing");
    Ok(())
}

#[test]
fn discarded_attempt_stays_closed_until_a_different_physical_attempt() -> Result<()> {
    let source = RuntimeLivePreviewSource::new("session", "run", false);
    source.begin_attempt("discarded")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "partial".into(),
        },
        1,
    )?;
    source.apply_committed(
        &PublicRunEventKind::ProviderTurnPartialOutputDiscarded {
            output: sigil_kernel::PublicProviderTurnPartialOutputDiscardedViewV1 {
                text_discarded: true,
                reasoning_discarded: false,
                tool_request_discarded: false,
            },
        },
        false,
    );
    source.begin_attempt("discarded")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "late".into(),
        },
        2,
    )?;
    assert!(source.reader().poll_updates()?.is_empty());
    source.begin_attempt("retry")?;
    source.apply_delta(
        &PublicRunEventKind::TextDelta {
            text: "recovered".into(),
        },
        2,
    )?;
    assert_eq!(
        source.reader().poll_updates()?[0].preview.as_str(),
        "recovered"
    );
    Ok(())
}
