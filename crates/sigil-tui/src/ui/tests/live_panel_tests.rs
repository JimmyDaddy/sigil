use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use crate::{
    app::AppState,
    timeline::ComposerQueueRow,
    ui::StatusKind,
    view_model::{
        LivePanelViewModel, LiveProgressViewModel, PlanApprovalViewModel,
        QueueActionButtonViewModel, TaskStripRowViewModel, TaskStripViewModel,
        VerificationCardViewModel,
    },
};

use super::*;
use crate::ui::theme::{accent_blue, phase_accent};

fn rendered_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect()
}

fn test_config() -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: Some(
                sigil_kernel::ConnectionId::new("deepseek-default").expect("valid test connection"),
            ),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(true),
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        connections: BTreeMap::from([(
            "deepseek-default".to_owned(),
            serde_json::json!({
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {"source": "environment", "name": "SIGIL_API_KEY"}
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn render_live_progress_lines_shows_current_phase() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.set_terminal_size(120, 30);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;

    let view_model = LivePanelViewModel::from_app(&app, 4);
    let lines = render_live_progress_lines(
        view_model
            .progress
            .as_ref()
            .expect("busy run should expose live progress"),
        phase_accent(&view_model.phase),
        80,
    );
    let plain = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(plain.contains("Thinking..."));
    assert!(!plain.contains("(Thinking)"));
    assert!(plain.contains("reasoning with"));
    Ok(())
}

#[test]
fn status_band_line_pads_tail_with_band_background() {
    let theme = Theme::default();
    let bg = theme.palette.surface_panel_alt;
    let line = status_band_line(Line::from(vec![Span::raw("Queue")]), 12, bg);

    assert_eq!(line_display_width(&line.spans), 12);
    assert_eq!(line.style.bg, Some(bg));
    assert_eq!(line.spans[0].style.bg, Some(bg));
    let tail = line.spans.last().expect("expected padded tail span");
    assert_eq!(tail.content.as_ref(), "       ");
    assert_eq!(tail.style.bg, Some(bg));
}

#[test]
fn status_band_line_truncates_to_exact_display_width() {
    let theme = Theme::default();
    let bg = theme.palette.surface_panel_alt;
    let line = status_band_line(
        Line::from(vec![Span::raw("0123456789"), Span::raw("overflow")]),
        8,
        bg,
    );

    assert_eq!(line_display_width(&line.spans), 8);
    let plain = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(plain, "01234...");
}

#[test]
fn status_band_line_matches_ratatui_width_and_filters_controls() {
    let bg = Theme::default().palette.surface_panel_alt;
    let line = status_band_line(Line::from(Span::raw("ｶﾞ\t")), 2, bg);

    assert_eq!(line_display_width(&line.spans), 2);
    assert_eq!(
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "ｶﾞ"
    );
}

#[test]
fn status_band_allocation_matches_renderer_priority_under_capacity_pressure() {
    let allocation = allocate_status_band_rows(
        LiveStatusRowAllocation {
            queue: 3,
            progress: 2,
            plan: 12,
            task: 4,
            verification: 4,
        },
        6,
        None,
    );

    assert_eq!(allocation.queue, 1);
    assert_eq!(allocation.progress, 1);
    assert_eq!(allocation.plan, 2);
    assert_eq!(allocation.task, 1);
    assert_eq!(allocation.verification, 1);
}

#[test]
fn status_band_allocation_keeps_the_focused_verification_action_visible() {
    let allocation = allocate_status_band_rows(
        LiveStatusRowAllocation {
            queue: 3,
            verification: 4,
            ..LiveStatusRowAllocation::default()
        },
        1,
        Some(StatusBandSectionKind::Verification),
    );

    assert_eq!(allocation.verification, 1);
    assert_eq!(allocation.queue, 0);
}

#[test]
fn queue_action_layout_always_keeps_the_selected_button_complete() {
    let labels = ["Run next", "Interrupt", "Edit", "Delete"];
    for width in 16..=20 {
        for selected_index in 0..labels.len() {
            let layout = queue_action_button_layout(&labels, selected_index, width);
            let selected = layout
                .iter()
                .find(|placement| placement.button_index == selected_index)
                .expect("selected queue action must remain visible");
            assert_eq!(
                selected.width,
                terminal_cell_width(labels[selected_index]).saturating_add(2)
            );
            assert!(selected.x.saturating_add(selected.width) <= width);
        }
    }
}

#[test]
fn queue_item_window_keeps_selected_item_next_to_the_pinned_action() {
    let two_rows = queue_item_window(4, 3, 2);
    assert!(!two_rows.show_header);
    assert_eq!(two_rows.item_indices, vec![3]);

    let three_rows = queue_item_window(4, 3, 3);
    assert!(three_rows.show_header);
    assert_eq!(three_rows.item_indices, vec![3]);

    let four_rows = queue_item_window(4, 3, 4);
    assert!(four_rows.show_header);
    assert_eq!(four_rows.item_indices, vec![2, 3]);

    let full = queue_item_window(4, 3, 6);
    assert!(full.show_header);
    assert_eq!(full.item_indices, vec![0, 1, 2, 3]);
}

#[test]
fn queue_action_ultranarrow_selected_delete_keeps_semantic_and_style_coordinates()
-> anyhow::Result<()> {
    let buttons = vec![
        QueueActionButtonViewModel {
            label: "Run next".to_owned(),
            detail: "run after the current turn".to_owned(),
            selected: false,
            destructive: false,
        },
        QueueActionButtonViewModel {
            label: "Interrupt".to_owned(),
            detail: "stop current turn and run this follow-up".to_owned(),
            selected: false,
            destructive: false,
        },
        QueueActionButtonViewModel {
            label: "Edit".to_owned(),
            detail: "edit follow-up".to_owned(),
            selected: false,
            destructive: false,
        },
        QueueActionButtonViewModel {
            label: "Delete".to_owned(),
            detail: "remove follow-up".to_owned(),
            selected: true,
            destructive: true,
        },
    ];
    let labels = buttons
        .iter()
        .map(|button| button.label.as_str())
        .collect::<Vec<_>>();
    let placement = queue_action_button_layout(&labels, 3, 16)
        .into_iter()
        .find(|placement| placement.button_index == 3)
        .expect("selected Delete action should be present");
    assert_eq!(placement.x, 0);
    assert_eq!(placement.width, terminal_cell_width(" Delete "));
    assert_eq!(buttons[placement.button_index].label, "Delete");

    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: vec![ComposerQueueRow {
            label: "queued prompt".to_owned(),
            detail: "queued · chat".to_owned(),
            status: StatusKind::Pending,
            selected: true,
        }],
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: buttons,
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    // The live status content is 16 cells wide after the panel's two-cell status inset.
    let backend = TestBackend::new(20, 7);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rows = rendered_rows(&terminal);
    let action_y = rows
        .iter()
        .position(|row| row.contains("Delete"))
        .expect("selected Delete button should render in full");
    let buffer = terminal.backend().buffer();
    let selected_style = Theme::default().palette.selection_bg;
    for x in 3..11 {
        assert_eq!(
            buffer[(x, action_y as u16)].style().bg,
            Some(selected_style),
            "renderer style must begin at status content x plus placement x"
        );
    }

    let tight = render_queue_actions(
        &view_model.queue_action_buttons,
        None,
        None,
        6,
        true,
        1,
        &Theme::default(),
    );
    assert_eq!(line_display_width(&tight.spans), 6);
    assert_eq!(
        tight
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "Delete"
    );
    assert_eq!(tight.spans[0].style.bg, Some(selected_style));

    let compact = render_queue_actions(
        &view_model.queue_action_buttons,
        Some("fourth queued prompt"),
        Some(3),
        16,
        true,
        1,
        &Theme::default(),
    );
    let compact_text = compact
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(compact_text.contains("#4 Delete"));
    let compact_labels = view_model
        .queue_action_buttons
        .iter()
        .map(|button| button.label.as_str())
        .collect::<Vec<_>>();
    let compact_layout = queue_action_button_layout_for_budget(&compact_labels, 3, Some(3), 16, 1);
    assert_eq!(compact_layout[0].button_index, 3);
    assert_eq!(compact_layout[0].x, 0);
    assert_eq!(compact_layout[0].width, terminal_cell_width(" #4 Delete "));
    Ok(())
}

#[test]
fn verification_fitting_pins_the_action_when_only_one_row_is_available() {
    let theme = Theme::default();
    let lines = render_verification_card_lines(
        &VerificationCardViewModel {
            title: "Verification".to_owned(),
            status: "check failed".to_owned(),
            recommended: Some("cargo test".to_owned()),
            why: Some("latest check failed".to_owned()),
            action_label: Some("run check"),
            inspect_lines: vec!["details".to_owned()],
            focused: true,
            inspect_open: true,
        },
        40,
        &theme,
    );

    let fitted = StatusBandSection {
        kind: StatusBandSectionKind::Verification,
        lines,
        selected_queue_item: None,
        compact_action_line: Some(render_compact_verification_action(
            &VerificationCardViewModel {
                title: "Verification".to_owned(),
                status: "check failed".to_owned(),
                recommended: Some("cargo test".to_owned()),
                why: Some("latest check failed".to_owned()),
                action_label: Some("run check"),
                inspect_lines: vec!["details".to_owned()],
                focused: true,
                inspect_open: true,
            },
            40,
            &theme,
        )),
    }
    .into_fitted_lines(1, &theme);
    let rendered = fitted[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.contains("Enter run"));
    assert!(rendered.contains("cargo test"));
}

#[test]
fn verification_one_and_two_row_budgets_keep_exact_recommended_target() {
    let theme = Theme::default();
    let card = VerificationCardViewModel {
        title: "Verification".to_owned(),
        status: "check failed".to_owned(),
        recommended: Some("docs-check".to_owned()),
        why: Some("latest check failed".to_owned()),
        action_label: Some("run check"),
        inspect_lines: Vec::new(),
        focused: true,
        inspect_open: false,
    };
    for budget in 1..=2 {
        let fitted = StatusBandSection {
            kind: StatusBandSectionKind::Verification,
            lines: render_verification_card_lines(&card, 36, &theme),
            selected_queue_item: None,
            compact_action_line: Some(render_compact_verification_action(&card, 36, &theme)),
        }
        .into_fitted_lines(budget, &theme);
        let rendered = fitted
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Enter run"));
        assert!(rendered.contains("docs-check"));
    }
}

#[test]
fn narrow_compact_verification_marks_a_target_that_cannot_fit() {
    let theme = Theme::default();
    let line = render_compact_verification_action(
        &VerificationCardViewModel {
            title: "Verification".to_owned(),
            status: "check failed".to_owned(),
            recommended: Some("workspace-integration-check-with-long-name".to_owned()),
            why: None,
            action_label: Some("run check"),
            inspect_lines: Vec::new(),
            focused: true,
            inspect_open: false,
        },
        22,
        &theme,
    );
    let rendered = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.contains("Enter run"));
    assert!(rendered.contains("tgt hidden"));
    assert_eq!(terminal_cell_width(&rendered), 22);
}

#[test]
fn phase_accent_uses_blue_for_agent_phase() {
    assert_eq!(
        phase_accent(&crate::timeline::RunPhase::Agent("explore".to_owned())),
        accent_blue()
    );
}

#[test]
fn render_live_panel_keeps_wrapped_tail_visible() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from(
            "prefix words that wrap across rows before visible TAIL",
        )],
    };
    let backend = TestBackend::new(16, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("TAIL"));
    Ok(())
}

