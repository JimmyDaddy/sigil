use super::*;

#[test]
fn short_transcript_stays_in_live_panel_instead_of_terminal_scrollback() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(120, 32);
    app.push_timeline(TimelineRole::User, "hello");
    app.push_timeline(TimelineRole::Assistant, "latest answer");

    assert_eq!(app.scrollback_line_count(), 0);
    let live = app
        .transcript_lines(app.timeline_viewport_rows())
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(live.contains("hello"));
    assert!(live.contains("latest answer"));
}

#[test]
fn reasoning_delta_creates_collapsed_thinking_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;

    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Phase)
    );
    assert!(
        app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        })
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Thinking && entry.text == "planning step 1\nplanning step 2"
    }));
    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));
    Ok(())
}

#[test]
fn ctrl_t_toggles_thinking_block_expansion() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;

    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    let expanded = app.transcript_lines(20);
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    }));
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));
    assert_eq!(app.last_notice(), Some("thinking expanded"));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    let recollapsed = app.transcript_lines(20);
    assert!(recollapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(!recollapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));
    assert_eq!(app.last_notice(), Some("thinking collapsed"));
    Ok(())
}

#[test]
fn tool_result_is_rendered_as_multiline_json_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle(RunEvent::ToolResult(termquill_kernel::ToolResult {
        call_id: "call-1".to_owned(),
        tool_name: "ls".to_owned(),
        content: "[\".git\",\"Cargo.toml\"]".to_owned(),
        is_error: false,
        metadata: termquill_kernel::ToolResultMeta::default(),
    }))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(entry.role, TimelineRole::Tool);
    assert_eq!(rendered["tool_name"], "ls");
    assert_eq!(rendered["preview_kind"], "json");
    assert_eq!(rendered["status"], "ok");
    assert!(rendered["preview_lines"].as_array().is_some_and(|lines| {
        lines
            .iter()
            .any(|line| line.as_str().is_some_and(|text| text.contains(".git")))
    }));
    Ok(())
}

#[test]
fn ctrl_u_and_ctrl_d_scroll_transcript_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    let bottom = app.transcript_lines(4);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))?;
    let scrolled = app.transcript_lines(4);

    assert!(app.timeline_scroll_back > 0);
    assert_ne!(bottom, scrolled);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn ctrl_home_and_ctrl_end_jump_transcript_between_oldest_and_newest() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
    assert_eq!(app.timeline_scroll_back, app.max_timeline_scroll_back());

    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL))?;
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn scrolling_transcript_to_top_reaches_earliest_message() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..20 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
    let top = app.transcript_lines(app.timeline_viewport_rows());

    assert!(top.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("message 0"))
    }));
    assert_eq!(app.timeline_scroll_back, app.max_timeline_scroll_back());
    Ok(())
}

#[test]
fn transcript_live_tail_ignores_trailing_gap_rows() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.push_timeline(TimelineRole::User, "hello");

    let tail = app.transcript_lines(1);
    let rendered = tail
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(rendered.contains("hello"));
}

#[test]
fn mouse_scroll_moves_transcript() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    app.handle_mouse_scroll(true);
    assert!(app.timeline_scroll_back > 0);

    app.handle_mouse_scroll(false);
    assert_eq!(app.timeline_scroll_back, 0);
}

#[test]
fn compaction_status_tracks_latest_prompt_tokens_instead_of_cumulative_totals() -> Result<()> {
    let mut config = test_config();
    config.agent.provider = "planned".to_owned();
    config.agent.model = "planned-model".to_owned();
    config.compaction.context_window_tokens = Some(100);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 70,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 70,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;
    assert_eq!(app.compaction_status, "soft");

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 20,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 20,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;

    assert_eq!(app.compaction_status, "ready");
    Ok(())
}

#[test]
fn context_usage_and_compaction_policy_share_effective_window() -> Result<()> {
    let mut config = test_config();
    config.agent.model = "deepseek-v4-pro".to_owned();
    config.compaction.context_window_tokens = Some(128_000);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 90_354,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 90_354,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;

    assert_eq!(app.context_usage_line(), "ctx: 9% · 90.4K / 1.0M tok");
    assert_eq!(app.compaction_status, "ready");
    assert!(app.footer_status_line().contains("tok 90.4K"));
    assert!(app.footer_status_line().contains("ctx 9%"));
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "policy: 1,000,000 model · soft 50% · hard 80%")
    );
    Ok(())
}

#[test]
fn live_activity_summary_tracks_busy_phase() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    assert!(app.live_activity_summary().is_none());

    app.is_busy = true;
    app.run_phase = RunPhase::Tool("read_file".to_owned());

    let summary = app.live_activity_summary().expect("expected live summary");
    assert_eq!(summary.label, "tool");
    assert_eq!(summary.detail, "running read_file");
}
