use super::super::timeline_flow::{
    TimelineHistoryAnchor, selected_timeline_line_columns, text_by_display_columns,
};
use super::*;
use crate::{
    app::{AgentView, EGRESS_DISCLOSURE_HEIGHT},
    mouse::{AppMouseOutcome, MouseInput, MouseInputKind},
    timeline::TimelineEntry,
    ui::LayoutSnapshot,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
};
use std::path::PathBuf;

#[test]
fn active_disclosure_reduces_the_timeline_viewport_by_reserved_rows() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(100, 30);
    let before = app.timeline_viewport_rows();
    let (receipt_tx, _receipt_rx) = tokio::sync::oneshot::channel();
    app.handle_worker_message(WorkerMessage::EgressDisclosureRequested {
        disclosure: sigil_kernel::PreEgressDisclosure::new(
            sigil_kernel::EgressDisclosureKind::Query,
            Some("query-viewport".to_owned()),
            "builtin-search",
            "tui",
            "Web search",
            "route-fingerprint",
            "profile-fingerprint",
            "https://example.com/",
            "https://example.com/",
            sigil_kernel::EgressNetworkRoute::Direct,
            vec![sigil_kernel::EgressDataCategory::SearchQuery],
        )?,
        receipt_tx,
    })?;

    assert_eq!(
        app.timeline_viewport_rows(),
        before.saturating_sub(usize::from(EGRESS_DISCLOSURE_HEIGHT))
    );
    Ok(())
}

#[test]
fn follow_up_partition_uses_the_same_status_height_for_viewport_and_rendering() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(100, 30);
    app.runtime.is_busy = true;
    app.composer.input = "inspect this after the current run".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::QueueConversationInput { .. })
    ));
    let expected = crate::ui::live_transcript_rows_for_app(Rect::new(0, 0, 100, 30), &app);

    assert_eq!(app.timeline_viewport_rows(), expected);
    assert!(app.queue_strip_rows() > 0);
    Ok(())
}

#[test]
fn long_task_frames_reuse_versioned_view_cache_without_store_scan_or_reducer_replay() -> Result<()>
{
    const STEP_COUNT: usize = 250;
    let temp = tempfile::tempdir()?;
    let config = sigil_kernel::RootConfig {
        workspace: sigil_kernel::WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    let task_id = sigil_kernel::TaskId::new("long_task")?;
    let parent_session_ref = sigil_kernel::SessionRef::new_relative("parent.jsonl")?;
    let steps = (0..STEP_COUNT)
        .map(|index| {
            Ok(sigil_kernel::TaskStepSpec {
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                title: format!("Inspect module {index}"),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref,
            objective: "Inspect a long workspace in parallel".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: steps.clone(),
            reason: None,
        })),
    ];
    for (index, step) in steps.iter().enumerate() {
        for status in [
            sigil_kernel::TaskStepStatus::Pending,
            sigil_kernel::TaskStepStatus::Running,
            if index + 1 == STEP_COUNT {
                sigil_kernel::TaskStepStatus::Running
            } else {
                sigil_kernel::TaskStepStatus::Completed
            },
        ] {
            entries.push(SessionLogEntry::Control(ControlEntry::TaskStep(
                sigil_kernel::TaskStepEntry {
                    task_id: task_id.clone(),
                    plan_version: 1,
                    step_id: step.step_id.clone(),
                    role: step.role,
                    status,
                    title: Some(step.title.clone()),
                    summary: None,
                    reason: None,
                },
            )));
        }
    }
    app.sync_current_session_state(entries);
    let cache_before = app.session_view_cache_evidence();
    assert_eq!(cache_before.1, 2 + STEP_COUNT * 3);

    let unreadable_session_path = temp.path().join("session-log-is-a-directory");
    std::fs::create_dir(&unreadable_session_path)?;
    app.session_log_path = unreadable_session_path;
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend)?;
    for _ in 0..3 {
        terminal.draw(|frame| crate::ui::render(frame, &app))?;
    }

    assert_eq!(app.session_view_cache_evidence(), cache_before);
    Ok(())
}

fn sync_child_agent_for_transcript_tests(app: &mut AppState) -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let child_session_ref =
        sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;
    let thread_id = sigil_kernel::AgentThreadId::new("child_1")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "inspect".to_owned(),
                display_name: Some("repo read".to_owned()),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: child_session_ref.clone(),
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::System,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id,
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: child_session_ref,
                profile_id,
                profile_snapshot_id: snapshot_id.clone(),
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: snapshot_id,
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    model_ref: None,
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new("/tmp/workspace")?,
                    effective_tool_scope_hash: "sha256:tools".to_owned(),
                    effective_permission_policy_hash: "sha256:permissions".to_owned(),
                    effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                    provider_capability_hash: "sha256:provider".to_owned(),
                    model_visible_agent_index_hash: Some("sha256:index".to_owned()),
                    budget_policy_hash: "sha256:budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: "inspect".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Background,
                invocation_source: sigil_kernel::AgentInvocationSource::Task,
                display_name: Some("repo read".to_owned()),
                created_at_ms: None,
            },
        )),
    ]);
    app.activate_agent_from_command("child_1")?;
    Ok(())
}

fn transcript_plain(lines: Vec<Line<'static>>) -> String {
    lines
        .into_iter()
        .flat_map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcript_plain_lines(lines: Vec<Line<'static>>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn column_selection_helpers_cover_empty_and_zero_width_edges() {
    let unchanged = selected_timeline_line_columns(Line::from(Span::raw("abc")), 2..2);
    assert_eq!(unchanged.spans.len(), 1);
    assert_eq!(unchanged.spans[0].content.as_ref(), "abc");

    let selected = selected_timeline_line_columns(Line::from(Span::raw("\u{0301}a")), 0..1);
    let selected_text = selected
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(selected_text, "a");
    assert!(
        selected
            .spans
            .iter()
            .any(|span| span.style.bg == Some(Color::Rgb(242, 171, 122)))
    );

    assert_eq!(text_by_display_columns("abc", 2, 2), "");
    assert_eq!(text_by_display_columns("\u{0301}a", 0, 1), "a");
}

#[test]
fn short_transcript_is_visible_in_the_application_timeline() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 32);
    app.push_timeline(TimelineRole::User, "hello");
    app.push_timeline(TimelineRole::Assistant, "latest answer");

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
fn long_transcript_keeps_render_cache_consistent_without_front_trim() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    for index in 0..450 {
        app.push_timeline(TimelineRole::Notice, format!("notice {index}"));
    }

    assert!(app.timeline.len() >= 450);
    assert!((0..app.timeline.len()).all(|index| app.timeline_entry_render_range(index).is_some()));
    let rendered = app.timeline_plain_lines().join("\n");
    assert!(rendered.contains("notice 0"));
    assert!(rendered.contains("notice 449"));
}

#[test]
fn layout_snapshot_live_text_rows_stay_in_bounds_after_resize_and_append() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert!(app.set_terminal_size(44, 14));
    for index in 0..12 {
        app.push_timeline(
            TimelineRole::Assistant,
            format!("long assistant message {index} that wraps differently by width"),
        );
    }

    let narrow = LayoutSnapshot::from_app(Rect::new(0, 0, 44, 14), &app);
    assert!(!narrow.live_text_rows.is_empty());
    assert!(
        narrow
            .live_text_rows
            .iter()
            .all(|row| app.timeline_plain_line(row.line_index).is_some())
    );

    assert!(app.set_terminal_size(120, 14));
    app.push_timeline(TimelineRole::Notice, "after resize append");
    let wide = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 14), &app);
    assert!(!wide.live_text_rows.is_empty());
    assert!(
        wide.live_text_rows
            .iter()
            .all(|row| app.timeline_plain_line(row.line_index).is_some())
    );
}

#[test]
fn timeline_selection_clears_on_resize_rerender_to_avoid_stale_lines() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert!(app.set_terminal_size(90, 12));
    app.push_timeline(
        TimelineRole::Assistant,
        "long assistant message that will wrap differently after resize",
    );
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 90, 12), &app);

    let line_index = layout
        .live_text_rows
        .first()
        .expect("expected live text row")
        .line_index;
    assert!(!app.begin_timeline_text_selection_at(line_index, 0));
    assert!(app.update_timeline_text_selection(line_index));
    assert!(app.selected_timeline_line_range().is_some());

    assert!(app.set_terminal_size(36, 12));
    assert!(app.selected_timeline_line_range().is_none());
    assert!(app.selected_timeline_text().is_none());
}