#[test]
fn render_live_panel_keeps_bottom_padding_clear() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(16, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let buffer = terminal.backend().buffer();
    let bottom_row = buffer.content()[48..64]
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(bottom_row.trim().is_empty());
    Ok(())
}

#[test]
fn render_live_panel_merges_task_strip_into_status_band() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "reasoning with deepseek-v4-pro".to_owned(),
        }),
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            task_id: "task_1".to_owned(),
            verification: None,
            title: "Improve task status display".to_owned(),
            detail: "running · v1 · 1/2 done".to_owned(),
            route_diagnostics: vec![
                "subagent-read×2 → deepseek/deepseek-v4-pro · 2 model requests running · concurrency limit 4".to_owned(),
            ],
            completion_progress: vec![
                "read batch v1 · 1/2 arrived · commits follow request order".to_owned(),
                "arrival #1 → commit #2 · inspect layout · ok".to_owned(),
            ],
            rows: vec![
                TaskStripRowViewModel {
                    kind: crate::ui::StatusKind::Success,
                    label: "1. inspect layout".to_owned(),
                    active: false,
                },
                TaskStripRowViewModel {
                    kind: crate::ui::StatusKind::Pending,
                    label: "2. update status band".to_owned(),
                    active: true,
                },
            ],
            expanded: false,
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(104, 11);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("visible tail"));
    assert!(rendered.contains("Thinking..."));
    assert!(rendered.contains("Improve task status display"));
    assert!(rendered.contains("running · v1 · 1/2 done"));
    assert!(rendered.contains("Route subagent-read×2"));
    assert!(rendered.contains("2 model requests running"));
    assert!(rendered.contains("Batch read batch v1"));
    assert!(rendered.contains("arrival #1"));
    assert!(rendered.contains("commit #2"));
    assert!(rendered.contains("✓ 1. inspect layout"));
    assert!(rendered.contains("◇ 2. update status band"));
    assert!(rendered.contains("▌"));
    assert!(!rendered.contains("status:"));
    Ok(())
}

