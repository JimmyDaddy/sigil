use super::*;

#[test]
fn streaming_batch_defers_rerender_until_drain() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.begin_timeline_render_batch();
    app.handle(RunEvent::TextDelta("```rust\n".to_owned()))?;
    let revision_after_first_delta = app.timeline_revision();
    for _ in 0..32 {
        app.handle(RunEvent::TextDelta("fn main() {}\n".to_owned()))?;
    }
    app.handle(RunEvent::TextDelta("```\n".to_owned()))?;

    let rendered_before_flush = app.timeline_plain_lines().join("\n");
    assert!(!rendered_before_flush.contains("fn main"));
    assert_eq!(app.timeline_revision(), revision_after_first_delta);

    assert!(app.flush_timeline_render_batch());

    let rendered_after_flush = app.timeline_plain_lines().join("\n");
    assert!(rendered_after_flush.contains("fn main"));
    assert!(app.timeline_revision() > revision_after_first_delta);
    Ok(())
}

#[test]
fn streaming_deltas_do_not_fill_ui_event_log() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let initial_events = app.events.len();

    for _ in 0..32 {
        app.handle(RunEvent::TextDelta("chunk ".to_owned()))?;
    }

    assert!(
        app.events
            .iter()
            .any(|event| event.label == "phase" && event.detail == "streaming")
    );
    assert!(!app.events.iter().any(|event| event.label == "text"));
    let after_text_events = app.events.len();
    assert_eq!(after_text_events, initial_events + 1);

    for _ in 0..32 {
        app.handle(RunEvent::ReasoningDelta("thought ".to_owned()))?;
    }

    assert!(
        app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        })
    );
    assert!(!app.events.iter().any(|event| event.label == "reasoning"));
    assert_eq!(app.events.len(), after_text_events + 1);

    for _ in 0..32 {
        app.handle(RunEvent::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: r#"{"path":"src/lib.rs"}"#.to_owned(),
        })?;
    }

    assert!(!app.events.iter().any(|event| event.label == "tool:args"));
    assert_eq!(app.events.len(), after_text_events + 1);
    Ok(())
}
