use std::path::Path;

use super::*;
use crate::app::tests::common::test_config;

fn queued_item(
    id: &str,
    kind: sigil_kernel::ConversationInputKind,
    status: sigil_kernel::ConversationInputStatus,
) -> sigil_kernel::ConversationQueueItemProjection {
    queued_item_with_target(
        id,
        sigil_kernel::ConversationInputTarget::MainThread,
        kind,
        status,
    )
}

fn queued_item_with_target(
    id: &str,
    target: sigil_kernel::ConversationInputTarget,
    kind: sigil_kernel::ConversationInputKind,
    status: sigil_kernel::ConversationInputStatus,
) -> sigil_kernel::ConversationQueueItemProjection {
    sigil_kernel::ConversationQueueItemProjection {
        queued: sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new(id).expect("valid queue id"),
            target,
            kind,
            prompt_hash: format!("sha256:{id}"),
            prompt: format!("{id} prompt"),
            reasoning_effort: None,
            created_at_ms: None,
        },
        status,
        reason: None,
    }
}

fn queued_entry(id: &str, prompt: &str) -> sigil_kernel::SessionLogEntry {
    sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::ConversationInputQueued(
        sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new(id).expect("valid queue id"),
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: format!("sha256:{id}"),
            prompt: prompt.to_owned(),
            reasoning_effort: None,
            created_at_ms: None,
        },
    ))
}

#[test]
fn queue_flow_helpers_cover_kinds_statuses_and_empty_targets() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_entry("queue_1", "first"),
        queued_entry("queue_2", "second"),
    ]);
    assert_eq!(
        app.queue_index_for_target("")
            .expect("empty target uses selection"),
        0
    );
    app.composer.queue_selected = 1;
    let selected = app
        .queue_action_for_target("", |queue_id| AppAction::PromoteQueuedConversationInput {
            queue_id,
        })
        .expect("selected queue item should resolve");
    assert!(matches!(
        selected,
        AppAction::PromoteQueuedConversationInput { ref queue_id }
            if queue_id.as_str() == "queue_2"
    ));
    assert_eq!(
        app.composer_queue_summary().as_deref(),
        Some("2 follow-ups pending · next main: first")
    );

    let paused = queued_item(
        "queue_paused",
        sigil_kernel::ConversationInputKind::Chat,
        sigil_kernel::ConversationInputStatus::Queued,
    );
    assert_eq!(
        queue_item_detail(&paused, true),
        "paused · main · follow-up"
    );
    assert_eq!(
        queue_status_kind(sigil_kernel::ConversationInputStatus::Queued, true),
        StatusKind::Warning
    );

    for (kind, label) in [
        (
            sigil_kernel::ConversationInputKind::PlanPrompt,
            "pending · main · plan",
        ),
        (
            sigil_kernel::ConversationInputKind::AgentMention,
            "pending · main · agent",
        ),
        (
            sigil_kernel::ConversationInputKind::AgentMessage,
            "pending · main · message",
        ),
        (
            sigil_kernel::ConversationInputKind::Unknown,
            "pending · main · unknown",
        ),
    ] {
        assert_eq!(
            queue_item_detail(
                &queued_item(
                    "queue_kind",
                    kind,
                    sigil_kernel::ConversationInputStatus::Queued
                ),
                false,
            ),
            label
        );
    }
    let agent_thread = queued_item_with_target(
        "queue_agent",
        sigil_kernel::ConversationInputTarget::AgentThread {
            thread_id: sigil_kernel::AgentThreadId::new("agent_chat_1")
                .expect("valid agent thread id"),
        },
        sigil_kernel::ConversationInputKind::AgentMessage,
        sigil_kernel::ConversationInputStatus::Queued,
    );
    assert_eq!(
        queue_item_detail(&agent_thread, false),
        "pending · agent agent_chat_1 · message"
    );
    let mut agent_queue_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    agent_queue_app.sync_current_session_state(vec![sigil_kernel::SessionLogEntry::Control(
        sigil_kernel::ControlEntry::ConversationInputQueued(agent_thread.queued.clone()),
    )]);
    assert_eq!(agent_queue_app.composer_queue_summary().as_deref(), None);

    for (status, label, kind) in [
        (
            sigil_kernel::ConversationInputStatus::Dispatching,
            "dispatching",
            StatusKind::Running,
        ),
        (
            sigil_kernel::ConversationInputStatus::Delivered,
            "delivered",
            StatusKind::Success,
        ),
        (
            sigil_kernel::ConversationInputStatus::Rejected,
            "rejected",
            StatusKind::Error,
        ),
        (
            sigil_kernel::ConversationInputStatus::Cancelled,
            "cancelled",
            StatusKind::Error,
        ),
        (
            sigil_kernel::ConversationInputStatus::Stale,
            "stale",
            StatusKind::Error,
        ),
        (
            sigil_kernel::ConversationInputStatus::Unknown,
            "unknown",
            StatusKind::Unknown,
        ),
    ] {
        assert_eq!(queue_status_label(status), label);
        assert_eq!(queue_status_kind(status, false), kind);
    }
}