#[test]
fn render_live_panel_keeps_progress_and_task_rows_single_line_on_narrow_width() -> anyhow::Result<()>
{
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: Some(LiveProgressViewModel {
            title: "Thinking through a very long operation name".to_owned(),
            detail: "progress detail that must remain on exactly one physical row".to_owned(),
        }),
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            task_id: "task_1".to_owned(),
            verification: None,
            title: "Task task_1".to_owned(),
            detail: "running with a deliberately long status description".to_owned(),
            route_diagnostics: vec![
                "subagent-read → provider/model with additional route diagnostics".to_owned(),
            ],
            completion_progress: vec![
                "read batch v1 with a deliberately long completion summary".to_owned(),
            ],
            rows: vec![TaskStripRowViewModel {
                kind: StatusKind::Running,
                label: "1. implement a deliberately long task label".to_owned(),
                active: true,
            }],
            expanded: false,
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(38, 10);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rows = rendered_rows(&terminal);
    let thinking = rows
        .iter()
        .position(|row| row.contains("Thinking"))
        .expect("progress title should render");
    let progress_detail = rows
        .iter()
        .position(|row| row.contains("progress detail"))
        .expect("progress detail should render");
    let task = rows
        .iter()
        .position(|row| row.contains("Task task_1"))
        .expect("task header should render");
    let route = rows
        .iter()
        .position(|row| row.contains("Route subagent-read"))
        .expect("route row should render");
    let batch = rows
        .iter()
        .position(|row| row.contains("Batch read batch"))
        .expect("batch row should render");
    let task_row = rows
        .iter()
        .position(|row| row.contains("1. implement"))
        .expect("task row should render");
    assert_eq!(progress_detail, thinking + 1);
    assert_eq!(task, progress_detail + 1);
    assert_eq!(route, task + 1);
    assert_eq!(batch, route + 1);
    assert_eq!(task_row, batch + 1);
    Ok(())
}

