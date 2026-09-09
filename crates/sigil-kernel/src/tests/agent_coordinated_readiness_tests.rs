use super::*;
use std::{sync::mpsc, thread};

struct FinalAnswerAckHandler {
    store: JsonlSessionStore,
    writer: Option<thread::JoinHandle<Result<bool>>>,
}

impl EventHandler for FinalAnswerAckHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        let RunEvent::AssistantMessage(message) = event else {
            return Ok(());
        };
        if message.assistant_kind != Some(AssistantMessageKind::FinalAnswer) {
            return Ok(());
        }
        let session_id = self
            .store
            .active_projection_snapshot()?
            .frontier()
            .session_id()
            .to_owned();
        let public_event = crate::PublicRunEvent::new(
            session_id,
            "readiness-ack-run".to_owned(),
            1,
            crate::PublicRunEventKind::Notice {
                message: "pending adapter receipt".to_owned(),
            },
        );
        let public_event_id = "readiness-pending-public-event".to_owned();
        let outbox = crate::PublicEventOutboxEntryV1 {
            schema_version: crate::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.clone(),
            domain_event_id: public_event_id.clone(),
            run_id: public_event.run_id.clone(),
            sequence: 1,
            payload_digest: crate::stable_event_hash(serde_json::to_vec(&public_event)?),
            event: public_event,
        };
        crate::PublicEventOutboxRecorder::new(self.store.clone()).append_outbox(&outbox)?;
        let receipt = crate::PublicEventDeliveryReceiptV1 {
            schema_version: crate::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id,
            adapter: "tui".to_owned(),
            delivered_at_unix_ms: 1,
        };
        let writer_store = self.store.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        self.writer = Some(thread::spawn(move || {
            writer_store.append_event_if(
                DurableEventType::PublicEventDeliveryReceipt,
                crate::EventClass::Critical,
                serde_json::to_value(receipt)?,
                |_| {
                    // The final message is already durable. Keep a real ACK writer's
                    // coordinator and data-file lock occupied until finalization queues.
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(writer_store.path())?;
                    fs2::FileExt::try_lock_exclusive(&file)?;
                    let attempts = writer_store
                        .active_projection_metrics()
                        .writer_lock_attempt_total;
                    entered_tx.send(())?;
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while writer_store
                        .active_projection_metrics()
                        .writer_lock_attempt_total
                        == attempts
                    {
                        anyhow::ensure!(
                            Instant::now() < deadline,
                            "finalization did not queue on the ACK writer coordinator"
                        );
                        thread::yield_now();
                    }
                    drop(file);
                    Ok(true)
                },
            )
        }));
        entered_rx.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    }
}

#[tokio::test]
async fn agent_finalization_waits_for_public_ack_writer_before_recording_completed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::new(Mutex::new(Vec::new())),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model").with_store(store.clone());
    let mut handler = FinalAnswerAckHandler {
        store: store.clone(),
        writer: None,
    };
    let result = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("answer"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await;
    assert!(
        handler
            .writer
            .take()
            .expect("final answer started ACK writer")
            .join()
            .expect("ACK writer thread")?
    );
    let output = result?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    let readiness = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(value)) => Some(value),
            _ => None,
        })
        .expect("readiness persisted after waiting for ACK");
    assert_eq!(readiness.evaluation.run_status, crate::RunStatus::Completed);
    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::NotApplicable
    );
    assert_eq!(
        readiness.evaluation.visible_state,
        VisibleCompletionState::Completed
    );

    let records = store.read_event_records_coordinated()?;
    crate::PublicEventOutboxProjectionV1::from_records(&records)?;
    let final_sequence = records
        .iter()
        .find_map(|record| {
            let event = record.stored_event();
            (event.event_type == DurableEventType::AssistantMessageRecorded.as_str())
                .then_some(event.stream_sequence)
        })
        .expect("durable final answer");
    let ack_sequence = records
        .iter()
        .find_map(|record| {
            let event = record.stored_event();
            (event.event_type == DurableEventType::PublicEventDeliveryReceipt.as_str())
                .then_some(event.stream_sequence)
        })
        .expect("durable ACK");
    let terminals: Vec<_> = records
        .iter()
        .filter(|record| {
            record.stored_event().event_type == DurableEventType::RunFinalized.as_str()
        })
        .collect();
    assert_eq!(terminals.len(), 1);
    let terminal = terminals[0].stored_event();
    assert_eq!(terminal.payload["run_status"], "completed");
    assert_eq!(
        terminal.payload["final_message_id"],
        output.result.final_message_id.expect("final message id")
    );
    assert!(final_sequence < ack_sequence && ack_sequence < terminal.stream_sequence);
    Ok(())
}