#[test]
fn reasoning_delta_keeps_latest_thinking_expanded_until_tool_starts() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5".to_owned(),
    ))?;

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
        entry.role == TimelineRole::Thinking
            && entry.text
                == "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5"
    }));
    let streaming = app.transcript_lines(20);
    let streaming_plain = transcript_plain(streaming.clone());
    assert!(streaming_plain.contains("thinking"));
    assert!(!streaming_plain.contains("thought"));
    assert!(app.collapsible_thinking_entry_indices().is_empty());
    assert!(streaming.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    }));
    assert!(streaming.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 5"))
    }));

    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let collapsed = app.transcript_lines(20);
    let collapsed_plain = transcript_plain(collapsed.clone());
    assert!(collapsed_plain.contains("thought"));
    assert!(!collapsed_plain.contains("thinking"));
    assert_eq!(app.collapsible_thinking_entry_indices().len(), 1);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 5"))
    }));
    Ok(())
}

#[test]
fn empty_reasoning_delta_does_not_create_empty_thinking_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(String::new()))?;
    app.handle(RunEvent::ReasoningDelta("\n  \t".to_owned()))?;

    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking)
    );

    app.handle(RunEvent::ReasoningDelta("Still".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(" ".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("running".to_owned()))?;

    let thinking = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Thinking)
        .collect::<Vec<_>>();
    assert_eq!(thinking.len(), 1);
    assert_eq!(thinking[0].text, "Still running");
    Ok(())
}

#[test]
fn ctrl_t_toggles_thinking_block_expansion() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 3\nplanning step 4\nplanning step 5".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("5 lines"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 1"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("3 lines hidden"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 5"))
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
            .any(|span| span.content.as_ref().contains("planning step 5"))
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
            .any(|span| span.content.as_ref().contains("planning step 5"))
    }));
    assert_eq!(app.last_notice(), Some("thinking collapsed"));
    Ok(())
}

#[test]
fn ctrl_t_toggles_thinking_from_activity_without_tool_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5"
            .to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    app.active_pane = PaneFocus::Activity;
    app.timeline_state.selected_tool_activity_key = None;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    assert_eq!(app.last_notice(), Some("thinking expanded"));
    let expanded = transcript_plain(app.transcript_lines(20));
    assert!(expanded.contains("Ctrl-T collapse"));
    assert!(expanded.contains("planning step 5"));
    Ok(())
}

#[test]
fn ctrl_o_expands_latest_diagram_source_and_copy_preserves_raw_markdown() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let response = "Architecture:\n\n```mermaid\nflowchart TD\nA --> B\n```\n\nDone.".to_owned();
    app.push_timeline(TimelineRole::Assistant, response.clone());

    let collapsed = transcript_plain(app.transcript_lines(48));
    assert!(collapsed.contains("diagram"));
    assert!(collapsed.contains("flowchart"));
    assert!(!collapsed.contains("A --> B"));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;

    let expanded = transcript_plain(app.transcript_lines(48));
    assert!(expanded.contains("A --> B"));
    assert_eq!(
        app.last_notice(),
        Some("diagram source expanded · Ctrl-L copies response")
    );

    let action = app
        .request_copy_selection_or_latest_response()
        .expect("latest response should be copyable");
    assert!(matches!(
        action,
        AppAction::CopyToClipboard { text } if text == response
    ));
    Ok(())
}

#[test]
fn single_line_thinking_block_stays_visible_without_toggle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("single visible step".to_owned()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let rendered = transcript_plain(app.transcript_lines(20));
    assert!(rendered.contains("1 line"));
    assert!(!rendered.contains("1 line hidden"));
    assert!(!rendered.contains("Ctrl-T expand"));
    assert!(rendered.contains("single visible step"));
    let thinking_view_events_before = app
        .events
        .iter()
        .filter(|event| event.label == "thinking:view")
        .count();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    let rendered_after = transcript_plain(app.transcript_lines(20));
    assert!(rendered_after.contains("single visible step"));
    assert!(!rendered_after.contains("Ctrl-T collapse"));
    assert_eq!(
        app.events
            .iter()
            .filter(|event| event.label == "thinking:view")
            .count(),
        thinking_view_events_before
    );
    assert_ne!(app.last_notice(), Some("thinking expanded"));
    assert_ne!(app.last_notice(), Some("thinking collapsed"));
    Ok(())
}

#[test]
fn short_thinking_block_stays_visible_without_toggle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let rendered = transcript_plain(app.transcript_lines(20));
    assert!(rendered.contains("4 lines"));
    assert!(rendered.contains("planning step 4"));
    assert!(!rendered.contains("hidden"));
    assert!(!rendered.contains("Ctrl-T expand"));
    assert!(app.collapsible_thinking_entry_indices().is_empty());
    Ok(())
}

#[test]
fn thinking_entry_toggle_handles_missing_uncollapsible_and_global_override() -> Result<()> {
    let mut short_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    short_app.push_timeline(TimelineRole::Thinking, " \n ");
    let short_index = short_app
        .timeline
        .iter()
        .position(|entry| entry.role == TimelineRole::Thinking)
        .expect("expected thinking entry");

    assert!(!short_app.toggle_thinking_entry(usize::MAX));
    assert!(!short_app.toggle_thinking_entry(short_index));

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5"
            .to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-2".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let entry_index = app.collapsible_thinking_entry_indices()[0];

    app.toggle_thinking_block_mode();
    let expanded = transcript_plain(app.transcript_lines(20));
    assert!(expanded.contains("Ctrl-T collapse"));
    assert!(expanded.contains("planning step 5"));

    assert!(app.toggle_thinking_entry(entry_index));
    let collapsed = transcript_plain(app.transcript_lines(20));
    assert!(collapsed.contains("Ctrl-T expand"));
    assert!(!collapsed.contains("planning step 5"));
    Ok(())
}

#[test]
fn ctrl_t_expands_thinking_when_tool_selection_is_stale_in_composer() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r##"{
  "call_id": "call-first",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"##,
    );
    let tool_key = app
        .timeline_state
        .selected_tool_activity_key
        .clone()
        .expect("tool card should be selected");
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(tool_key.clone())
    );
    assert_eq!(app.active_pane, PaneFocus::Composer);

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 2\nplanning step 3\nplanning step 4\nplanning step 5".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-2".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    assert_eq!(app.last_notice(), Some("thinking expanded"));
    assert!(
        !app.timeline_state
            .expanded_tool_activity_keys
            .contains(&tool_key)
    );
    let expanded = app.transcript_lines(20);
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    }));
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 5"))
    }));
    Ok(())
}

#[test]
fn tool_result_is_rendered_as_multiline_json_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolResult(sigil_kernel::ToolResult::ok(
        "call-1".to_owned(),
        "ls".to_owned(),
        "[\".git\",\"Cargo.toml\"]".to_owned(),
        sigil_kernel::ToolResultMeta::default(),
    )))?;

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
fn batched_streaming_text_deltas_rerender_once_after_drain() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.begin_timeline_render_batch();
    app.handle(RunEvent::TextDelta("```rust\n".to_owned()))?;
    let revision_after_first_delta = app.timeline_revision();
    app.handle(RunEvent::TextDelta("fn main() {}\n".to_owned()))?;
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
fn partial_provider_output_discard_replaces_tui_live_text_and_reasoning() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::TextDelta("discarded visible text".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "discarded visible reasoning".to_owned(),
    ))?;
    assert!(
        app.timeline_plain_lines()
            .join("\n")
            .contains("discarded visible text")
    );

    app.handle(RunEvent::ProviderTurnPartialOutputDiscarded(
        sigil_kernel::PublicProviderTurnPartialOutputDiscardedViewV1 {
            text_discarded: true,
            reasoning_discarded: true,
            tool_request_discarded: false,
        },
    ))?;
    app.handle(RunEvent::TextDelta("replacement answer".to_owned()))?;

    let rendered = app.timeline_plain_lines().join("\n");
    assert!(!rendered.contains("discarded visible text"));
    assert!(!rendered.contains("discarded visible reasoning"));
    assert!(rendered.contains("Discarded incomplete provider text before recovery"));
    assert!(rendered.contains("replacement answer"));
    Ok(())
}