#[test]
fn render_live_panel_shows_focused_verification_card_and_evidence() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            task_id: "task_1".to_owned(),
            title: "Task task_1".to_owned(),
            detail: "paused · check failed".to_owned(),
            route_diagnostics: Vec::new(),
            completion_progress: Vec::new(),
            verification: Some(VerificationCardViewModel {
                title: "Verification".to_owned(),
                status: "check failed".to_owned(),
                recommended: Some("cargo-test".to_owned()),
                why: Some("the latest result failed".to_owned()),
                action_label: Some("run check"),
                inspect_lines: vec![
                    "Snapshot: snapshot-1".to_owned(),
                    "Changeset: not linked".to_owned(),
                ],
                focused: true,
                inspect_open: true,
            }),
            rows: vec![TaskStripRowViewModel {
                kind: crate::ui::StatusKind::Error,
                label: "1. check failed · implement".to_owned(),
                active: true,
            }],
            expanded: false,
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Verification  ·  check failed"));
    assert!(rendered.contains("Recommended  cargo-test"));
    assert!(rendered.contains("Enter run check  ·  I inspect"));
    assert!(rendered.contains("Snapshot: snapshot-1"));
    assert!(rendered.contains("Changeset: not linked"));
    Ok(())
}

#[test]
fn render_live_panel_shows_queue_strip_actions_above_status() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: vec![ComposerQueueRow {
            label: "queued prompt".to_owned(),
            detail: "queued · chat".to_owned(),
            status: StatusKind::Pending,
            selected: true,
        }],
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: vec![
            QueueActionButtonViewModel {
                label: "Run next".to_owned(),
                detail: "run after the current turn".to_owned(),
                selected: true,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Interrupt".to_owned(),
                detail: "stop current turn and run this follow-up".to_owned(),
                selected: false,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Edit".to_owned(),
                detail: "edit follow-up".to_owned(),
                selected: false,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Delete".to_owned(),
                detail: "remove follow-up".to_owned(),
                selected: false,
                destructive: true,
            },
        ],
        progress: Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "reasoning with deepseek-v4-pro".to_owned(),
        }),
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(104, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Follow-ups"));
    assert!(rendered.contains("queued prompt"));
    assert!(rendered.contains("Run next"));
    assert!(rendered.contains("Interrupt"));
    assert!(rendered.contains("Edit"));
    assert!(rendered.contains("Delete"));
    assert!(rendered.contains("Thinking..."));
    assert!(!rendered.contains("S now"));
    assert!(!rendered.contains("D delete"));
    assert!(!rendered.contains("E edit"));
    Ok(())
}

