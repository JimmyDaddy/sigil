use super::*;
use sigil_application::{APPLICATION_CONTRACT_SCHEMA_VERSION, SafeText};
use sigil_kernel::{EventHandler, ModelMessage, RunEvent};

fn fixture() -> AppState {
    let mut app = AppState::from_root_config(
        std::path::Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    app.timeline.clear();
    app.live_preview.session_id = app.session_id.clone();
    app.live_preview.run_id = "run".to_owned();
    app
}

fn update(app: &AppState, attempt: &str, revision: u64, base: u64, text: &str) -> LiveRunUpdate {
    LiveRunUpdate {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        session_id: app.session_id.clone(),
        run_id: "run".to_owned(),
        attempt_id: Some(attempt.to_owned()),
        slot_id: "assistant-text".to_owned(),
        live_revision: revision,
        base_durable_sequence: base,
        kind: LiveRunUpdateKind::Text,
        preview: SafeText::new(text).expect("bounded preview"),
        tool_progress: None,
        truncated: false,
    }
}

#[test]
fn live_replacements_wait_for_delivered_frontier_and_reject_stale_attempts_and_revisions() {
    let mut app = fixture();
    app.accept_live_updates(vec![update(&app, "attempt-1", 1, 3, "first")]);
    assert!(!app.apply_ready_live_updates());
    assert!(app.timeline.is_empty());
    app.accept_live_durable_frontier(&app.session_id.clone(), "unrelated-run", 99);
    assert!(!app.apply_ready_live_updates());
    app.accept_live_durable_frontier(&app.session_id.clone(), "run", 3);
    assert!(app.apply_ready_live_updates());
    assert_eq!(app.timeline.len(), 1);
    app.accept_live_updates(vec![
        update(&app, "attempt-1", 3, 3, "replacement"),
        update(&app, "attempt-1", 2, 3, "stale"),
    ]);
    assert!(app.apply_ready_live_updates());
    assert_eq!(app.timeline[0].text, "replacement");
    app.accept_live_updates(vec![
        update(&app, "attempt-1", 4, 10, "future old"),
        update(&app, "attempt-2", 5, 3, "fresh"),
    ]);
    assert!(app.apply_ready_live_updates());
    app.accept_live_durable_frontier(&app.session_id.clone(), "run", 10);
    app.accept_live_updates(vec![update(&app, "attempt-1", 4, 3, "old attempt")]);
    assert!(!app.apply_ready_live_updates());
    assert_eq!(app.timeline.len(), 1);
    assert_eq!(app.timeline[0].text, "fresh");
}

#[test]
fn final_message_replaces_truncated_preview_and_prevents_late_reappearance() -> anyhow::Result<()> {
    let mut app = fixture();
    let mut truncated = update(&app, "attempt-1", 1, 0, "prefix");
    truncated.truncated = true;
    app.accept_live_updates(vec![truncated]);
    assert!(app.apply_ready_live_updates());
    assert!(app.timeline[0].text.contains("Live preview truncated"));
    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("prefix and complete final".to_owned()),
        Vec::new(),
    )))?;
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );
    assert_eq!(app.timeline[0].text, "prefix and complete final");
    app.accept_live_updates(vec![update(&app, "attempt-1", 2, 0, "late")]);
    assert!(!app.apply_ready_live_updates());
    assert_eq!(app.timeline[0].text, "prefix and complete final");
    Ok(())
}

#[test]
fn discarded_and_terminal_live_output_cannot_return_after_a_later_frontier() {
    let mut app = fixture();
    app.accept_live_updates(vec![update(&app, "attempt-1", 1, 9, "pending")]);
    app.discard_live_attempt();
    app.accept_live_durable_frontier(&app.session_id.clone(), "run", 9);
    assert!(!app.apply_ready_live_updates());
    app.accept_live_updates(vec![update(&app, "attempt-2", 2, 10, "next")]);
    app.clear_worker_run_state();
    app.accept_live_durable_frontier(&app.session_id.clone(), "run", 10);
    assert!(!app.apply_ready_live_updates());
    assert!(app.timeline.is_empty());
    assert!(app.live_preview.pending.is_empty());
    assert!(app.live_preview.run_id.is_empty());
}