#[test]
fn streaming_assistant_defers_code_highlight_until_finished() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let plain_code_style = Style::default()
        .fg(Color::Rgb(236, 240, 246))
        .bg(Color::Rgb(28, 33, 41));

    app.handle(RunEvent::TextDelta("```rust\n".to_owned()))?;
    app.handle(RunEvent::TextDelta("fn main() {}\n```\n".to_owned()))?;

    let streaming_style =
        timeline_span_style_containing(&app, "fn main").expect("streaming fn should render");
    assert_eq!(streaming_style, plain_code_style);

    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let finished_style =
        timeline_span_style_containing(&app, "fn").expect("finished fn should render");
    assert_ne!(finished_style, plain_code_style);
    Ok(())
}

#[test]
fn agent_tool_pre_tool_streaming_text_is_thinking_not_assistant() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::TextDelta("parent pre-tool analysis".to_owned()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-agent-1".to_owned(),
        name: "spawn_agent".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let entry = app
        .timeline
        .iter()
        .find(|entry| entry.text == "parent pre-tool analysis")
        .expect("streaming entry should remain");
    assert_eq!(entry.role, TimelineRole::Thinking);
    assert_eq!(entry.text, "parent pre-tool analysis");
    Ok(())
}

#[test]
fn empty_streaming_text_before_agent_tool_does_not_create_empty_thought() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::TextDelta(String::new()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-agent-empty".to_owned(),
        name: "spawn_agent".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking && entry.text.trim().is_empty())
    );
    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(!rendered.contains("thought  1 line"));
    Ok(())
}

#[test]
fn assistant_message_before_tool_remains_visible() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("checking provider shape".to_owned()),
        Vec::new(),
    )))?;
    let before_tool = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(
        before_tool
            .iter()
            .any(|line| line.contains("checking provider shape"))
    );
    assert!(
        !before_tool
            .iter()
            .any(|line| line.contains("• checking provider shape"))
    );

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1".to_owned(),
        "read_file".to_owned(),
        "file contents",
        ToolResultMeta::default(),
    )))?;

    let after_tool = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(
        after_tool
            .iter()
            .any(|line| line.contains("checking provider shape"))
    );
    assert!(
        after_tool
            .iter()
            .any(|line| line.contains("• checking provider shape"))
    );

    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("final answer".to_owned()),
        Vec::new(),
    )))?;

    let after_final = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(after_final.iter().any(|line| line.contains("final answer")));
    assert!(
        !after_final
            .iter()
            .any(|line| line.contains("• final answer"))
    );
    Ok(())
}

#[test]
fn live_reasoning_trace_before_final_answer_stays_visible_as_thinking() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "draft summary that should stay visible".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some("final answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(rendered.contains("final answer"));
    assert!(rendered.contains("draft summary that should stay visible"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Thinking)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn live_rejected_final_candidate_is_removed_before_continuation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::TextDelta(
        "candidate summary before facts".to_owned(),
    ))?;
    app.handle(RunEvent::Notice(
        "recorded run facts added before final answer; continuing".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some("accepted final summary".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(!rendered.contains("candidate summary before facts"));
    assert!(rendered.contains("accepted final summary"));
    assert!(rendered.contains("recorded run facts added before final answer"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn terminal_final_blocker_notice_removes_the_rejected_candidate() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::TextDelta(
        "candidate summary while a joined agent is still pending".to_owned(),
    ))?;
    app.handle(RunEvent::Notice(
        "pending agent state still blocks final answer; ending this run without another provider retry"
            .to_owned(),
    ))?;

    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(!rendered.contains("candidate summary while a joined agent is still pending"));
    assert!(rendered.contains("pending agent state still blocks final answer"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        0
    );
    Ok(())
}

#[test]
fn rejected_final_candidate_notice_keeps_finished_thinking_visible() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "reasoning that should stay visible".to_owned(),
    ))?;
    app.handle(RunEvent::TextDelta(
        "candidate summary before facts".to_owned(),
    ))?;
    app.handle(RunEvent::Notice(
        "recorded run facts added before final answer; continuing".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some("accepted final summary".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(rendered.contains("reasoning that should stay visible"));
    assert!(!rendered.contains("candidate summary before facts"));
    assert!(rendered.contains("recorded run facts added before final answer"));
    assert!(rendered.contains("accepted final summary"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Thinking)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn live_reasoning_trace_between_tools_stays_visible_when_final_answer_arrives() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.push_timeline(TimelineRole::User, "inspect and summarize");
    app.handle(RunEvent::ReasoningDelta(
        "first draft summary that should remain visible".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-read".to_owned(),
        name: "read_file".to_owned(),
        args_json: json!({"path":"src/lib.rs"}).to_string(),
    }))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-read",
        "read_file",
        "file contents",
        ToolResultMeta::default(),
    )))?;
    app.handle(RunEvent::ReasoningDelta(
        "second draft summary that should remain visible".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            Some("final answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(rendered.contains("final answer"));
    assert!(rendered.contains("first draft summary that should remain visible"));
    assert!(rendered.contains("second draft summary that should remain visible"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Thinking)
            .count(),
        2
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Tool)
    );
    Ok(())
}

#[test]
fn live_reasoning_trace_before_agent_poll_tool_is_not_rendered() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "Still running. Let me poll again.".to_owned(),
    ))?;
    app.handle(RunEvent::AssistantMessage(
        ModelMessage::assistant_with_kind(
            None,
            vec![ToolCall {
                id: "call-wait".to_owned(),
                name: "wait_agent".to_owned(),
                args_json: json!({"thread_id":"agent_chat_1"}).to_string(),
            }],
            AssistantMessageKind::ToolPreamble,
        ),
    ))?;

    let rendered = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(!rendered.contains("Still running. Let me poll again."));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking)
    );
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

#[test]
fn timeline_cache_and_scroll_edges_cover_empty_and_guard_paths() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.timeline.clear();
    app.rebuild_timeline_render_store();

    assert_eq!(app.effective_timeline_render_len(), 0);
    assert_eq!(app.visible_timeline_render_range(10), 0..0);
    assert_eq!(
        app.transcript_lines(10)
            .into_iter()
            .map(|line| line
                .spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>())
            .collect::<Vec<_>>(),
        vec![
            "no messages yet".to_owned(),
            "send a prompt to start".to_owned()
        ]
    );

    app.rerender_timeline_entry(99);
    app.append_timeline_render_store_entry(0);

    app.timeline.push(crate::timeline::TimelineEntry {
        role: TimelineRole::Notice,
        text: "manual notice".to_owned(),
    });
    app.append_timeline_render_store_entry(0);
    assert!(app.timeline_entry_render_range(0).is_some());
    assert!(app.visible_timeline_render_range(10).end <= app.timeline_render_line_count());

    app.timeline.clear();
    app.rebuild_timeline_render_store();
    app.push_timeline(TimelineRole::Assistant, "streaming answer");
    app.timeline_state.streaming_assistant_index = Some(0);
    app.runtime.is_busy = true;
    assert_eq!(app.visible_timeline_render_range(10), 0..1);
    Ok(())
}

#[test]
fn info_rail_visibility_rebuilds_the_timeline_for_the_actual_content_width() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.timeline = vec![TimelineEntry {
        role: TimelineRole::Assistant,
        text: "a long response row ".repeat(20),
    }];
    app.rebuild_timeline_render_store();
    let visible_count = app.timeline_render_line_count();
    let visible_revision = app.timeline_revision();

    app.toggle_info_rail_visibility();

    assert!(!app.info_rail_visible());
    assert!(app.timeline_revision() > visible_revision);
    assert!(app.timeline_render_line_count() < visible_count);
}

#[test]
fn info_rail_visibility_preserves_the_main_history_anchor() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 12);
    let text = (0..240)
        .map(|index| format!("rail-token-{index:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    app.push_timeline(TimelineRole::Assistant, text);
    let target_line = app
        .timeline_plain_lines()
        .iter()
        .position(|line| line.contains("rail-token-120"))
        .expect("visible-rail render should contain the marker");
    let viewport = app.timeline_viewport_rows();
    app.timeline_scroll_back = app
        .effective_timeline_render_len()
        .saturating_sub(target_line.saturating_add(viewport));
    assert_eq!(
        app.visible_timeline_render_range(viewport).start,
        target_line
    );

    app.toggle_info_rail_visibility();

    let visible = app.visible_timeline_render_range(app.timeline_viewport_rows());
    let top_after = app
        .timeline_plain_line(visible.start)
        .expect("reflowed top line after hiding the rail");
    assert!(!app.info_rail_visible());
    assert!(
        top_after.contains("rail-token-120"),
        "rail reflow must preserve the logical top marker: {top_after:?}"
    );
}

#[test]
fn narrow_terminal_timeline_projection_uses_the_real_content_width() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(12, 20);
    app.push_timeline(TimelineRole::Assistant, "abcdefghijklmnop".to_owned());

    let lines = app.timeline_plain_lines();
    assert!(
        lines.len() > 1,
        "narrow content should wrap in the render store"
    );
    assert!(
        lines
            .iter()
            .all(|line| crate::ui::terminal_cell_width(line) <= 10),
        "render-store rows must already fit the ten-cell live-panel width: {lines:?}"
    );
}