#[test]
fn render_live_panel_queue_strip_covers_paused_and_unfocused_rows() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: vec![
            ComposerQueueRow {
                label: "first queued prompt".to_owned(),
                detail: "paused · chat".to_owned(),
                status: StatusKind::Pending,
                selected: false,
            },
            ComposerQueueRow {
                label: "second queued prompt".to_owned(),
                detail: "queued · chat".to_owned(),
                status: StatusKind::Running,
                selected: true,
            },
        ],
        queue_paused: true,
        queue_panel_focused: false,
        queue_action_buttons: vec![QueueActionButtonViewModel {
            label: "Interrupt".to_owned(),
            detail: "stop current turn and run this follow-up".to_owned(),
            selected: false,
            destructive: false,
        }],
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(96, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Follow-ups paused"));
    assert!(rendered.contains("/queue advanced"));
    assert!(rendered.contains("first queued prompt"));
    assert!(rendered.contains("second queued prompt"));
    assert!(rendered.contains("Interrupt"));
    Ok(())
}

#[test]
fn render_live_panel_keeps_focused_queue_rows_single_line_on_narrow_width() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: vec![ComposerQueueRow {
            label: "queued prompt with a label that is much wider than the status area".to_owned(),
            detail: "queued · chat · additional detail that must not wrap".to_owned(),
            status: StatusKind::Pending,
            selected: true,
        }],
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: vec![
            QueueActionButtonViewModel {
                label: "Run next".to_owned(),
                detail: "run after the current turn".to_owned(),
                selected: true,
                destructive: false,
            },
            QueueActionButtonViewModel {
                label: "Delete".to_owned(),
                detail: "remove follow-up".to_owned(),
                selected: false,
                destructive: true,
            },
        ],
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(34, 7);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rows = rendered_rows(&terminal);
    let header = rows
        .iter()
        .position(|row| row.contains("Follow-ups"))
        .expect("queue header should render");
    let actions = rows
        .iter()
        .position(|row| row.contains("Actions"))
        .expect("queue actions should render");
    assert_eq!(
        actions,
        header + 2,
        "selected row must occupy exactly one row"
    );
    assert!(rows[header + 1].contains("queued prompt"));
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("queued prompt"))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn render_live_panel_short_queue_keeps_selected_last_item_and_action_visible() -> anyhow::Result<()>
{
    let queue_rows = (0..4)
        .map(|index| ComposerQueueRow {
            label: format!("queued prompt {}", index + 1),
            detail: "queued · chat".to_owned(),
            status: StatusKind::Pending,
            selected: index == 3,
        })
        .collect();
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows,
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: vec![QueueActionButtonViewModel {
            label: "Delete".to_owned(),
            detail: "remove follow-up".to_owned(),
            selected: true,
            destructive: true,
        }],
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(40, 5);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rows = rendered_rows(&terminal);
    assert!(rows.iter().any(|row| row.contains("queued prompt 4")));
    assert!(!rows.iter().any(|row| row.contains("queued prompt 1")));
    assert!(rows.iter().any(|row| row.contains("Delete")));
    Ok(())
}