fn task_snapshot_fixture() -> anyhow::Result<(
    tempfile::TempDir,
    AppState,
    Vec<sigil_kernel::SessionLogEntry>,
)> {
    let root = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = root.path().display().to_string();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.handle_worker_message(crate::runner::WorkerMessage::TaskRunStarted {
        task_id: "snapshot_task".to_owned(),
        objective: "preserve the durable Task".to_owned(),
    })?;
    app.live_preview.session_id = app.session_id.clone();
    app.live_preview.run_id = "current-run".to_owned();
    app.live_preview.durable_sequence = 1;
    let task_id = sigil_kernel::TaskId::new("snapshot_task")?;
    let entries = vec![
        sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskRun(
            sigil_kernel::TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                objective: "preserve the durable Task".to_owned(),
                title: None,
                status: sigil_kernel::TaskRunStatus::Started,
                reason: None,
            },
        )),
        sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::TaskDirectExecutionAdmittedV1(
                sigil_kernel::TaskDirectExecutionAdmittedV1::task_request(
                    task_id.clone(),
                    "preserve the durable Task",
                    1,
                ),
            ),
        ),
        sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::TaskRunCancellationScopeBound(
                sigil_kernel::TaskRunCancellationScopeBoundEntry {
                    task_id,
                    run_scope_id: "current-scope".to_owned(),
                },
            ),
        ),
    ];
    Ok((root, app, entries))
}

#[test]
fn task_pause_projection_rejects_foreign_session_run_task_and_older_frontier() -> anyhow::Result<()>
{
    for invalid in ["session", "run", "task", "frontier"] {
        let (_root, mut app, entries) = task_snapshot_fixture()?;
        let before = serde_json::to_value(&app.session_browser.current_entries)?;
        let message = crate::runner::WorkerMessage::TaskStateSynchronized {
            session_id: if invalid == "session" {
                "old-session".to_owned()
            } else {
                app.session_id.clone()
            },
            run_id: if invalid == "run" {
                "old-run".to_owned()
            } else {
                "current-run".to_owned()
            },
            task_id: if invalid == "task" {
                "old_task".to_owned()
            } else {
                "snapshot_task".to_owned()
            },
            public_sequence: if invalid == "frontier" { 0 } else { 1 },
            entries,
        };
        app.handle_worker_message(message)?;
        assert_eq!(
            serde_json::to_value(&app.session_browser.current_entries)?,
            before,
            "reject {invalid}"
        );
        assert!(app.active_task_pause_request().is_err());
    }
    Ok(())
}

#[test]
fn task_pause_projection_is_applied_once_without_overwriting_later_live_entries()
-> anyhow::Result<()> {
    let (_root, mut app, entries) = task_snapshot_fixture()?;
    let snapshot = |app: &AppState| crate::runner::WorkerMessage::TaskStateSynchronized {
        session_id: app.session_id.clone(),
        run_id: "current-run".to_owned(),
        task_id: "snapshot_task".to_owned(),
        public_sequence: 1,
        entries: entries.clone(),
    };
    app.handle_worker_message(snapshot(&app))?;
    assert!(app.active_task_pause_request().is_ok());
    app.handle(RunEvent::Control(sigil_kernel::ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: sigil_kernel::TaskId::new("snapshot_task")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "preserve the durable Task".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        },
    )))?;
    let after_live = serde_json::to_value(&app.session_browser.current_entries)?;
    // A duplicate is already unsafe before the separate frontier wakeup arrives.
    app.handle_worker_message(snapshot(&app))?;
    assert_eq!(
        serde_json::to_value(&app.session_browser.current_entries)?,
        after_live
    );
    app.accept_live_durable_frontier(&app.session_id.clone(), "current-run", 2);
    app.handle_worker_message(snapshot(&app))?;
    assert_eq!(
        serde_json::to_value(&app.session_browser.current_entries)?,
        after_live
    );
    Ok(())
}