#[test]
fn history_inspection_keeps_its_top_anchor_during_streaming_and_reflow() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..24 {
        app.push_timeline(TimelineRole::Assistant, format!("history row {index}"));
    }

    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
    let initial_scroll_back = app.timeline_scroll_back;
    assert_eq!(
        app.visible_timeline_render_range(app.timeline_viewport_rows())
            .start,
        0
    );

    app.append_assistant_delta("streaming tail ");
    app.append_assistant_delta(&"more output ".repeat(80));
    assert!(app.timeline_scroll_back > initial_scroll_back);
    assert_eq!(
        app.visible_timeline_render_range(app.timeline_viewport_rows())
            .start,
        0
    );

    app.set_terminal_size(32, 12);
    assert_eq!(
        app.visible_timeline_render_range(app.timeline_viewport_rows())
            .start,
        0
    );

    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL))?;
    assert!(app.timeline_at_live_tail());
    Ok(())
}

#[test]
fn history_anchor_survives_height_only_resize() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..60 {
        app.push_timeline(TimelineRole::Assistant, format!("height row {index}"));
    }
    let target_start: usize = 12;
    let viewport = app.timeline_viewport_rows();
    app.timeline_scroll_back = app
        .effective_timeline_render_len()
        .saturating_sub(target_start.saturating_add(viewport));
    assert_eq!(
        app.visible_timeline_render_range(viewport).start,
        target_start
    );
    let top_before = app.timeline_plain_line(target_start).map(str::to_owned);

    app.set_terminal_size(80, 18);
    let visible = app.visible_timeline_render_range(app.timeline_viewport_rows());

    assert_eq!(visible.start, target_start);
    assert_eq!(
        app.timeline_plain_line(visible.start),
        top_before.as_deref()
    );
}

#[test]
fn history_anchor_maps_a_long_entry_by_logical_content_across_width_reflow() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(30, 12);
    let text = (0..240)
        .map(|index| format!("token{index:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    app.push_timeline(TimelineRole::Assistant, text);
    let target_line = app
        .timeline_plain_lines()
        .iter()
        .position(|line| line.contains("token120"))
        .expect("narrow render should contain the marker");
    let viewport = app.timeline_viewport_rows();
    app.timeline_scroll_back = app
        .effective_timeline_render_len()
        .saturating_sub(target_line.saturating_add(viewport));
    assert_eq!(
        app.visible_timeline_render_range(viewport).start,
        target_line
    );

    app.set_terminal_size(120, 12);
    let visible = app.visible_timeline_render_range(app.timeline_viewport_rows());
    let top_after = app
        .timeline_plain_line(visible.start)
        .expect("reflowed top line");

    assert!(
        top_after.contains("token120"),
        "logical marker should remain on the anchored top row: {top_after:?}"
    );
}

#[test]
fn info_rail_visibility_rerenders_the_active_child_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    sync_child_agent_for_transcript_tests(&mut app)?;
    let timeline_entries = vec![TimelineEntry {
        role: TimelineRole::Assistant,
        text: "a long child response row ".repeat(20),
    }];
    let rendered_body_lines = app.render_child_timeline_body_lines(&timeline_entries);
    let visible_count = rendered_body_lines.len();
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries,
        rendered_body_lines,
        total_timeline_entries: 1,
        transcript_truncated: false,
        load_error: None,
    });

    app.toggle_info_rail_visibility();

    let hidden_count = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("child transcript")
        .rendered_body_lines
        .len();
    assert!(!app.info_rail_visible());
    assert!(hidden_count < visible_count);
    Ok(())
}

#[test]
fn child_history_anchor_survives_live_append_and_width_reflow() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(64, 24);
    sync_child_agent_for_transcript_tests(&mut app)?;
    let timeline_entries = (0..30)
        .map(|index| TimelineEntry {
            role: TimelineRole::Assistant,
            text: format!("child-anchor-{index:02} {}", "payload ".repeat(10)),
        })
        .collect::<Vec<_>>();
    let rendered_body_lines = app.render_child_timeline_body_lines(&timeline_entries);
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries,
        rendered_body_lines,
        total_timeline_entries: 30,
        transcript_truncated: false,
        load_error: None,
    });
    app.timeline_scroll_back = app.max_timeline_scroll_back() / 2;
    assert!(app.timeline_scroll_back > 0);
    let first_visible_anchor = |app: &AppState| {
        let lines = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
        lines
            .iter()
            .find_map(|line| {
                let start = line.find("child-anchor-")?;
                Some(line[start..].chars().take(15).collect::<String>())
            })
            .unwrap_or_else(|| panic!("a child anchor should be visible in {lines:?}"))
    };
    let before = first_visible_anchor(&app);

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_mut()
        .expect("child transcript");
    transcript.timeline_entries.push(TimelineEntry {
        role: TimelineRole::Assistant,
        text: "child-anchor-30 appended tail".to_owned(),
    });
    transcript.total_timeline_entries = 31;
    app.rerender_active_agent_child_transcript();
    assert_eq!(first_visible_anchor(&app), before);

    app.set_terminal_size(38, 40);
    assert_eq!(first_visible_anchor(&app), before);
    Ok(())
}

#[test]
fn child_history_anchor_survives_file_reload_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(60, 24);
    app.session_log_path = temp.path().join("parent.jsonl");
    sync_child_agent_for_transcript_tests(&mut app)?;
    let child_path = temp.path().join("children/task_1/step_1-child_1.jsonl");
    let child_store = JsonlSessionStore::new(&child_path)?;
    for index in 0..96 {
        child_store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some(format!("reload-anchor-{index:02}")),
            Vec::new(),
        )))?;
    }
    assert!(app.reload_active_agent_child_transcript());
    app.timeline_scroll_back = app.max_timeline_scroll_back() / 2;
    assert!(app.timeline_scroll_back > 0);
    let visible_anchor = |app: &AppState| {
        transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()))
            .into_iter()
            .find(|line| line.contains("reload-anchor-"))
            .expect("a reload anchor should be visible")
    };
    let before = visible_anchor(&app);
    let captured_identity = match app.capture_timeline_history_anchor() {
        Some(TimelineHistoryAnchor::Child { entry_identity, .. }) => entry_identity,
        other => panic!("expected child history anchor, got {other:?}"),
    };
    assert!(captured_identity.is_some());

    for index in 96..106 {
        child_store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some(format!("reload-anchor-{index:02}")),
            Vec::new(),
        )))?;
    }
    assert!(app.reload_active_agent_child_transcript());
    assert_eq!(
        visible_anchor(&app),
        before,
        "captured identity: {captured_identity:?}, scroll_back: {}",
        app.timeline_scroll_back
    );
    Ok(())
}

#[test]
fn child_history_anchor_disambiguates_repeated_entries_by_context() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(64, 40);
    sync_child_agent_for_transcript_tests(&mut app)?;
    let timeline_entries = (0..24)
        .flat_map(|index| {
            [
                TimelineEntry {
                    role: TimelineRole::Notice,
                    text: "Started shell".to_owned(),
                },
                TimelineEntry {
                    role: TimelineRole::Notice,
                    text: format!("result-anchor-{index:02} {}", "detail ".repeat(8)),
                },
            ]
        })
        .collect::<Vec<_>>();
    let rendered_body_lines = app.render_child_timeline_body_lines(&timeline_entries);
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries,
        rendered_body_lines,
        total_timeline_entries: 48,
        transcript_truncated: false,
        load_error: None,
    });
    let mut expected_result = None;
    for scroll_back in 1..=app.max_timeline_scroll_back() {
        app.timeline_scroll_back = scroll_back;
        let repeated_entry_is_anchored = matches!(
            app.capture_timeline_history_anchor(),
            Some(TimelineHistoryAnchor::Child {
                entry_identity: Some((TimelineRole::Notice, ref text)),
                ..
            }) if text == "Started shell"
        );
        if !repeated_entry_is_anchored {
            continue;
        }
        expected_result =
            transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()))
                .into_iter()
                .find_map(|line| {
                    let start = line.find("result-anchor-")?;
                    Some(line[start..].chars().take(16).collect::<String>())
                });
        if expected_result.is_some() {
            break;
        }
    }
    let expected_result = expected_result.expect("a repeated entry anchor should be selectable");

    app.set_terminal_size(48, 40);
    let actual_result = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()))
        .into_iter()
        .find_map(|line| {
            let start = line.find("result-anchor-")?;
            Some(line[start..].chars().take(16).collect::<String>())
        })
        .expect("the anchored repeated entry should keep its following result visible");
    assert_eq!(actual_result, expected_result);
    Ok(())
}

