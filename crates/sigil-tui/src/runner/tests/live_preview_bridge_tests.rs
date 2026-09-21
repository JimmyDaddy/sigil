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

#[test]
fn native_execution_preview_reaches_command_cards_without_a_provider_attempt() -> Result<()> {
    use sigil_kernel::{ToolCall, ToolExecutionId, ToolProgressEvent, ToolResult, ToolResultMeta};
    let temp = tempfile::tempdir()?;
    let session = Session::load_from_store(
        "fixture",
        "model",
        sigil_kernel::JsonlSessionStore::new(temp.path().join("session.jsonl"))?,
    )?;
    let mut app = crate::AppState::from_root_config(
        &temp.path().join("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    app.session_id = session.session_scope_id().to_owned();
    app.timeline.clear();
    app.runtime.is_busy = true;
    app.set_terminal_size(160, 60);
    let (sender, receiver) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(sender);
    handler.start_public_run(&session, "run", "input")?;
    for message in receiver.try_iter() {
        app.handle_worker_message(message)?;
    }
    for (call_id, command) in [
        ("scoped-call-one", "cargo check --workspace"),
        ("scoped-call-two", "cargo test --workspace"),
    ] {
        handler.handle(RunEvent::ToolCallCompleted(ToolCall {
            id: call_id.into(),
            name: "exec_command".into(),
            args_json: serde_json::json!({"command":command}).to_string(),
        }))?;
    }
    let progress =
        |execution_id: &str, call_id: &str, output: Option<String>, sequence| -> Result<RunEvent> {
            Ok(RunEvent::ToolProgress(ToolProgressEvent {
                execution_id: ToolExecutionId::new(execution_id).map_err(anyhow::Error::msg)?,
                call_id: call_id.into(),
                tool_name: "exec_command".into(),
                sequence,
                status: "running".into(),
                message: Some("command is running".into()),
                total_bytes: Some(output.as_ref().map_or(0, |value| value.len()) as u64),
                output_preview: output,
                output_log_ref: None,
                updated_at_ms: None,
                details: serde_json::Value::Null,
            }))
        };
    handler.handle(progress("execution-one", "scoped-call-one", None, 1)?)?;
    for sequence in 1..=1000 {
        handler.handle(progress(
            "execution-two",
            "scoped-call-two",
            Some(format!("test progress {sequence}")),
            sequence,
        )?)?;
    }
    app.poll_background_tasks();
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == crate::timeline::TimelineRole::Tool),
        "undelivered durable call frontier gates preview application"
    );
    let messages = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(
        messages.len(),
        4,
        "only completed calls and their durable frontiers enter the worker queue"
    );
    assert!(!messages.iter().any(|message| matches!(message, WorkerMessage::Event(event) if matches!(event.as_ref(), RunEvent::ToolProgress(_)))));
    for message in messages {
        app.handle_worker_message(message)?;
    }
    app.poll_background_tasks();
    app.flush_timeline_render_batch();
    let cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == crate::timeline::TimelineRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(
        cards.len(),
        2,
        "progress replaces the matching pending command card"
    );
    assert!(cards.iter().all(|entry| {
        serde_json::from_str::<serde_json::Value>(&entry.text).expect("tool card")["status"]
            == "running"
    }));
    let rendered = app.timeline_plain_lines().join("\n");
    assert!(rendered.contains("cargo check --workspace"), "{rendered}");
    assert!(rendered.contains("cargo test --workspace"), "{rendered}");
    assert!(rendered.contains("test progress 1000"), "{rendered}");
    assert!(
        !rendered.contains("command is running"),
        "a status message is not captured stdout: {rendered}"
    );
    handler.handle(RunEvent::ToolResult(ToolResult::ok(
        "scoped-call-one",
        "exec_command",
        "completed",
        ToolResultMeta::default(),
    )))?;
    handler.handle(progress(
        "execution-one",
        "scoped-call-one",
        Some("late first output".into()),
        2,
    )?)?;
    for message in receiver.try_iter() {
        app.handle_worker_message(message)?;
    }
    app.poll_background_tasks();
    app.flush_timeline_render_batch();
    let cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == crate::timeline::TimelineRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert_eq!(
        cards
            .iter()
            .filter(
                |entry| serde_json::from_str::<serde_json::Value>(&entry.text).expect("tool card")
                    ["status"]
                    == "running"
            )
            .count(),
        1
    );
    assert!(
        !app.timeline_plain_lines()
            .join("\n")
            .contains("late first output")
    );
    Ok(())
}

