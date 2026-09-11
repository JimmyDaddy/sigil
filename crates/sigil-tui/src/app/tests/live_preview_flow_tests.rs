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
        attempt_id: attempt.to_owned(),
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