#[test]
fn child_agent_transcript_lines_cover_load_states_and_viewport_edges() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;

    let header_only = transcript_plain(app.transcript_lines(1));
    assert!(header_only.contains("agent view"));
    assert!(!header_only.contains("session:"));

    app.agent_panel.active_child_transcript = None;
    let unloaded = transcript_plain(app.transcript_lines(8));
    assert!(unloaded.contains("repo read"));
    assert!(unloaded.contains("child session not loaded"));

    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: Some("permission denied opening child session".to_owned()),
    });
    let load_error = transcript_plain(app.transcript_lines(8));
    assert!(load_error.contains("load error: permission denied"));
    assert!(load_error.contains("path: children/task_1/step_1-child_1.jsonl"));

    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: None,
    });
    let empty = transcript_plain(app.transcript_lines(8));
    assert!(empty.contains("child session has no transcript messages yet"));

    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: vec![
            TimelineEntry {
                role: TimelineRole::User,
                text: "child prompt".to_owned(),
            },
            TimelineEntry {
                role: TimelineRole::Assistant,
                text: "child answer".to_owned(),
            },
        ],
        rendered_body_lines: vec![Line::from("child prompt"), Line::from("child answer")],
        total_timeline_entries: 2,
        transcript_truncated: false,
        load_error: None,
    });
    let restored = transcript_plain(app.transcript_lines(12));
    assert!(restored.contains("child prompt"));
    assert!(restored.contains("child answer"));
    Ok(())
}

#[test]
fn child_header_and_fallback_rows_are_prewrapped_to_the_live_panel_width() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(40, 10);
    sync_child_agent_for_transcript_tests(&mut app)?;
    let long_ref = sigil_kernel::SessionRef::new_relative(format!(
        "children/{}/child-session.jsonl",
        "long-segment".repeat(8)
    ))?;
    app.agent_panel.active_view = AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: long_ref,
    };
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from(format!(
            "children/{}/missing-child.jsonl",
            "missing-segment".repeat(8)
        )),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: Some(
            "permission denied while opening a very long child transcript path".repeat(3),
        ),
    });

    let width = 38;
    let rendered = app.transcript_lines(usize::MAX);
    assert!(rendered.len() > 5);
    assert!(rendered.iter().all(|line| {
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        crate::ui::terminal_cell_width(&text) <= width
    }));
    assert_eq!(
        crate::ui::wrap_terminal_lines(rendered.clone(), width).len(),
        rendered.len()
    );
    assert!(app.transcript_lines(8).len() <= 8);
    Ok(())
}

#[test]
fn running_child_agent_transcript_keeps_latest_thinking_active() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let timeline_entries = vec![TimelineEntry {
        role: TimelineRole::Thinking,
        text: "child step 1\nchild step 2\nchild step 3\nchild step 4".to_owned(),
    }];
    let rendered_body_lines = app.render_child_timeline_body_lines(&timeline_entries);
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries,
        rendered_body_lines,
        total_timeline_entries: 1,
        transcript_truncated: false,
        load_error: None,
    });

    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("thinking"));
    assert!(!rendered.contains("thought"));
    assert!(rendered.contains("child step 4"));
    Ok(())
}

#[test]
fn child_agent_transcript_uses_cached_bounded_timeline_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let entries = (0..96)
        .map(|index| TimelineEntry {
            role: TimelineRole::Assistant,
            text: format!("child entry {index}"),
        })
        .collect::<Vec<_>>();
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: entries[16..].to_vec(),
        rendered_body_lines: entries[16..]
            .iter()
            .map(|entry| Line::from(entry.text.clone()))
            .collect(),
        total_timeline_entries: entries.len(),
        transcript_truncated: false,
        load_error: None,
    });

    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("showing latest 80 of 96 child transcript entries"));
    assert!(!rendered.contains("child entry 0"));
    assert!(rendered.contains("child entry 95"));
    Ok(())
}

#[test]
fn child_agent_transcript_reload_uses_tail_and_skips_unchanged_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    sync_child_agent_for_transcript_tests(&mut app)?;
    let child_path = temp.path().join("children/task_1/step_1-child_1.jsonl");
    let child_store = JsonlSessionStore::new(&child_path)?;
    for index in 0..96 {
        child_store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some(format!("child message {index}")),
            Vec::new(),
        )))?;
    }

    app.reload_active_agent_child_transcript();
    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("showing latest 80 of 96 child transcript entries"));
    assert!(!rendered.contains("child message 0"));
    assert!(rendered.contains("child message 95"));

    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_mut()
        .expect("child transcript");
    transcript.rendered_body_lines = vec![Line::from("cached sentinel")];
    app.reload_active_agent_child_transcript();
    let unchanged = transcript_plain(app.transcript_lines(16));

    assert!(unchanged.contains("cached sentinel"));
    Ok(())
}

#[test]
fn running_child_agent_parent_sync_does_not_reload_changing_transcript() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    sync_child_agent_for_transcript_tests(&mut app)?;
    let child_path = temp.path().join("children/task_1/step_1-child_1.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&child_path)?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("first child line".to_owned()),
        Vec::new(),
    )))?;
    app.reload_active_agent_child_transcript();
    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_mut()
        .expect("child transcript");
    transcript.rendered_body_lines = vec![Line::from("running cached transcript")];

    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("second child line".to_owned()),
        Vec::new(),
    )))?;
    app.refresh_active_agent_view_after_parent_sync();
    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("running cached transcript"));
    assert!(!rendered.contains("second child line"));
    assert!(app.poll_background_tasks());
    let refreshed = transcript_plain(app.transcript_lines(16));
    assert!(refreshed.contains("second child line"));
    assert!(!app.poll_background_tasks());
    Ok(())
}

#[test]
fn missing_child_agent_transcript_load_error_is_cached() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("children"), "not a directory")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: "missing_child".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative("children/missing.jsonl")?,
    };

    assert!(app.reload_active_agent_child_transcript());
    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("missing child transcript should record load error");
    assert!(transcript.load_error.is_some());
    assert_eq!(
        transcript.file_signature,
        super::super::ChildTranscriptFileSignature::empty()
    );
    assert!(transcript.timeline_entries.is_empty());
    assert!(transcript.rendered_body_lines.is_empty());

    assert!(!app.reload_active_agent_child_transcript());
    Ok(())
}