#[test]
fn native_scoped_task_approvals_update_distinct_command_cards_and_keep_authority_ids() -> Result<()>
{
    use sigil_kernel::{ToolCall, ToolSpec};
    let temp = tempfile::tempdir()?;
    let session = Session::load_from_store(
        "fixture",
        "model",
        sigil_kernel::JsonlSessionStore::new(temp.path().join("session.jsonl"))?,
    )?;
    let mut app = crate::AppState::from_root_config(
        &temp.path().join("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    app.session_id = session.session_scope_id().into();
    app.timeline.clear();
    app.set_terminal_size(160, 60);
    let (sender, receiver) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(sender);
    handler.start_public_run(&session, "run", "input")?;
    let call = |id: &str, command: &str| ToolCall {
        id: id.into(),
        name: "exec_command".into(),
        args_json: serde_json::json!({"command": command}).to_string(),
    };
    let requested = |display_id: &str, request_id: &str, command: &str| {
        let mut identity = crate::app::tests::common::test_approval_identity("same-provider-id");
        identity.approval_request_id = request_id.into();
        RunEvent::ToolApprovalRequested {
            display_call_id: Some(display_id.into()),
            approval_identity: identity,
            effects: std::collections::BTreeSet::new(),
            analysis: sigil_kernel::ToolAnalysisStatus::Complete,
            containment: sigil_kernel::ExecutionContainmentRequest::default(),
            safe_summary: sigil_kernel::ToolPermissionSummary::default(),
            decision_reasons: Vec::new(),
            session_grant_available: false,
            session_grant_unavailable_reason: None,
            call: call("same-provider-id", command),
            spec: ToolSpec {
                name: "exec_command".into(),
                description: "Execute a command".into(),
                input_schema: serde_json::json!({"type":"object"}),
                category: sigil_kernel::ToolCategory::Shell,
                access: sigil_kernel::ToolAccess::Write,
                network_effect: None,
                preview: sigil_kernel::ToolPreviewCapability::None,
            },
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: sigil_kernel::ApprovalMode::Ask,
            network_policy_decision: sigil_kernel::ApprovalMode::Allow,
            source_policy_decision: sigil_kernel::ApprovalMode::Allow,
            operation: sigil_kernel::ToolOperation::ExecuteWorkspaceCheckCommand,
            risk: sigil_kernel::PermissionRisk::Medium,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        }
    };
    for (display_id, request_id, command) in [
        ("task-one-call", "approval-one", "cargo check --workspace"),
        ("task-two-call", "approval-two", "cargo test --workspace"),
    ] {
        handler.handle(RunEvent::ToolCallCompleted(call(display_id, command)))?;
        handler.handle(requested(display_id, request_id, command))?;
    }
    for message in receiver.try_iter() {
        app.handle_worker_message(message)?;
    }
    app.flush_timeline_render_batch();
    let cards = || {
        app.timeline
            .iter()
            .filter(|entry| entry.role == crate::timeline::TimelineRole::Tool)
            .map(|entry| serde_json::from_str::<serde_json::Value>(&entry.text).expect("card"))
            .collect::<Vec<_>>()
    };
    assert_eq!(cards().len(), 2);
    assert!(cards().iter().all(|card| card["status"] == "approval"));
    let pending = app.approval.pending.as_ref().expect("second approval");
    assert_eq!(pending.call.id, "same-provider-id");
    assert_eq!(pending.approval_request_id, "approval-two");
    let rendered = app.timeline_plain_lines().join("\n");
    assert!(rendered.contains("cargo check --workspace"), "{rendered}");
    assert!(rendered.contains("cargo test --workspace"), "{rendered}");
    handler.handle(RunEvent::ToolApprovalResolved {
        display_call_id: Some("task-two-call".into()),
        call_id: "same-provider-id".into(),
        approval_request_id: "stale-request".into(),
        approved: false,
        reason: None,
    })?;
    for message in receiver.try_iter() {
        app.handle_worker_message(message)?;
    }
    assert!(app.timeline.iter().filter(|entry| entry.role == crate::timeline::TimelineRole::Tool)
        .all(|entry| serde_json::from_str::<serde_json::Value>(&entry.text).expect("card")["status"] == "approval"));
    handler.handle(RunEvent::ToolApprovalResolved {
        display_call_id: Some("task-one-call".into()),
        call_id: "same-provider-id".into(),
        approval_request_id: "approval-one".into(),
        approved: false,
        reason: Some("denied".into()),
    })?;
    for message in receiver.try_iter() {
        app.handle_worker_message(message)?;
    }
    let cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == crate::timeline::TimelineRole::Tool)
        .map(|entry| serde_json::from_str::<serde_json::Value>(&entry.text).expect("card"))
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0]["status"], "denied");
    assert_eq!(cards[1]["status"], "approval");
    assert_eq!(
        app.approval
            .pending
            .as_ref()
            .expect("second approval stays open")
            .approval_request_id,
        "approval-two"
    );
    handler.handle(RunEvent::ToolApprovalResolved {
        display_call_id: Some("task-two-call".into()),
        call_id: "same-provider-id".into(),
        approval_request_id: "approval-two".into(),
        approved: true,
        reason: None,
    })?;
    for message in receiver.try_iter() {
        app.handle_worker_message(message)?;
    }
    assert!(app.approval.pending.is_none());
    let cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == crate::timeline::TimelineRole::Tool)
        .map(|entry| serde_json::from_str::<serde_json::Value>(&entry.text).expect("card"))
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0]["status"], "denied");
    assert_eq!(cards[1]["status"], "pending");
    Ok(())
}