fn complete_operation(
    app: &mut AppState,
    operation: QueueOperation,
    result: std::result::Result<(), QueueOperationFailure>,
) {
    app.handle_worker_message(
        crate::runner::WorkerMessage::ConversationQueueOperationCompleted {
            session_log_path: app.session_log_path.clone(),
            operation,
            result,
        },
    )
    .expect("local queue receipt applies");
}

#[test]
fn queue_remove_waits_for_receipt_and_rejects_repeated_actions_until_failure() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![queued_entry("queue_1", "first")]);
    let queue_id = ConversationInputQueueId::new("queue_1").expect("queue id");
    assert!(app.cancel_selected_queue_item().is_some());
    assert_eq!(app.composer_queue_rows()[0].detail, "removing follow-up");
    assert_eq!(app.composer_queue_rows()[0].status, StatusKind::Running);
    assert_eq!(app.last_notice(), Some("removing follow-up"));
    assert!(app.cancel_selected_queue_item().is_none());
    assert!(app.promote_selected_queue_item().is_none());
    assert!(
        app.move_selected_queue_item(QueueMoveDirection::Down)
            .is_none()
    );
    assert!(!app.begin_edit_selected_queue_item());
    assert!(
        app.execute_queue_slash_command("delete 1")
            .expect("slash")
            .is_none()
    );
    assert!(
        crate::view_model::LivePanelViewModel::from_app(&app, 1)
            .queue_action_buttons
            .iter()
            .all(|button| !button.enabled)
    );

    complete_operation(
        &mut app,
        QueueOperation::Cancel {
            queue_id: queue_id.clone(),
        },
        Err(QueueOperationFailure::Storage {
            message: "queue write failed".to_owned(),
        }),
    );
    assert_eq!(app.composer_queue_rows().len(), 1);
    assert_eq!(app.composer_queue_rows()[0].status, StatusKind::Error);
    assert_eq!(app.composer_queue_rows()[0].detail, "queue write failed");
    assert!(app.composer_queue_actions_enabled());
    assert!(app.cancel_selected_queue_item().is_some());
    let entries = vec![
        queued_entry("queue_1", "first"),
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(
            sigil_kernel::ConversationInputStatusEntry {
                queue_id: queue_id.clone(),
                status: ConversationInputStatus::Cancelled,
                reason: Some("cancelled by user".to_owned()),
                updated_at_ms: None,
            },
        )),
    ];
    app.handle_worker_message(crate::runner::WorkerMessage::ConversationQueueUpdated {
        items: Vec::new(),
        paused: false,
        entries,
    })
    .expect("durable queue update");
    complete_operation(&mut app, QueueOperation::Cancel { queue_id }, Ok(()));
    assert!(app.composer_queue_rows().is_empty());
    assert_eq!(app.last_notice(), Some("follow-up removed"));
}