#[test]
fn thinking_mode_change_rerenders_the_active_child_cache() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let child_session_ref =
        sigil_kernel::SessionRef::new_relative("children/task_1/step_1-thinking.jsonl")?;
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: "thinking-child".to_owned(),
        child_session_ref,
    };
    let timeline_entries = vec![TimelineEntry {
        role: TimelineRole::Thinking,
        text: (1..=8)
            .map(|index| format!("reasoning line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }];
    let collapsed = app.render_child_timeline_body_lines(&timeline_entries);
    app.agent_panel.active_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-thinking.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: timeline_entries.clone(),
        rendered_body_lines: collapsed.clone(),
        total_timeline_entries: timeline_entries.len(),
        transcript_truncated: false,
        load_error: None,
    });

    app.toggle_thinking_block_mode();

    let expanded = &app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("active child cache")
        .rendered_body_lines;
    assert_ne!(expanded, &collapsed);
    assert_eq!(
        expanded,
        &app.render_child_timeline_body_lines(&timeline_entries)
    );
    Ok(())
}

#[test]
fn timeline_scroll_and_live_summary_edges_cover_pending_and_busy_states() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 6);
    for index in 0..12 {
        app.push_timeline(TimelineRole::Notice, format!("notice {index}"));
    }

    app.handle_mouse_scroll(true);
    assert!(app.timeline_scroll_back > 0);
    app.handle_mouse_scroll(false);
    assert_eq!(app.timeline_scroll_back, 0);

    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle_mouse_scroll(false);
    assert_eq!(app.approval.scroll_back, 3);
    app.handle_mouse_scroll(true);
    assert_eq!(app.approval.scroll_back, 0);

    app.active_pane = PaneFocus::Activity;
    app.scroll_active_pane(2);
    assert_eq!(app.approval.scroll_back, 0);
    app.unscroll_active_pane(4);
    assert_eq!(app.approval.scroll_back, 4);
    app.approval.pending = None;
    app.scroll_active_pane(5);
    assert_eq!(app.activity_scroll_back, 5);
    app.unscroll_active_pane(3);
    assert_eq!(app.activity_scroll_back, 2);

    assert!(app.live_activity_summary().is_none());
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Idle;
    assert_eq!(
        app.live_activity_summary()
            .map(|summary| (summary.label, summary.detail)),
        Some(("working".to_owned(), "waiting for next event".to_owned()))
    );
    app.runtime.run_phase = RunPhase::Tool("bash".to_owned());
    assert_eq!(
        app.live_activity_summary()
            .map(|summary| (summary.label, summary.detail)),
        Some(("tool".to_owned(), "running bash".to_owned()))
    );
    app.runtime.run_phase = RunPhase::Streaming;
    assert_eq!(
        app.live_activity_summary()
            .map(|summary| (summary.label, summary.detail)),
        Some(("streaming".to_owned(), "receiving response".to_owned()))
    );
    Ok(())
}

fn timeline_span_style_containing(app: &AppState, text: &str) -> Option<Style> {
    app.timeline_render_lines()
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains(text))
        .map(|span| span.style)
}

#[test]
fn tool_result_uses_live_approval_preview_snapshot_for_diff_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-call-1".to_owned(),
        approved: true,
        reason: None,
    })?;

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "write_file",
        "wrote note.txt",
        ToolResultMeta {
            bytes: Some(14),
            changed_files: vec!["note.txt".to_owned()],
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["tool_name"], "write_file");
    assert!(
        rendered["summary"].as_str().is_some_and(|summary| {
            summary.contains("diff +1 -1") && summary.contains("1 file")
        })
    );
    assert_eq!(rendered["diff"]["files"][0]["path"], "note.txt");
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "+gamma"))
            })
    );

    Ok(())
}

#[test]
fn control_preview_snapshot_event_caches_diff_for_tool_result() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &sample_approval_preview(),
        Default::default(),
        Some("preview-hash".to_owned()),
    );

    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "write_file",
        "wrote note.txt",
        ToolResultMeta {
            bytes: Some(14),
            changed_files: vec!["note.txt".to_owned()],
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["diff"]["summary"], "+1 -1 · 1 file");
    assert!(app.events.iter().any(|event| {
        event.label == "control"
            && event
                .detail
                .contains("preview call-1 write_file files=1 +1 -1")
    }));
    Ok(())
}

#[test]
fn approval_preview_snapshot_caches_diff_for_approved_tool_result() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-call-1".to_owned(),
        approved: true,
        reason: None,
    })?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "write_file",
        "wrote note.txt",
        ToolResultMeta {
            bytes: Some(14),
            changed_files: vec!["note.txt".to_owned()],
            ..ToolResultMeta::default()
        },
    )))?;

    let tool_entry = app
        .timeline
        .iter()
        .rev()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected approved write tool card");
    let rendered: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(rendered["tool_name"], "write_file");
    assert_eq!(rendered["diff"]["summary"], "+1 -1 · 1 file");
    assert_eq!(rendered["diff"]["files"][0]["path"], "note.txt");
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "-beta"))
                    && lines
                        .iter()
                        .any(|line| line.as_str().is_some_and(|text| text == "+gamma"))
            })
    );
    Ok(())
}

#[test]
fn delete_file_tool_result_uses_preview_snapshot_for_diff_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-delete-1",
        "delete_file",
        &sample_delete_approval_preview(),
        Default::default(),
        Some("delete-preview-hash".to_owned()),
    );

    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-delete-1",
        "delete_file",
        "deleted /workspace/note.txt",
        ToolResultMeta {
            bytes: Some(11),
            changed_files: vec!["note.txt".to_owned()],
            details: json!({
                "action": "delete",
                "call": {
                    "summary": "path=note.txt"
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["tool_name"], "delete_file");
    assert!(
        rendered["summary"].as_str().is_some_and(|summary| {
            summary.contains("diff +0 -2") && summary.contains("1 file")
        })
    );
    assert_eq!(rendered["metadata"]["details"]["action"], "delete");
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "-alpha"))
                    && lines
                        .iter()
                        .any(|line| line.as_str().is_some_and(|text| text == "-beta"))
            })
    );

    Ok(())
}

#[test]
fn error_tool_result_does_not_render_cached_preview_as_applied_diff() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-call-1".to_owned(),
        approved: false,
        reason: Some("denied".to_owned()),
    })?;

    app.handle(RunEvent::ToolResult(ToolResult::error(
        "call-1",
        "write_file",
        ToolErrorKind::ApprovalDenied,
        "tool execution denied by user: denied",
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["status"], "error");
    assert!(rendered.get("diff").is_none());
    Ok(())
}

#[test]
fn ctrl_u_and_ctrl_d_scroll_transcript_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
fn inspection_tool_entries_render_as_individual_activities() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-ls",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "preview_lines": ["[\"src/main.rs\"]"],
  "preview_value": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=crates"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-search",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "command=grep -n needle src/main.rs"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#,
    );

    let rendered = app.timeline_plain_lines().join("\n");
    let indices = app
        .tool_timeline_entry_indices()
        .expect("expected tool entries");
    let ranges = indices
        .iter()
        .map(|index| {
            app.timeline_entry_render_range(*index)
                .expect("expected render range")
        })
        .collect::<Vec<_>>();

    assert_eq!(ranges.len(), 3);
    assert_ne!(ranges[0], ranges[1]);
    assert_ne!(ranges[1], ranges[2]);
    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Searched needle in src/main.rs"));
    assert!(rendered.contains("Read README.md"));
}

#[test]
fn permission_notices_between_inspection_tools_remain_visible() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Notice,
        "permission ls subject=crates mode=allow",
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-ls",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "preview_lines": ["[\"src/main.rs\"]"],
  "preview_value": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=crates"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Notice,
        "permission read_file subject=README.md mode=allow",
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#,
    );

    let rendered = app.timeline_plain_lines().join("\n");
    let live = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));

    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("notice"));
    assert!(rendered.contains("permission ls subject=crates mode=allow"));
    assert!(rendered.contains("permission read_file subject=README.md mode=allow"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Read README.md"));
    assert!(live.contains("notice"));
    assert!(live.contains("permission read_file subject=README.md mode=allow"));
}

#[test]
fn file_changes_and_complex_bash_do_not_create_inspected_group() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-ls",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "preview_lines": ["[\"src/main.rs\"]"],
  "preview_value": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=crates"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-write",
  "tool_name": "write_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "metadata": {
    "changed_files": ["note.txt"],
    "details": {"call": {"summary": "path=note.txt"}}
  }
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-complex",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "command=grep needle src/main.rs | head"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#,
    );

    let rendered = app.timeline_plain_lines().join("\n");

    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Wrote note.txt"));
    assert!(rendered.contains("Ran grep needle src/main.rs | head"));
    assert!(rendered.contains("Read README.md"));
}