#[test]
fn render_live_panel_one_row_queue_budget_labels_the_destructive_target() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: (0..4)
            .map(|index| ComposerQueueRow {
                label: format!("queued prompt {}", index + 1),
                detail: "queued · chat".to_owned(),
                status: StatusKind::Pending,
                selected: index == 3,
            })
            .collect(),
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: vec![QueueActionButtonViewModel {
            label: "Delete".to_owned(),
            detail: "remove follow-up".to_owned(),
            selected: true,
            destructive: true,
        }],
        progress: None,
        plan_approval: None,
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(40, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = rendered_rows(&terminal).join("\n");
    assert!(rendered.contains("Delete"));
    assert!(rendered.contains("#4 queued"));
    Ok(())
}

#[test]
fn render_live_panel_shows_plan_approval_surface() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: Some(PlanApprovalViewModel {
            summary: "inspect and edit with preview".to_owned(),
            steps: vec!["inspect and edit with preview".to_owned()],
            target_path_count: 2,
            suggested_check_count: 1,
            stale: false,
            stale_reason: None,
        }),
        task_strip: None,
        transcript_lines: vec![Line::from("plan body")],
    };
    let backend = TestBackend::new(96, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Plan"));
    assert!(rendered.contains("ready"));
    assert!(rendered.contains("1 step"));
    assert!(rendered.contains("2 paths"));
    assert!(rendered.contains("1 check"));
    assert!(rendered.contains("inspect and edit"));
    assert!(rendered.contains("Enter"));
    assert!(rendered.contains("review"));
    assert!(!rendered.contains("scoped edits"));
    assert!(!rendered.contains("Shift-Enter"));
    assert!(!rendered.contains("Esc reject"));
    Ok(())
}

#[test]
fn render_live_panel_one_row_plan_budget_discloses_hidden_plan_before_run() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: Some(PlanApprovalViewModel {
            summary: "hidden on a one-row status budget".to_owned(),
            steps: vec!["inspect".to_owned(), "edit".to_owned()],
            target_path_count: 1,
            suggested_check_count: 1,
            stale: false,
            stale_reason: None,
        }),
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(40, 4);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = rendered_rows(&terminal).join("\n");
    assert!(rendered.contains("Enter review"));
    assert!(rendered.contains("plan…"));
    Ok(())
}

#[test]
fn plan_one_row_compact_action_keeps_review_visible_at_16_columns() {
    let theme = Theme::default();
    let action = render_plan_action_line(16, &theme);
    let compact = status_band_line(
        render_hidden_plan_action_line(action, &theme),
        16,
        theme.palette.surface_panel_alt,
    );
    let rendered = compact
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(rendered.contains("Enter"));
    assert!(!rendered.trim().is_empty());
}

#[test]
fn render_live_panel_bounds_long_plan_and_keeps_actions_on_short_narrow_terminal()
-> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Idle,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: Some(PlanApprovalViewModel {
            summary: "inspect a large change and keep the approval controls visible".to_owned(),
            steps: (0..8)
                .map(|index| format!("step {index} with a deliberately long description"))
                .collect(),
            target_path_count: 2,
            suggested_check_count: 1,
            stale: false,
            stale_reason: None,
        }),
        task_strip: None,
        transcript_lines: vec![Line::from("visible tail")],
    };
    assert_eq!(
        live_status_rows(&view_model, 34),
        7,
        "large plans stay a compact overview, including the separator"
    );
    let backend = TestBackend::new(36, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rows = rendered_rows(&terminal);
    assert!(rows.iter().any(|row| row.contains("Plan ready")));
    assert!(rows.iter().any(|row| row.contains("Enter")));
    assert!(rows.last().is_some_and(|row| row.trim().is_empty()));
    Ok(())
}