#[test]
fn queue_operation_failure_preserves_active_task_tools_approval_and_recovery_gate() {
    use crate::app::tests::common::{inject_write_file_approval, sample_approval_preview};
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let task_id = sigil_kernel::TaskId::new("task_running").expect("task id");
    let mut queued = queued_item_with_target(
        "queue_1",
        ConversationInputTarget::Task { task_id },
        ConversationInputKind::TaskGuidance,
        ConversationInputStatus::Queued,
    )
    .queued;
    queued.prompt = "preserve ongoing work".to_owned();
    app.sync_current_session_state(vec![SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(queued),
    )]);
    app.handle_worker_message(crate::runner::WorkerMessage::TaskRunStarted {
        task_id: "task_running".to_owned(),
        objective: "ongoing task".to_owned(),
    })
    .expect("task started");
    inject_write_file_approval(&mut app, sample_approval_preview()).expect("pending approval");
    app.worker_ready = true;
    app.runtime.allow_projection_run_recovery = true;
    app.safe_tool_calls.insert(
        "inflight_tool".to_owned(),
        sigil_kernel::ToolCall {
            id: "inflight_tool".to_owned(),
            name: "bash".to_owned(),
            args_json: "{}".to_owned(),
        },
    );
    let phase = app.runtime.run_phase.clone();
    let approval = format!("{:?}", app.approval.pending);
    assert!(app.cancel_selected_queue_item().is_some());
    complete_operation(
        &mut app,
        QueueOperation::Cancel {
            queue_id: ConversationInputQueueId::new("queue_1").expect("queue id"),
        },
        Err(QueueOperationFailure::ItemUnavailable {
            queue_id: ConversationInputQueueId::new("queue_1").expect("queue id"),
            status: ConversationInputStatus::Delivered,
        }),
    );
    assert!(app.worker_ready);
    assert!(app.runtime.is_busy);
    assert!(app.runtime.allow_projection_run_recovery);
    assert_eq!(app.runtime.run_phase, phase);
    assert_eq!(
        app.runtime
            .active_task
            .as_ref()
            .expect("active task")
            .task_id,
        "task_running"
    );
    assert_eq!(format!("{:?}", app.approval.pending), approval);
    assert!(app.safe_tool_calls.contains_key("inflight_tool"));
    assert!(
        app.timeline
            .iter()
            .all(|entry| !entry.text.starts_with("Run failed:"))
    );
}

#[test]
fn queue_edit_keeps_draft_until_matching_success_and_preserves_later_typing() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![queued_entry("queue_1", "first")]);
    assert!(app.begin_edit_selected_queue_item());
    app.set_input_and_cursor("edited follow-up".to_owned());
    let action = app
        .finish_queue_edit_submission("edited follow-up".to_owned())
        .expect("edit queued");
    assert_eq!(app.composer.input, "edited follow-up");
    assert!(
        app.finish_queue_edit_submission("edited follow-up".to_owned())
            .is_none()
    );
    assert!(app.fail_queue_action(&action, "edit was refused".to_owned()));
    assert_eq!(app.composer.input, "edited follow-up");
    assert!(app.composer.queue_edit_target.is_some());
    assert!(app.composer.pending_queue_operations.is_empty());
    let retry = app
        .finish_queue_edit_submission("edited follow-up".to_owned())
        .expect("retry edit");
    let operation = AppState::queue_operation_for_action(&retry).expect("edit operation");
    app.set_input_and_cursor("newer composer draft".to_owned());
    complete_operation(&mut app, operation, Ok(()));
    assert_eq!(app.composer.input, "newer composer draft");
    assert!(app.composer.queue_edit_target.is_some());
    let next_edit = app
        .finish_queue_edit_submission("newer composer draft".to_owned())
        .expect("later typing remains an editable follow-up");
    let next_operation = AppState::queue_operation_for_action(&next_edit).expect("edit operation");
    complete_operation(&mut app, next_operation, Ok(()));
    assert!(app.composer.queue_edit_target.is_none());
    assert!(app.composer.input.is_empty());
}