#[test]
fn mouse_scroll_moves_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 80, 12), &app);
    let outcome = app.handle_mouse_event(
        MouseInput {
            column: 1,
            row: 1,
            kind: MouseInputKind::ScrollUp,
            modifiers: KeyModifiers::NONE,
        },
        &layout,
    )?;
    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.timeline_scroll_back > 0);

    let outcome = app.handle_mouse_event(
        MouseInput {
            column: 1,
            row: 1,
            kind: MouseInputKind::ScrollDown,
            modifiers: KeyModifiers::NONE,
        },
        &layout,
    )?;
    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn default_open_large_diff_stays_stable_when_new_output_arrives() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 18);
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-delete-1",
        "delete_file",
        &sample_delete_approval_preview(),
        Default::default(),
        Some("delete-preview-hash".to_owned()),
    );
    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-delete-1",
        "delete_file",
        "deleted /workspace/note.txt",
        ToolResultMeta {
            bytes: Some(11),
            changed_files: vec!["note.txt".to_owned()],
            details: json!({
                "action": "delete",
                "call": {
                    "summary": "path=note.txt"
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;
    let first_revision = app.timeline_revision();

    for index in 0..5 {
        app.push_timeline(TimelineRole::Notice, format!("notice {index}"));
    }
    app.handle(RunEvent::TextDelta("stream one".to_owned()))?;
    app.handle(RunEvent::TextDelta("\nstream two".to_owned()))?;

    let rendered = app.timeline_plain_lines().join("\n");
    assert_eq!(rendered.matches("--- current/note.txt").count(), 1);
    assert_eq!(rendered.matches("-alpha").count(), 1);
    assert_eq!(rendered.matches("Deleted note.txt").count(), 1);
    assert_eq!(rendered.matches("path=note.txt").count(), 0);
    assert!(rendered.contains("stream one"));
    assert!(rendered.contains("stream two"));
    assert!(app.timeline_revision() > first_revision);
    Ok(())
}

#[test]
fn compaction_status_tracks_latest_prompt_tokens_instead_of_cumulative_totals() -> Result<()> {
    let mut config = test_config();
    config.agent.runtime_provider = "planned".to_owned();
    config.agent.model = "planned-model".to_owned();
    config.compaction.context_window_tokens = Some(100);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 70,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 70,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;
    assert_eq!(app.runtime.compaction_status, "soft");

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 20,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 20,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;

    assert_eq!(app.runtime.compaction_status, "ready");
    Ok(())
}

#[test]
fn context_usage_and_compaction_policy_share_effective_window() -> Result<()> {
    let mut config = test_config();
    config.agent.model = "deepseek-v4-pro".to_owned();
    config.compaction.context_window_tokens = Some(128_000);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 90_354,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 90_354,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;

    assert_eq!(
        app.context_usage_line(),
        "ctx: 9% · cache=0% · prompt 90.4K / 1.0M provider · soft at 700.0K"
    );
    assert_eq!(app.runtime.compaction_status, "ready");
    assert!(app.footer_status_line().contains("tok 90.4K"));
    assert!(app.footer_status_line().contains("ctx 9%"));
    assert!(
        app.usage_sidebar_lines().iter().any(
            |line| line == "policy: provider 1,000,000 · soft 70% (700.0K) · hard 92% (920.0K)"
        )
    );

    config.agent.runtime_provider = "custom".to_owned();
    config.agent.model = "custom-model".to_owned();
    let mut fallback_app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    fallback_app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 64_000,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 64_000,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;
    assert_eq!(
        fallback_app.context_usage_line(),
        "ctx: 50% · cache=0% · prompt 64.0K / 128.0K fallback · soft at 89.6K"
    );
    Ok(())
}

#[test]
fn usage_display_shows_session_and_delta_costs() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_hit_tokens: 75,
        cache_miss_tokens: 25,
        input_cost: 0.12,
        output_cost: 0.03,
        cache_savings: 0.45,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;

    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "session tok: input 100 · output 40")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "cache: 75% · save USD 0.4500")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: USD 0.1500")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "spent since opening: USD 0.1500")
    );
    assert!(
        app.footer_status_line()
            .contains("spent USD 0.1500 since opening / USD 0.1500 total")
    );
    assert!(
        !app.usage_sidebar_lines()
            .iter()
            .any(|line| line.contains('$'))
    );
    Ok(())
}

#[test]
fn session_delta_stats_reset_on_session_switch_and_follow_balance_currency() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.balance_snapshot = sigil_runtime::BalanceSnapshot {
        total: Some(12.34),
        currency: Some("CNY".to_owned()),
        available: true,
        status: "CNY 12.34".to_owned(),
    };
    let restored_path = app
        .workspace_root
        .join(".sigil/sessions/session-restored.jsonl");

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 10,
        cache_hit_tokens: 50,
        cache_miss_tokens: 50,
        input_cost: 0.20,
        output_cost: 0.05,
        cache_savings: 0.10,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;
    assert_eq!(app.runtime.session_delta_stats.input_cost, 0.20);

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: restored_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries: vec![SessionLogEntry::Control(ControlEntry::UsageSnapshot(
            UsageStats {
                prompt_tokens: 200,
                completion_tokens: 20,
                cache_hit_tokens: 120,
                cache_miss_tokens: 80,
                input_cost: 1.00,
                output_cost: 0.50,
                cache_savings: 2.00,
                system_fingerprint: None,
                cache_usage: None,
                pricing_snapshot: None,
            },
        ))],
    })?;

    assert_eq!(
        app.runtime.stats.input_cost + app.runtime.stats.output_cost,
        1.50
    );
    assert_eq!(
        app.runtime.session_delta_stats.input_cost + app.runtime.session_delta_stats.output_cost,
        0.0
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: CNY 10.8000")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "spent since opening: CNY 0.0000")
    );

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_hit_tokens: 75,
        cache_miss_tokens: 25,
        input_cost: 0.12,
        output_cost: 0.03,
        cache_savings: 0.45,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;

    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: CNY 11.8800")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "spent since opening: CNY 1.0800")
    );
    assert!(
        app.footer_status_line()
            .contains("spent CNY 1.0800 since opening / CNY 11.8800 total")
    );
    Ok(())
}

#[test]
fn usage_display_prefers_configured_cost_currency_over_balance_currency() -> Result<()> {
    let mut config = test_config();
    config.appearance.usage_cost_currency = sigil_kernel::UsageCostCurrency::Cny;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.runtime.balance_snapshot = sigil_runtime::BalanceSnapshot {
        total: Some(3.25),
        currency: Some("USD".to_owned()),
        available: true,
        status: "USD 3.25".to_owned(),
    };

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_hit_tokens: 75,
        cache_miss_tokens: 25,
        input_cost: 0.12,
        output_cost: 0.03,
        cache_savings: 0.45,
        system_fingerprint: None,
        cache_usage: None,
        pricing_snapshot: None,
    }))?;

    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: CNY 1.0800")
    );
    assert!(
        app.footer_status_line()
            .contains("spent CNY 1.0800 since opening / CNY 1.0800 total")
    );
    Ok(())
}

#[test]
fn activity_pane_keymap_preserves_composer_shortcuts_and_sidebar_navigation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Activity;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let rows = app.agent_sidebar_rows();
    assert!(rows.iter().any(|row| row.label == "main" && row.selected));

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Review);
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);

    sync_child_agent_for_transcript_tests(&mut app)?;
    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Agents;
    app.agent_panel.selected = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Agents);
    assert_eq!(app.agent_panel.selected, 1);

    app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.active_pane, PaneFocus::Activity);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "/");

    app.active_pane = PaneFocus::Activity;
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.active_pane, PaneFocus::Composer);
    Ok(())
}

#[test]
fn busy_escape_returns_from_history_without_cancelling_the_run() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    app.active_pane = PaneFocus::Activity;
    app.composer.input = "keep draft".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "keep draft");
    assert_ne!(app.last_notice(), Some("cancellation requested"));
    assert!(
        app.timeline.iter().all(|entry| {
            entry.role != TimelineRole::Notice || entry.text != "cancel requested"
        })
    );
    Ok(())
}

#[test]
fn slash_command_busy_and_unknown_paths_leave_tui_responsive() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.composer.input = "/unknown".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.last_notice(), Some("unknown slash command"));

    app.runtime.is_busy = true;
    app.composer.input = "/compact".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "busy; compact later")
    );

    app.composer.input = "/resume missing".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "busy; resume later")
    );
    Ok(())
}

#[test]
fn app_status_helpers_cover_empty_balance_context_and_session_title() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.balance_snapshot.available = true;
    app.runtime.balance_snapshot.total = None;
    app.runtime.balance_snapshot.currency = None;
    app.runtime.balance_snapshot.status = "checking".to_owned();
    app.runtime.stats.last_prompt_tokens = 1234;

    assert_eq!(app.balance_sidebar_line(), "balance: checking");
    assert_eq!(
        app.context_usage_line(),
        "ctx: 0% · cache=0% · prompt 1.2K / 1.0M provider · soft at 700.0K"
    );
    let policy = app.compaction_policy_line();
    assert!(policy.starts_with("policy: "));
    assert!(policy.contains("provider"));
    assert!(policy.contains("soft"));
    assert!(policy.contains("hard"));
    assert_eq!(app.permission_card_lines()[2], "scope: saved default");
    assert!(app.session_display_title().contains("deepseek-v4-flash"));

    app.push_timeline(TimelineRole::User, "\n\nfirst line\nsecond line");
    assert_eq!(
        app.latest_user_prompt_preview(),
        Some("first line  +1 more".to_owned())
    );
    assert_eq!(app.session_display_title(), "first line");

    app.runtime.is_busy = true;
    assert_eq!(app.permission_card_lines()[2], "busy: locked during run");
    assert!(app.footer_status_line().contains("Ctrl-C cancel"));
}