#[test]
fn render_live_panel_reserves_stacked_surface_action_rows_before_optional_details()
-> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: vec![ComposerQueueRow {
            label: "queued follow-up".to_owned(),
            detail: "queued · chat".to_owned(),
            status: StatusKind::Pending,
            selected: true,
        }],
        queue_paused: false,
        queue_panel_focused: true,
        queue_action_buttons: vec![QueueActionButtonViewModel {
            label: "Run next".to_owned(),
            detail: "run after the current turn".to_owned(),
            selected: true,
            destructive: false,
        }],
        progress: Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "a progress detail that is intentionally too wide for this terminal".to_owned(),
        }),
        plan_approval: Some(PlanApprovalViewModel {
            summary: "stacked plan".to_owned(),
            steps: vec!["inspect".to_owned(), "edit".to_owned(), "verify".to_owned()],
            target_path_count: 1,
            suggested_check_count: 1,
            stale: false,
            stale_reason: None,
        }),
        task_strip: Some(TaskStripViewModel {
            task_id: "task_1".to_owned(),
            verification: None,
            title: "Task task_1".to_owned(),
            detail: "running".to_owned(),
            route_diagnostics: vec!["route detail".to_owned()],
            completion_progress: Vec::new(),
            rows: vec![TaskStripRowViewModel {
                kind: StatusKind::Running,
                label: "1. implement".to_owned(),
                active: true,
            }],
            expanded: false,
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(50, 9);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = rendered_rows(&terminal).join("\n");
    assert!(rendered.contains("queued follow-up"));
    assert!(rendered.contains("Actions"));
    assert!(rendered.contains("Thinking..."));
    assert!(rendered.contains("Enter review"));
    assert!(rendered.contains("Task task_1"));
    Ok(())
}

#[test]
fn render_live_panel_keeps_long_task_label_expanded() -> anyhow::Result<()> {
    let view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            task_id: "task_3".to_owned(),
            verification: None,
            title: "Task task_3".to_owned(),
            detail: "started".to_owned(),
            route_diagnostics: Vec::new(),
            completion_progress: Vec::new(),
            rows: vec![TaskStripRowViewModel {
                kind: crate::ui::StatusKind::Running,
                label: "1. 输出一个冷笑话2、解释一下这个冷笑话为什么好笑".to_owned(),
                active: true,
            }],
            expanded: false,
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };
    let backend = TestBackend::new(96, 6);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    let compact = rendered.replace(' ', "");
    assert!(compact.contains("1.输出一个冷笑话2、解释一下这个冷笑话为什么好笑"));
    Ok(())
}

#[test]
fn render_live_panel_task_strip_expands_all_rows_and_keeps_active_row_visible_when_collapsed()
-> anyhow::Result<()> {
    let rows = (1..=12)
        .map(|index| TaskStripRowViewModel {
            kind: if index < 8 {
                StatusKind::Success
            } else if index == 8 {
                StatusKind::Running
            } else {
                StatusKind::Pending
            },
            label: format!("{index}. task item {index}"),
            active: index == 8,
        })
        .collect::<Vec<_>>();
    let mut view_model = LivePanelViewModel {
        phase: crate::timeline::RunPhase::Thinking,
        queue_rows: Vec::new(),
        queue_paused: false,
        queue_panel_focused: false,
        queue_action_buttons: Vec::new(),
        progress: None,
        plan_approval: None,
        task_strip: Some(TaskStripViewModel {
            task_id: "task_12".to_owned(),
            verification: None,
            title: "Twelve task items".to_owned(),
            detail: "running · 7/12 done".to_owned(),
            route_diagnostics: Vec::new(),
            completion_progress: Vec::new(),
            rows,
            expanded: false,
        }),
        transcript_lines: vec![Line::from("visible tail")],
    };

    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;
    let collapsed = rendered_rows(&terminal).join("\n");
    assert!(
        collapsed.contains("6. task item 6"),
        "rendered: {collapsed:?}"
    );
    assert!(
        collapsed.contains("8. task item 8"),
        "rendered: {collapsed:?}"
    );
    assert!(
        collapsed.contains("9. task item 9"),
        "rendered: {collapsed:?}"
    );
    assert!(!collapsed.contains("5. task item 5"));
    assert!(collapsed.contains("+8 more tasks · click/Ctrl-T expand"));

    view_model.task_strip.as_mut().expect("task strip").expanded = true;
    let backend = TestBackend::new(100, 20);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;
    let expanded = rendered_rows(&terminal).join("\n");
    assert!(
        expanded.contains("1. task item 1"),
        "rendered: {expanded:?}"
    );
    assert!(
        expanded.contains("12. task item 12"),
        "rendered: {expanded:?}"
    );
    assert!(expanded.contains("Show less · click/Ctrl-T collapse"));
    Ok(())
}