#[test]
fn queue_receipts_clear_only_matching_item_and_ignore_previous_session() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_entry("queue_1", "first"),
        queued_entry("queue_2", "second"),
    ]);
    assert!(app.cancel_selected_queue_item().is_some());
    app.composer.queue_selected = 1;
    assert!(app.cancel_selected_queue_item().is_some());
    let first = ConversationInputQueueId::new("queue_1").expect("queue id");
    let second = ConversationInputQueueId::new("queue_2").expect("queue id");
    complete_operation(
        &mut app,
        QueueOperation::Promote {
            queue_id: first.clone(),
        },
        Ok(()),
    );
    assert_eq!(app.composer.pending_queue_operations.len(), 2);
    complete_operation(
        &mut app,
        QueueOperation::Cancel {
            queue_id: first.clone(),
        },
        Err(QueueOperationFailure::UnknownItem {
            queue_id: first.clone(),
        }),
    );
    assert!(!app.composer.pending_queue_operations.contains_key(&first));
    assert!(app.composer.pending_queue_operations.contains_key(&second));
    let previous_path = app.session_log_path.clone();
    app.restore_session_view(
        crate::app::tests::common::fixture_session_id(
            &previous_path.with_file_name("different-session.jsonl"),
        ),
        previous_path.with_file_name("different-session.jsonl"),
        "test".to_owned(),
        "model".to_owned(),
        vec![queued_entry("queue_1", "new session item")],
        "switched",
    );
    assert!(app.composer.pending_queue_operations.is_empty());
    assert!(app.composer.queue_operation_errors.is_empty());
    assert!(app.cancel_selected_queue_item().is_some());
    let notice = app.last_notice().map(str::to_owned);
    app.handle_worker_message(
        crate::runner::WorkerMessage::ConversationQueueOperationCompleted {
            session_log_path: previous_path,
            operation: QueueOperation::Cancel {
                queue_id: first.clone(),
            },
            result: Err(QueueOperationFailure::UnknownItem {
                queue_id: first.clone(),
            }),
        },
    )
    .expect("old receipt ignored");
    assert!(app.composer.pending_queue_operations.contains_key(&first));
    assert_eq!(app.last_notice(), notice.as_deref());
}

#[test]
fn queue_pause_waits_for_matching_receipt_without_optimistic_state_change() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![queued_entry("queue_1", "first")]);
    let pause = app.toggle_queue_pause_to(true).expect("pause dispatched");
    assert!(!app.composer_queue_paused());
    assert_eq!(app.last_notice(), Some("pausing follow-ups"));
    assert!(app.toggle_queue_pause_to(true).is_none());
    assert!(app.toggle_queue_pause_to(false).is_none());
    complete_operation(
        &mut app,
        QueueOperation::SetPaused { paused: false },
        Ok(()),
    );
    assert_eq!(app.composer.pending_queue_pause, Some(true));
    assert!(app.fail_queue_action(&pause, "pause write failed".to_owned()));
    assert_eq!(app.composer.pending_queue_pause, None);
    assert!(!app.composer_queue_paused());
    assert!(app.toggle_queue_pause_to(true).is_some());
}

#[test]
fn queue_enqueue_failure_remains_visible_after_projection_confirmation() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_optimistic_conversation_queue_item(
        "queued input".to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
    );
    let operation = app.composer.pending_queue_enqueues[0].clone();
    // Durable confirmation keeps the submitted reasoning binding as well as its prompt.
    let mut confirmed = app.composer.optimistic_queue_items[0].clone();
    confirmed.queue_id = ConversationInputQueueId::new("queue_1").expect("valid queue id");
    app.sync_current_session_state(vec![SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(confirmed),
    )]);
    assert!(app.composer.optimistic_queue_items.is_empty());
    app.set_input_and_cursor("new draft".to_owned());
    complete_operation(
        &mut app,
        operation,
        Err(QueueOperationFailure::Storage {
            message: "queue confirmation read failed".to_owned(),
        }),
    );
    assert_eq!(app.last_notice(), Some("queue confirmation read failed"));
    assert!(app.composer.pending_queue_enqueues.is_empty());
    assert_eq!(app.composer_queue_rows().len(), 1);
    assert_eq!(app.composer.input, "new draft");
}