#[test]
fn live_activity_summary_tracks_busy_phase() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert!(app.live_activity_summary().is_none());

    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("read_file".to_owned());

    let summary = app.live_activity_summary().expect("expected live summary");
    assert_eq!(summary.label, "tool");
    assert_eq!(summary.detail, "running read_file");
}

#[test]
fn child_agent_view_live_activity_overrides_parent_busy_phase() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("wait_agent".to_owned());

    let summary = app.live_activity_summary().expect("child live summary");

    assert_eq!(summary.label, "agent");
    assert!(summary.detail.contains("repo read"));
    assert!(summary.detail.contains("started"));
    assert!(!summary.detail.contains("wait_agent"));
    assert!(matches!(app.live_panel_phase(), RunPhase::Agent(_)));
    Ok(())
}

#[test]
fn terminal_child_agent_view_does_not_render_working_progress() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_terminal")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("profile_snapshot_terminal")?;
    let session_ref = sigil_kernel::SessionRef::new_relative("children/agent_chat_terminal.jsonl")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::System,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: session_ref.clone(),
                profile_id,
                profile_snapshot_id: snapshot_id.clone(),
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: snapshot_id.clone(),
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    model_ref: None,
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(
                        "/tmp/workspace".to_owned(),
                    )?,
                    effective_tool_scope_hash: "sha256:tools".to_owned(),
                    effective_permission_policy_hash: "sha256:permissions".to_owned(),
                    effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                    provider_capability_hash: "sha256:provider".to_owned(),
                    model_visible_agent_index_hash: Some("sha256:index".to_owned()),
                    budget_policy_hash: "sha256:budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Foreground,
                invocation_source: sigil_kernel::AgentInvocationSource::Chat,
                display_name: Some("kernel explorer".to_owned()),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(
            sigil_kernel::AgentThreadResultRecordedEntry {
                result: sigil_kernel::AgentThreadResult {
                    thread_id: thread_id.clone(),
                    session_ref,
                    status: sigil_kernel::AgentThreadTerminalStatus::Completed,
                    summary: "done".to_owned(),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: "sha256:done".to_owned(),
                    final_answer_ref: None,
                },
            },
        )),
    ]);
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: "agent_chat_terminal".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/agent_chat_terminal.jsonl",
        )?,
    };
    app.runtime.is_busy = false;

    assert!(app.live_activity_summary().is_none());

    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Streaming;
    let summary = app
        .live_activity_summary()
        .expect("parent activity should still be visible");
    assert_eq!(summary.label, "streaming");
    assert_eq!(summary.detail, "receiving response");
    Ok(())
}

#[test]
fn terminal_task_child_view_does_not_render_working_progress() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let mut entries = app.session_browser.current_entries.clone();
    entries.push(SessionLogEntry::Control(ControlEntry::TaskChildSession(
        sigil_kernel::TaskChildSessionEntry {
            task_id: sigil_kernel::TaskId::new("task_1")?,
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("step_1")?,
            child_task_id: sigil_kernel::TaskId::new("child_1")?,
            child_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/task_1/step_1-child_1.jsonl",
            )?,
            role: sigil_kernel::AgentRole::SubagentRead,
            status: sigil_kernel::TaskChildSessionStatus::Completed,
            summary_hash: None,
        },
    )));
    entries.push(SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
        sigil_kernel::AgentThreadClosedEntry {
            thread_id: sigil_kernel::AgentThreadId::new("child_1")?,
            reason: Some("hidden from sidebar".to_owned()),
        },
    )));
    app.sync_current_session_state(entries);
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/task_1/step_1-child_1.jsonl",
        )?,
    };

    assert!(app.live_activity_summary().is_none());
    Ok(())
}

#[test]
fn child_agent_view_live_activity_falls_back_to_task_child_entry() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let mut entries = app.session_browser.current_entries.clone();
    entries.push(SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
        sigil_kernel::AgentThreadClosedEntry {
            thread_id: sigil_kernel::AgentThreadId::new("child_1")?,
            reason: Some("hidden from sidebar".to_owned()),
        },
    )));
    app.sync_current_session_state(entries);
    app.agent_panel.active_view = super::super::AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/task_1/step_1-child_1.jsonl",
        )?,
    };
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("wait_agent".to_owned());

    let summary = app
        .live_activity_summary()
        .expect("task child summary should survive closed thread filtering");

    assert!(matches!(app.live_panel_phase(), RunPhase::Agent(profile) if profile == "agent"));
    assert_eq!(summary.label, "agent");
    assert!(summary.detail.contains("child_1"));
    assert!(summary.detail.contains("started"));
    assert!(summary.detail.contains("subagent_read"));
    assert!(!summary.detail.contains("wait_agent"));
    Ok(())
}

#[test]
fn duplicate_phase_markers_do_not_append_duplicate_events() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.push_phase_marker("thinking|deepseek-v4-flash");
    app.push_phase_marker("thinking|deepseek-v4-flash");
    app.push_phase_marker("tool|bash");

    let phase_events = app
        .events
        .iter()
        .filter(|event| event.label == "phase")
        .map(|event| event.detail.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        phase_events,
        vec!["thinking|deepseek-v4-flash", "tool|bash"]
    );
}

#[test]
fn transcript_lines_on_empty_timeline_still_renders_placeholder_lines() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let lines = app.transcript_lines(3);
    assert!(!lines.is_empty());
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| !span.content.as_ref().trim().is_empty())
    }));
}

#[test]
fn push_assistant_message_once_deduplicates_and_ignores_empty() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    // Empty content should not push.
    app.push_assistant_message_once(String::new());
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Assistant)
    );

    // First non-empty push creates an entry.
    app.push_assistant_message_once("hello".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );

    // Duplicate content since last user message should be suppressed.
    app.push_assistant_message_once("hello".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );

    // Different content pushes a new entry.
    app.push_assistant_message_once("world".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        2
    );

    // After a user message interjection, duplicate of previous assistant is allowed.
    app.push_timeline(TimelineRole::User, "interjection".to_owned());
    app.push_assistant_message_once("hello".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        3
    );

    Ok(())
}

#[test]
fn usage_sidebar_explains_cache_write_and_proven_local_layout_change() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 10,
        cache_hit_tokens: 40,
        cache_miss_tokens: 60,
        cache_usage: Some(sigil_kernel::CacheUsageV1 {
            schema_version: sigil_kernel::CacheUsageV1::SCHEMA_VERSION,
            read: Some(sigil_kernel::CacheTokenCountV1::provider_reported(40)),
            write: Some(sigil_kernel::CacheTokenCountV1::provider_reported(20)),
            uncached: Some(sigil_kernel::CacheTokenCountV1::provider_reported(40)),
            local_layout_mutation: Some(sigil_kernel::CacheLayoutMutationKind::ToolSchemaChanged),
            provider_miss_without_local_mutation: false,
        }),
        ..UsageStats::default()
    }))?;

    assert!(app.usage_sidebar_lines().iter().any(|line| {
        line == "cache io: read 40 · write 20 · miss 60 · layout tool_schema_changed"
    }));
    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        cache_miss_tokens: 100,
        cache_usage: Some(sigil_kernel::CacheUsageV1 {
            schema_version: sigil_kernel::CacheUsageV1::SCHEMA_VERSION,
            read: Some(sigil_kernel::CacheTokenCountV1::provider_reported(0)),
            write: None,
            uncached: Some(sigil_kernel::CacheTokenCountV1::provider_reported(100)),
            local_layout_mutation: Some(sigil_kernel::CacheLayoutMutationKind::Identical),
            provider_miss_without_local_mutation: true,
        }),
        ..UsageStats::default()
    }))?;
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| { line == "cache miss source: provider miss without local mutation" })
    );
    Ok(())
}
