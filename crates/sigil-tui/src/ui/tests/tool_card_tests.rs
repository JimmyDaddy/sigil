use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::{Value, json};

use crate::{
    app::{TimelineEntry, TimelineRole},
    ui::{
        TimelineRenderOptions,
        theme::{Theme, ThemePalette, accent_gold, accent_rose, accent_teal, dim},
    },
};

use super::*;

fn test_palette() -> ThemePalette {
    crate::ui::theme::default_palette()
}

fn code_intelligence_row(summary: &ToolCardRender, entry: &Value) -> Option<Vec<Span<'static>>> {
    super::code_intelligence_row_with_palette(summary, entry, &test_palette())
}

fn code_intelligence_servers_line(value: &Value) -> Option<Vec<Span<'static>>> {
    super::code_intelligence_servers_line_with_palette(value, &test_palette())
}

fn render_tool_diff_line(
    accent: Color,
    line: NumberedDiffLine<'_>,
    line_number_width: usize,
) -> Line<'static> {
    super::render_tool_diff_line_with_palette(accent, line, line_number_width, &test_palette())
}

fn tool_diff_old_line_number_style(line: NumberedDiffLine<'_>) -> Style {
    super::tool_diff_old_line_number_style_with_palette(line, &test_palette())
}

fn tool_diff_new_line_number_style(line: NumberedDiffLine<'_>) -> Style {
    super::tool_diff_new_line_number_style_with_palette(line, &test_palette())
}

fn tool_title_spans(title: &ToolCardTitle, max_chars: usize) -> Vec<Span<'static>> {
    super::tool_title_spans_with_palette(title, max_chars, &test_palette())
}

fn line_plain_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn plain_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(line_plain_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn parsed_summary(value: Value) -> ToolCardRender {
    parse_tool_summary(&value.to_string())
}

fn base_summary(tool_name: &str) -> ToolCardRender {
    ToolCardRender {
        call_id: None,
        tool_name: tool_name.to_owned(),
        is_error: false,
        error_kind: None,
        summary: None,
        metadata: ToolCardMetadata::default(),
        preview_kind: ToolPreviewKind::Text,
        preview_lines: Vec::new(),
        hidden_lines: 0,
        preview_value: None,
        diff: None,
    }
}

#[test]
fn tool_card_classifies_shell_search_variants_and_rejects_complex_commands() {
    let rg = classify_simple_shell_search("rg --glob '*.rs' needle src")
        .expect("rg search should classify");
    let fd =
        classify_simple_shell_search("fd -e rs matcher crates").expect("fd search should classify");
    let find = classify_simple_shell_search("find src -name '*.rs'").expect("find should classify");

    assert_eq!(rg.pattern, "needle");
    assert_eq!(rg.location.as_deref(), Some("src"));
    assert_eq!(fd.pattern, "matcher");
    assert_eq!(fd.location.as_deref(), Some("crates"));
    assert_eq!(find.pattern, "*.rs");
    assert_eq!(find.location.as_deref(), Some("src"));

    assert!(classify_simple_shell_search("FOO=1 rg needle src").is_none());
    assert!(classify_simple_shell_search("grep needle src | head").is_none());
    assert!(classify_simple_shell_search("grep 'needle src").is_none());
}

#[test]
fn tool_card_parses_legacy_previews_and_mcp_metadata() {
    let markdown = parsed_summary(json!({
        "tool_name": "read_file",
        "status": "ok",
        "content": (1..=20)
            .map(|index| if index == 1 {
                "# Title".to_owned()
            } else {
                format!("- item {index}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }));
    let json_summary = parsed_summary(json!({
        "tool_name": "custom_tool",
        "status": "ok",
        "content": {"root": {"leaf": "value"}}
    }));
    let mcp = parsed_summary(json!({
        "tool_name": "mcp__filesystem__stat",
        "status": "ok",
        "metadata": {
            "details": {
                "call": {
                    "summary": "path=README.md id=call_123",
                    "subjects": [
                        "unknown:mcp_tool:mcp__filesystem__stat",
                        "unknown:mcp_trust_class:mcp_trust_class:workspace"
                    ]
                }
            }
        }
    }));
    let mcp_display = mcp_tool_display(&mcp).expect("expected parsed mcp display");
    let mcp_card = build_tool_card_display(&mcp);

    assert!(markdown.preview_kind == ToolPreviewKind::Markdown);
    assert_eq!(markdown.preview_lines.len(), 18);
    assert_eq!(markdown.hidden_lines, 2);

    assert!(json_summary.preview_kind == ToolPreviewKind::Json);
    assert!(json_summary.preview_value.is_some());
    assert!(!json_summary.preview_lines.is_empty());

    assert_eq!(mcp.metadata.mcp_server.as_deref(), Some("filesystem"));
    assert_eq!(mcp.metadata.mcp_tool.as_deref(), Some("stat"));
    assert_eq!(mcp.metadata.mcp_trust_class.as_deref(), Some("workspace"));
    assert_eq!(mcp_display.server, "filesystem");
    assert_eq!(mcp_display.tool, "stat");
    assert_eq!(mcp_card.title.plain(), "Called stat on filesystem");
    assert_eq!(mcp_card.status.detail.as_deref(), Some("trust workspace"));
}

#[test]
fn tool_card_collapsed_preview_and_title_truncation_cover_edge_cases() {
    let long_summary = ToolCardRender {
        preview_lines: (1..=8).map(|index| format!("line {index}")).collect(),
        hidden_lines: 2,
        ..base_summary("bash")
    };
    let collapsed = render_tool_collapsed_preview_body(&long_summary, accent_rose(), 80);
    let collapsed_text = plain_text(&collapsed);
    let title = ToolCardTitle::new(
        "Called",
        "extraordinarily-long-tool-name",
        Some("with very long trailing arguments".to_owned()),
    );
    let title_spans = tool_title_spans(&title, 12);
    let title_text = title_spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(collapsed.len(), COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS + 1);
    assert!(collapsed_text.contains("output"));
    assert!(collapsed_text.contains("line 1"));
    assert!(collapsed_text.contains("line 2"));
    assert!(collapsed_text.contains("line 3"));
    assert!(collapsed_text.contains("7 more lines hidden"));
    assert!(!collapsed_text.contains("line 4"));

    assert!(title_text.ends_with("..."));
    assert!(title_spans[0].style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn tool_card_render_path_and_generic_previews_cover_fallbacks() {
    let inferred_paths = ToolCardRender {
        tool_name: "glob".to_owned(),
        preview_lines: vec![
            "[".to_owned(),
            "\"src/lib.rs\",".to_owned(),
            "\"src/main.rs\"".to_owned(),
            "]".to_owned(),
        ],
        hidden_lines: 1,
        ..base_summary("glob")
    };
    let markdown_preview = ToolCardRender {
        preview_kind: ToolPreviewKind::Markdown,
        preview_lines: vec!["# Title".to_owned(), "- item".to_owned()],
        hidden_lines: 2,
        ..base_summary("custom_tool")
    };
    let json_preview = ToolCardRender {
        preview_value: Some(json!({"root": {"leaf": "value"}})),
        ..base_summary("custom_tool")
    };

    let inferred_lines = render_path_list_preview(&inferred_paths, accent_rose())
        .expect("expected inferred path list preview");
    let markdown_lines = render_generic_tool_preview(&markdown_preview, accent_rose(), 80);
    let json_lines = render_generic_tool_preview(&json_preview, accent_rose(), 80);

    assert!(plain_text(&inferred_lines).contains("3 paths"));
    assert!(plain_text(&inferred_lines).contains("src/lib.rs"));
    assert!(plain_text(&inferred_lines).contains("1 more lines hidden"));

    let markdown_text = plain_text(&markdown_lines);
    assert!(markdown_text.contains("formatted preview"));
    assert!(markdown_text.contains("Title"));
    assert!(markdown_text.contains("2 more lines hidden"));

    let json_text = plain_text(&json_lines);
    assert!(json_text.contains("structured payload"));
    assert!(json_text.contains("root"));
    assert!(json_text.contains("leaf"));
}

#[test]
fn tool_card_renders_agent_tool_status_and_result_pages() {
    let read_result = parsed_summary(json!({
        "tool_name": "read_agent_result",
        "status": "ok",
        "summary": "first 4/4 lines · 260 B",
        "preview_kind": "markdown",
        "preview_lines": ["# Child result", "", "- detailed result"],
        "preview_value": {
            "thread_id": "thread_1",
            "status": "completed",
            "session_ref": "children/thread_1.jsonl",
            "output_hash": "hash",
            "page": {
                "text": "# Child result\n\n- detailed result",
                "offset_chars": 4000,
                "returned_chars": 260,
                "total_chars": 4260,
                "next_offset_chars": null,
                "truncated": false
            }
        }
    }));
    let wait_result = parsed_summary(json!({
        "tool_name": "wait_agent",
        "status": "ok",
        "preview_kind": "json",
        "preview_value": {
            "thread_id": "thread_1",
            "status": "running",
            "terminal": false,
            "reason": null,
            "action_hint": "Ctrl-B background",
            "result_available": false,
            "result_ref": null
        }
    }));
    let named_wait_result = parsed_summary(json!({
        "tool_name": "wait_agent",
        "status": "ok",
        "preview_kind": "json",
        "preview_value": {
            "thread_id": "agent_chat_raw_id",
            "display_name": "kernel explorer",
            "status": "running",
            "terminal": false,
            "result_available": false
        }
    }));
    let spawn_result = parsed_summary(json!({
        "tool_name": "spawn_agent",
        "status": "ok",
        "preview_kind": "markdown",
        "preview_lines": ["## Summary", "Done"],
        "preview_value": {
            "thread_id": "thread_2",
            "status": "completed",
            "summary": "## Summary\nDone",
            "summary_truncated": true,
            "result_fetch": {
                "tool": "read_agent_result",
                "thread_id": "thread_2"
            }
        }
    }));
    let running_spawn_result = parsed_summary(json!({
        "tool_name": "spawn_agent",
        "status": "ok",
        "preview_kind": "json",
        "preview_lines": [
            "{",
            "  \"coalescing_key\": \"wait_agent:agent_chat_1\",",
            "  \"next_action\": \"continue independent parent work\"",
            "}"
        ],
        "preview_value": {
            "thread_id": "agent_chat_1",
            "display_name": "mailbox audit",
            "status": "running",
            "terminal": false,
            "reason": "agent tool spawned child session",
            "result_available": false,
            "next_action": "continue independent parent work"
        }
    }));
    let wait_ready = parsed_summary(json!({
        "tool_name": "wait_agent",
        "status": "ok",
        "preview_kind": "json",
        "preview_value": {
            "thread_id": "thread_ready",
            "status": "completed",
            "terminal": true,
            "reason": "finished cleanly",
            "result_available": true,
            "result_ref": {
                "read_tool": "read_agent_result",
                "thread_id": "thread_ready"
            }
        }
    }));
    let message_result = parsed_summary(json!({
        "tool_name": "message_agent",
        "status": "ok",
        "preview_kind": "json",
        "preview_value": {
            "thread_id": "thread_msg",
            "status": "failed"
        }
    }));
    let close_result = parsed_summary(json!({
        "tool_name": "close_agent",
        "status": "ok",
        "preview_kind": "json",
        "preview_value": {
            "thread_id": "thread_close",
            "status": "closed"
        }
    }));
    let fallback_result = parsed_summary(json!({
        "tool_name": "agent_custom",
        "status": "ok",
        "metadata": {
            "details": {
                "call": {
                    "summary": "profile_id=profile-thread"
                }
            }
        },
        "preview_kind": "json",
        "preview_value": {
            "status": 404
        }
    }));
    let missing_payload = parsed_summary(json!({
        "tool_name": "wait_agent",
        "status": "ok",
        "metadata": {
            "details": {
                "call": {
                    "summary": "thread_id=thread_arg"
                }
            }
        },
        "preview_kind": "json"
    }));

    let read_display = build_tool_card_display(&read_result);
    let wait_display = build_tool_card_display(&wait_result);
    let named_wait_display = build_tool_card_display(&named_wait_result);
    let spawn_display = build_tool_card_display(&spawn_result);
    let running_spawn_display = build_tool_card_display(&running_spawn_result);
    let wait_ready_display = build_tool_card_display(&wait_ready);
    let message_display = build_tool_card_display(&message_result);
    let close_display = build_tool_card_display(&close_result);
    let fallback_display = build_tool_card_display(&fallback_result);
    let missing_payload_display = build_tool_card_display(&missing_payload);
    let read_text = plain_text(&render_tool_preview_body(&read_result, accent_rose(), 96));
    let wait_text = plain_text(&render_tool_preview_body(&wait_result, accent_rose(), 96));
    let spawn_text = plain_text(&render_tool_preview_body(&spawn_result, accent_rose(), 96));
    let running_spawn_text = plain_text(&render_tool_preview_body(
        &running_spawn_result,
        accent_rose(),
        96,
    ));
    let wait_ready_text = plain_text(&render_tool_preview_body(&wait_ready, accent_rose(), 96));
    let missing_payload_text = plain_text(&render_tool_preview_body(
        &missing_payload,
        accent_rose(),
        96,
    ));

    assert_eq!(read_display.title.plain(), "Read agent result thread_1");
    assert_eq!(read_display.status.label, "DONE");
    assert_eq!(read_display.summary.as_deref(), Some("chars 4000+260/4260"));
    assert!(read_text.contains("result"));
    assert!(read_text.contains("completed · thread_1"));
    assert!(!read_text.contains("Child result"));
    assert!(!read_text.contains("detailed result"));
    assert!(!read_text.contains("structured payload"));

    assert_eq!(wait_display.title.plain(), "Checked agent thread_1");
    assert_eq!(wait_display.status.label, "RUNNING");
    assert_eq!(wait_display.summary.as_deref(), Some("result pending"));
    assert!(wait_text.contains("running · thread_1"));
    assert!(wait_text.contains("action Ctrl-B background"));

    assert_eq!(
        named_wait_display.title.plain(),
        "Checked agent kernel explorer"
    );

    assert_eq!(spawn_display.title.plain(), "Started agent thread_2");
    assert_eq!(spawn_display.status.label, "DONE");
    assert_eq!(
        spawn_display.summary.as_deref(),
        Some("summary truncated · read_agent_result available")
    );
    assert!(spawn_text.contains("Use read_agent_result"));
    assert_eq!(
        running_spawn_display.title.plain(),
        "Started agent mailbox audit"
    );
    assert_eq!(running_spawn_display.status.label, "RUNNING");
    assert_eq!(
        running_spawn_display.summary.as_deref(),
        Some("result pending")
    );
    assert!(running_spawn_text.contains("running · mailbox audit"));
    assert!(running_spawn_text.contains("agent tool spawned child session"));
    assert!(running_spawn_text.contains("action continue independent parent work"));
    assert!(!running_spawn_text.contains("coalescing_key"));

    assert_eq!(wait_ready_display.status.label, "DONE");
    assert_eq!(wait_ready_display.summary.as_deref(), Some("result ready"));
    assert!(wait_ready_text.contains("completed · thread_ready"));
    assert!(wait_ready_text.contains("result ready"));
    assert!(wait_ready_text.contains("finished cleanly"));
    assert!(wait_ready_text.contains("read_agent_result"));

    assert_eq!(message_display.title.plain(), "Messaged agent thread_msg");
    assert_eq!(message_display.status.label, "FAILED");
    assert!(message_display.status.is_error);
    assert_eq!(close_display.title.plain(), "Closed agent thread_close");
    assert_eq!(close_display.status.label, "CLOSED");
    assert_eq!(
        agent_tool_title(&fallback_result).plain(),
        "Called agent profile thread"
    );
    assert_eq!(
        fallback_display.title.plain(),
        "Called agent_custom profile_id=profile-thread"
    );
    assert_eq!(agent_tool_display_status("custom").label, "AGENT");
    assert_eq!(fallback_display.status.label, "OK");
    assert_eq!(
        missing_payload_display.title.plain(),
        "Checked agent thread_arg"
    );
    assert_eq!(missing_payload_display.status.label, "OK");
    assert!(missing_payload_text.contains("unknown · thread_arg"));
}

#[test]
fn tool_card_render_bash_and_diff_previews_cover_no_output_and_truncation() {
    let bash_error = ToolCardRender {
        is_error: true,
        error_kind: Some("exit_status".to_owned()),
        summary: Some("last 0/0 lines".to_owned()),
        metadata: ToolCardMetadata {
            exit_code: Some(9),
            stderr_bytes: Some(32),
            ..ToolCardMetadata::default()
        },
        ..base_summary("bash")
    };
    let no_output = ToolCardRender {
        summary: Some("0 lines · 0 B".to_owned()),
        metadata: ToolCardMetadata {
            exit_code: Some(0),
            execution_backend: Some("docker".to_owned()),
            execution_network_policy: Some("denied".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("bash")
    };
    let timed_out = ToolCardRender {
        is_error: true,
        error_kind: Some("timeout".to_owned()),
        metadata: ToolCardMetadata {
            execution_timeout_source: Some("wall_clock".to_owned()),
            execution_cleanup_status: Some("completed".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("bash")
    };
    let diff_summary = ToolCardRender {
        diff: Some(ToolCardDiff {
            summary: "+1 -0 · 1 file · truncated".to_owned(),
            truncated: true,
            original_line_count: 9,
            rendered_line_count: 5,
            files: vec![ToolCardDiffFile {
                path: "note.txt".to_owned(),
                lines: vec![
                    "--- current/note.txt".to_owned(),
                    "+++ proposed/note.txt".to_owned(),
                    "@@ -1 +1 @@".to_owned(),
                    "+new".to_owned(),
                ],
                truncated: false,
                original_line_count: 4,
                rendered_line_count: 4,
            }],
        }),
        ..base_summary("write_file")
    };
    let diff = diff_summary
        .diff
        .as_ref()
        .expect("expected diff payload for preview");

    let bash_lines = render_bash_preview(&bash_error, accent_rose());
    let diff_lines = render_tool_diff_preview(&diff_summary, diff, accent_rose());

    assert!(plain_text(&bash_lines).contains("stderr"));
    assert!(plain_text(&bash_lines).contains("exit 9"));
    assert!(plain_text(&bash_lines).contains("(no output)"));
    assert_eq!(
        tool_display_summary(&no_output).as_deref(),
        Some("(no output)")
    );
    assert_eq!(
        build_tool_card_display(&no_output).status.detail.as_deref(),
        Some("exit 0 · docker network denied")
    );
    let timeout_display = build_tool_card_display(&timed_out);
    assert_eq!(timeout_display.status.label, "TIMEOUT");
    assert_eq!(
        timeout_display.status.detail.as_deref(),
        Some("timeout wall_clock · cleanup completed")
    );

    let diff_text = plain_text(&diff_lines);
    assert!(diff_text.contains("created"));
    assert!(diff_text.contains("1 hunk"));
    assert!(diff_text.contains("diff truncated"));
    assert!(diff_text.contains("4 lines hidden"));
}

#[test]
fn tool_card_render_code_intelligence_previews_show_servers_and_diagnostics() {
    let symbols = ToolCardRender {
        tool_name: "code_workspace_symbols".to_owned(),
        preview_value: Some(json!({
            "server": "rust-analyzer",
            "capability": "workspace/symbol",
            "metadata": {"returned": 17, "total": 20},
            "servers": [
                {"server": "rust-analyzer", "status": "ready", "languages": ["rust"]},
                {"server": "taplo", "status": "ready", "languages": ["toml"]},
                {"server": "json-lsp", "status": "ready", "languages": ["json"]},
                {"server": "yaml-lsp", "status": "ready", "languages": ["yaml"]}
            ],
            "workspace_symbols": (0..17)
                .map(|index| json!({
                    "kind": "struct",
                    "name": format!("Symbol{index}"),
                    "path": "src/lib.rs",
                    "container_name": "AppState",
                    "range": {"start_line": index + 1, "start_character": 0}
                }))
                .collect::<Vec<_>>()
        })),
        ..base_summary("code_workspace_symbols")
    };
    let diagnostics = base_summary("code_diagnostics");
    let diagnostic_row = code_intelligence_row(
        &diagnostics,
        &json!({
            "severity": "warning",
            "path": "src/lib.rs",
            "message": "unused variable",
            "source": "clippy",
            "range": {"start_line": 4, "start_character": 7}
        }),
    )
    .expect("diagnostic rows should render");

    let symbol_lines = render_code_intelligence_preview(&symbols, accent_rose(), 96);
    let symbol_text = plain_text(&symbol_lines);
    let diagnostic_text = diagnostic_row
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(symbol_text.contains("LSP · rust-analyzer · workspace symbols · 17/20"));
    assert!(symbol_text.contains("servers"));
    assert!(symbol_text.contains("+1 more"));
    assert!(symbol_text.contains("Symbol0"));
    assert!(symbol_text.contains("in AppState"));
    assert!(symbol_text.contains("4 more lines hidden"));

    assert_eq!(diagnostic_row[0].content.as_ref().trim(), "warning");
    assert_eq!(diagnostic_row[0].style.fg, Some(accent_gold()));
    assert!(diagnostic_text.contains("src/lib.rs:4:7"));
    assert!(diagnostic_text.contains("clippy: "));
    assert!(diagnostic_text.contains("unused variable"));
}

#[test]
fn tool_card_render_entry_lines_respect_selection_and_hidden_preview_state() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: json!({
            "call_id": "call-read",
            "tool_name": "read_file",
            "status": "ok",
            "preview_kind": "markdown",
            "summary": "first 2/4 lines · 42 B",
            "preview_lines": ["# Title", "body"],
            "hidden_lines": 2,
            "metadata": {
                "details": {
                    "call": {"summary": "path=README.md"}
                }
            }
        })
        .to_string(),
    };
    let options = TimelineRenderOptions {
        selected_tool_activity_key: Some("call:call-read".to_owned()),
        max_content_width: 72,
        ..TimelineRenderOptions::default()
    };

    let lines = render_tool_entry_lines(&entry, &options, 0);
    let text = plain_text(&lines);

    assert!(text.contains("Read README.md"));
    assert!(text.contains("●"));
    assert!(text.contains("Title"));
    assert!(text.contains("body"));
    assert!(text.contains("2 more lines hidden"));
    assert!(!text.contains("preview hidden"));
}

#[test]
fn tool_card_frame_keeps_header_meta_and_body_in_one_block() {
    let palette = test_palette();
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: json!({
            "call_id": "call-frame",
            "tool_name": "read_file",
            "status": "ok",
            "preview_kind": "markdown",
            "summary": "first 2/4 lines · 42 B",
            "preview_lines": ["# Title", "body"],
            "hidden_lines": 2,
            "metadata": {
                "details": {
                    "call": {"summary": "path=README.md"}
                }
            }
        })
        .to_string(),
    };
    let options = TimelineRenderOptions {
        selected_tool_activity_key: Some("call:call-frame".to_owned()),
        max_content_width: 72,
        ..TimelineRenderOptions::default()
    };

    let lines = render_tool_entry_lines(&entry, &options, 0);
    let text = plain_text(&lines);

    assert_eq!(lines[0].spans[0].content.as_ref(), "●");
    assert_eq!(lines[1].spans[0].content.as_ref(), "└ ");
    assert_eq!(lines[1].spans[0].style.fg, lines[0].spans[0].style.fg);
    assert!(lines.iter().skip(2).all(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.content.as_ref() == "  ")
    }));
    assert!(lines.iter().all(|line| {
        line.spans
            .first()
            .is_some_and(|span| span.style.bg == Some(palette.surface_selection))
    }));
    assert!(text.contains("Read README.md"));
    assert!(text.contains("first 2/4 lines · 42 B"));
    assert!(text.contains("document excerpt"));
    assert!(text.contains("2 more lines hidden"));
}

#[test]
fn tool_card_agent_title_omits_duplicate_generic_agent_label() {
    let denied_spawn = parsed_summary(json!({
        "tool_name": "spawn_agent",
        "status": "error",
        "error_kind": "permission_denied",
        "preview_kind": "json",
        "preview_value": {
            "status": "denied",
            "reason": "agent budget denied child session"
        }
    }));

    let display = build_tool_card_display(&denied_spawn);
    let lines = render_tool_entry_lines(
        &TimelineEntry {
            role: TimelineRole::Tool,
            text: json!({
                "tool_name": "spawn_agent",
                "status": "error",
                "error_kind": "permission_denied",
                "preview_kind": "json",
                "preview_value": {
                    "status": "denied",
                    "reason": "agent budget denied child session"
                }
            })
            .to_string(),
        },
        &TimelineRenderOptions {
            max_content_width: 72,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let text = plain_text(&lines);

    assert_eq!(display.title.plain(), "Started agent");
    assert_eq!(lines[1].spans[0].style.fg, lines[0].spans[0].style.fg);
    assert!(!text.contains("Started agent agent"));
    assert!(text.contains("agent budget denied child session"));
}

#[test]
fn tool_card_render_entry_lines_styles_hovered_header() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: json!({
            "call_id": "call-hover",
            "tool_name": "read_file",
            "status": "ok",
            "preview_kind": "markdown",
            "summary": "first 1/1 lines · 12 B",
            "preview_lines": ["body"],
            "hidden_lines": 0
        })
        .to_string(),
    };
    let options = TimelineRenderOptions {
        hovered_tool_activity_key: Some("call:call-hover".to_owned()),
        max_content_width: 72,
        ..TimelineRenderOptions::default()
    };

    let lines = render_tool_entry_lines(&entry, &options, 0);

    assert_eq!(lines[0].spans[0].style.fg, Some(accent_gold()));
    assert_eq!(lines[0].spans[0].content.as_ref(), "●");
}

#[test]
fn tool_card_render_entry_lines_use_configured_theme_palette() {
    let theme = Theme::builtin(sigil_kernel::ThemeId::SolarizedLight);
    let palette = theme.palette.clone();
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: json!({
            "call_id": "call-themed",
            "tool_name": "read_file",
            "status": "ok",
            "preview_kind": "markdown",
            "summary": "first 2/4 lines · 42 B",
            "preview_lines": ["# Title", "`code`"],
            "hidden_lines": 2
        })
        .to_string(),
    };
    let options = TimelineRenderOptions {
        hovered_tool_activity_key: Some("call:call-themed".to_owned()),
        max_content_width: 72,
        theme,
        ..TimelineRenderOptions::default()
    };

    let lines = render_tool_entry_lines(&entry, &options, 0);
    let code_span = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "code")
        .expect("inline code preview should render");
    let hidden_tail = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref().contains("2 more lines hidden"))
        .expect("hidden tail should render");

    assert_eq!(lines[0].spans[0].style.fg, Some(palette.accent_warning));
    assert_eq!(code_span.style.fg, Some(palette.markdown_code_fg));
    assert_eq!(code_span.style.bg, Some(palette.markdown_code_bg));
    assert_eq!(hidden_tail.style.fg, Some(palette.text_muted));
}

#[test]
fn tool_card_renders_terminal_task_status_and_preview() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: json!({
            "tool_name": "terminal_task",
            "status": "ok",
            "summary": "running · cargo test",
            "preview_kind": "text",
            "preview_lines": ["running tests", "test result: ok"],
            "hidden_lines": 1,
            "metadata": {
                "details": {
                    "terminal_task": {
                        "task_id": "terminal-1",
                        "status": "running",
                        "status_detail": { "state": "running" },
                        "command": "cargo test",
                        "cwd": ".",
                        "shell": "sh",
                        "log_path": ".sigil/tasks/terminal-1/output.log",
                        "created_at_ms": 10,
                        "updated_at_ms": 20,
                        "output_hash": "hash",
                        "output_truncated": true,
                        "enforcement_backend": "local",
                        "sandbox_profile": "unconfined"
                    }
                }
            }
        })
        .to_string(),
    };
    let activity = tool_activity_view(&entry, 0).expect("terminal card activity");
    let options = TimelineRenderOptions {
        expand_tool_previews: true,
        selected_tool_activity_key: Some(activity.key.clone()),
        max_content_width: 96,
        ..TimelineRenderOptions::default()
    };

    let lines = render_tool_entry_lines(&entry, &options, 0);
    let text = plain_text(&lines);

    assert_eq!(activity.key, "terminal_task:terminal-1");
    assert_eq!(activity.title, "Terminal terminal-1 cargo test");
    assert!(activity.defaults_expanded);
    assert!(text.contains("Terminal terminal-1 cargo test"));
    assert!(text.contains("RUNNING"));
    assert!(text.contains("local unconfined"));
    assert!(text.contains("terminal"));
    assert!(text.contains("log"));
    assert!(text.contains("running tests"));
    assert!(text.contains("1 more lines hidden"));
}

#[test]
fn tool_card_renders_terminal_task_failure_and_exit_details() {
    let failed = parsed_summary(json!({
        "tool_name": "terminal_task",
        "status": "error",
        "summary": "failed child process",
        "preview_kind": "text",
        "preview_lines": [],
        "hidden_lines": 0,
        "metadata": {
            "details": {
                "terminal_task": {
                    "task_id": "terminal-failed",
                    "status": "failed",
                    "status_detail": {
                        "state": "failed",
                        "reason": "child process could not be interrupted cleanly"
                    },
                    "command": "cargo test",
                    "log_path": ".sigil/tasks/terminal-failed/output.log"
                }
            }
        }
    }));
    let exited_from_top_level_details = parsed_summary(json!({
        "tool_name": "terminal_cancel",
        "status": "ok",
        "preview_kind": "text",
        "preview_lines": [],
        "hidden_lines": 0,
        "metadata": {
            "details": {
                "task_id": "terminal-exited",
                "status": "exited",
                "status_detail": {"state": "exited", "exit_code": 7},
                "command": "cargo test",
                "enforcement_backend": "local",
                "sandbox_profile": "unconfined",
                "cleanup": {"status": "completed"}
            }
        }
    }));

    let failed_display = build_tool_card_display(&failed);
    let failed_lines = render_tool_preview_body(&failed, accent_rose(), 96);
    let exited_display = build_tool_card_display(&exited_from_top_level_details);
    let exited_activity = build_tool_activity_view(
        &exited_from_top_level_details,
        &json!({"tool_name": "terminal_cancel"}).to_string(),
    );
    let generic_error = ToolCardRender {
        is_error: true,
        ..base_summary("terminal_task")
    };
    let generic_error_display = build_tool_card_display(&generic_error);
    let generic_ok = ToolCardRender {
        ..base_summary("terminal_task")
    };
    let generic_ok_display = build_tool_card_display(&generic_ok);

    assert_eq!(failed_display.status.label, "FAILED");
    assert!(failed_display.status.is_error);
    assert!(
        failed_display
            .status
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("could not be interrupted"))
    );
    assert!(plain_text(&failed_lines).contains("(no output preview)"));

    assert_eq!(exited_display.status.label, "EXITED");
    assert_eq!(
        exited_display.status.detail.as_deref(),
        Some("exit 7 · local unconfined · cleanup completed")
    );
    assert_eq!(exited_activity.key, "terminal_task:terminal-exited");
    assert_eq!(exited_activity.title, "Terminal terminal-exited cargo test");
    assert_eq!(generic_error_display.status.label, "ERROR");
    assert_eq!(generic_error_display.status.kind, StatusKind::Error);
    assert_eq!(generic_ok_display.status.label, "OK");
    assert_eq!(generic_ok_display.status.kind, StatusKind::Success);
}

#[test]
fn tool_card_activity_view_uses_stable_hash_without_call_id() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: json!({
            "tool_name": "ls",
            "status": "ok",
            "preview_kind": "json",
            "preview_lines": ["[]"],
            "preview_value": []
        })
        .to_string(),
    };

    let first = tool_activity_view(&entry, 0).expect("tool activity should render");
    let second = tool_activity_view(&entry, 1).expect("tool activity should render");

    assert!(first.key.starts_with("hash:"));
    assert_eq!(first.key, second.key);
    assert_eq!(first.title, "Listed workspace");
}

#[test]
fn tool_card_preview_renderers_cover_text_bash_and_file_change_variants() {
    let read = ToolCardRender {
        preview_kind: ToolPreviewKind::Text,
        preview_lines: vec!["alpha".to_owned()],
        hidden_lines: 1,
        ..base_summary("read_file")
    };
    let paths = ToolCardRender {
        preview_value: Some(json!(["src/lib.rs", "src/main.rs"])),
        hidden_lines: 1,
        ..base_summary("ls")
    };
    let bash_summary_only = ToolCardRender {
        summary: Some("1 line · 2 B".to_owned()),
        ..base_summary("bash")
    };
    let bash_plain = base_summary("bash");
    let file_change = ToolCardRender {
        metadata: ToolCardMetadata {
            changed_files: vec!["note.txt".to_owned()],
            ..ToolCardMetadata::default()
        },
        preview_lines: vec!["wrote note.txt".to_owned()],
        ..base_summary("write_file")
    };
    let generic_code = ToolCardRender {
        tool_name: "code_symbols".to_owned(),
        preview_lines: vec!["plain fallback".to_owned()],
        preview_kind: ToolPreviewKind::Text,
        ..base_summary("code_symbols")
    };

    let read_lines = render_tool_preview_body(&read, accent_rose(), 72);
    let path_lines =
        render_path_list_preview(&paths, accent_rose()).expect("expected path preview");
    let bash_summary_lines = render_bash_preview(&bash_summary_only, accent_rose());
    let bash_plain_lines = render_bash_preview(&bash_plain, accent_rose());
    let file_change_lines =
        render_file_change_preview(&file_change, accent_rose()).expect("expected file change");
    let generic_code_lines = render_tool_preview_body(&generic_code, accent_rose(), 72);

    assert!(plain_text(&read_lines).contains("file excerpt"));
    assert!(plain_text(&read_lines).contains("alpha"));
    assert!(plain_text(&read_lines).contains("1 more lines hidden"));
    assert!(plain_text(&path_lines).contains("files"));
    assert!(plain_text(&path_lines).contains("3 paths"));
    assert!(plain_text(&bash_summary_lines).contains("1 line · 2 B"));
    assert!(plain_text(&bash_plain_lines).contains("terminal tail"));
    assert!(plain_text(&file_change_lines).contains("1 changed"));
    assert!(plain_text(&file_change_lines).contains("write summary"));
    assert!(plain_text(&generic_code_lines).contains("captured output"));
    assert_eq!(
        render_grep_preview(&base_summary("grep"), accent_rose()),
        None
    );
    assert_eq!(
        render_file_change_preview(&base_summary("write_file"), accent_rose()),
        None
    );
}

#[test]
fn tool_card_code_intelligence_helpers_cover_remaining_labels_and_fallbacks() {
    let definition = base_summary("code_definition");
    let references = base_summary("code_references");
    let diagnostics = base_summary("code_diagnostics");
    let actions = base_summary("code_actions");
    let empty_servers = code_intelligence_servers_line(&json!({
        "servers": [{"status": "ready"}]
    }));
    let fallback_generic = ToolCardRender {
        tool_name: "code_references".to_owned(),
        preview_value: Some(json!({"server": "custom", "capability": "custom/run"})),
        preview_kind: ToolPreviewKind::Json,
        ..base_summary("code_references")
    };
    let actions_preview = ToolCardRender {
        preview_value: Some(json!({
            "server": "rust-analyzer",
            "capability": "textDocument/codeAction",
            "code_actions": [{
                "title": "Replace symbol",
                "kind": "quickfix",
                "has_edit": true,
                "has_command": false
            }],
            "metadata": {"returned": 1, "total": 1}
        })),
        preview_kind: ToolPreviewKind::Json,
        ..base_summary("code_actions")
    };
    let definition_row = code_intelligence_row(
        &definition,
        &json!({
            "path": "src/lib.rs",
            "preview": "fn helper()",
            "range": {"start_line": 9}
        }),
    )
    .expect("definition rows should render");
    let reference_row = code_intelligence_row(
        &references,
        &json!({
            "path": "src/lib.rs",
            "name": "helper",
            "range": {"start_line": 12, "start_character": 0}
        }),
    )
    .expect("reference rows should render");
    let action_row = code_intelligence_row(
        &actions,
        &json!({
            "title": "Replace symbol",
            "kind": "quickfix",
            "has_edit": true,
            "has_command": false
        }),
    )
    .expect("action rows should render");
    let command_action_row = code_intelligence_row(
        &actions,
        &json!({
            "title": "Run command",
            "has_edit": false,
            "has_command": true
        }),
    )
    .expect("command action rows should render");
    let inspect_action_row = code_intelligence_row(
        &actions,
        &json!({
            "title": "Inspect only",
            "has_edit": false,
            "has_command": false
        }),
    )
    .expect("inspect action rows should render");
    let fallback_lines = render_code_intelligence_preview(&fallback_generic, accent_rose(), 72);
    let actions_lines = render_code_intelligence_preview(&actions_preview, accent_rose(), 72);

    assert_eq!(code_intelligence_section(&definition), "definition");
    assert_eq!(code_intelligence_section(&references), "references");
    assert_eq!(code_intelligence_section(&diagnostics), "diagnostics");
    assert_eq!(code_intelligence_section(&actions), "actions");
    assert_eq!(
        code_intelligence_source_label("tree-sitter-rust", "workspace/symbol"),
        "Tree-sitter"
    );
    assert_eq!(
        code_intelligence_source_label("custom", "custom/run"),
        "Code"
    );
    assert_eq!(
        code_intelligence_capability_label("textDocument/documentSymbol"),
        "document symbols"
    );
    assert_eq!(
        code_intelligence_capability_label("textDocument/definition"),
        "definition"
    );
    assert_eq!(
        code_intelligence_capability_label("textDocument/references"),
        "references"
    );
    assert_eq!(
        code_intelligence_capability_label("tree_sitter/diagnostics"),
        "diagnostics"
    );
    assert_eq!(
        code_intelligence_capability_label("custom/run"),
        "custom / run"
    );
    assert!(empty_servers.is_none());
    assert_eq!(
        code_intelligence_server_label(&json!({"status": "ready"})),
        None
    );
    assert_eq!(
        code_intelligence_server_label(&json!({"server": "rust-analyzer"})).as_deref(),
        Some("rust-analyzer ready")
    );
    assert_eq!(diagnostic_severity_color("error"), accent_rose());
    assert_eq!(diagnostic_severity_color("info"), accent_teal());
    assert_eq!(
        file_change_count_label(&base_summary("write_file")),
        "changed"
    );
    assert_eq!(
        file_change_result_label(&base_summary("edit_file")),
        "edit summary"
    );
    assert_eq!(
        file_change_result_label(&base_summary("write_file")),
        "write summary"
    );
    assert_eq!(
        file_change_result_label(&base_summary("custom_tool")),
        "file summary"
    );
    assert!(
        definition_row
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("src/lib.rs:9")
    );
    assert!(
        reference_row
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("ref")
    );
    let action_text = action_row
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(action_text.contains("quickfix"));
    assert!(action_text.contains("edit"));
    assert!(action_text.contains("Replace symbol"));
    assert!(
        command_action_row
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("command")
    );
    assert!(
        inspect_action_row
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("inspect")
    );
    assert!(plain_text(&fallback_lines).contains("structured payload"));
    assert!(plain_text(&actions_lines).contains("Replace symbol"));
}

#[test]
fn tool_card_diff_and_title_helpers_cover_remaining_branches() {
    let changed = ToolCardDiffFile {
        path: "note.txt".to_owned(),
        lines: vec![" context".to_owned()],
        truncated: false,
        original_line_count: 1,
        rendered_line_count: 1,
    };
    let modified = ToolCardDiffFile {
        path: "note.txt".to_owned(),
        lines: vec!["-old".to_owned(), "+new".to_owned()],
        truncated: true,
        original_line_count: 4,
        rendered_line_count: 2,
    };
    let create = ToolCardDiffFile {
        path: "new.txt".to_owned(),
        lines: vec!["+created".to_owned()],
        truncated: false,
        original_line_count: 1,
        rendered_line_count: 1,
    };
    let summary = ToolCardRender {
        metadata: ToolCardMetadata {
            action: Some("delete".to_owned()),
            ..ToolCardMetadata::default()
        },
        diff: Some(ToolCardDiff {
            summary: "diff".to_owned(),
            truncated: false,
            original_line_count: 1,
            rendered_line_count: 1,
            files: vec![create],
        }),
        ..base_summary("write_file")
    };
    let diff_line = render_tool_diff_line(
        accent_rose(),
        NumberedDiffLine {
            text: "",
            kind: DiffLineKind::Added,
            old_line: None,
            new_line: Some(3),
        },
        2,
    );
    let exact_fit_title = ToolCardTitle::new("R", "subject", None);
    let exact_fit = tool_title_spans(&exact_fit_title, 4)
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(diff_hunk_summary(&changed), "0 hunks");
    assert_eq!(
        diff_hunk_summary(&ToolCardDiffFile {
            path: modified.path.clone(),
            lines: vec!["@@ -1 +1 @@".to_owned(), "@@ -2 +2 @@".to_owned(),],
            truncated: modified.truncated,
            original_line_count: modified.original_line_count,
            rendered_line_count: modified.rendered_line_count,
        }),
        "2 hunks"
    );
    assert_eq!(
        tool_diff_file_label(&base_summary("write_file"), &modified),
        "modified"
    );
    assert_eq!(
        tool_diff_file_label(&base_summary("write_file"), &changed),
        "changed"
    );
    assert_eq!(tool_diff_file_label(&summary, &changed), "deleted");
    assert_eq!(write_file_action(&summary), "Created");
    assert_eq!(write_file_action(&base_summary("write_file")), "Wrote");
    assert!(diff_file_is_create(
        summary
            .diff
            .as_ref()
            .and_then(|diff| diff.files.first())
            .expect("expected create diff")
    ));
    assert!(line_plain_text(&diff_line).contains("3"));
    assert_eq!(
        diff_line
            .spans
            .last()
            .expect("expected diff body span")
            .content
            .as_ref(),
        " "
    );
    assert_eq!(
        tool_diff_old_line_number_style(NumberedDiffLine {
            text: "",
            kind: DiffLineKind::Context,
            old_line: None,
            new_line: None,
        }),
        Style::default().fg(dim())
    );
    assert_eq!(
        tool_diff_new_line_number_style(NumberedDiffLine {
            text: "",
            kind: DiffLineKind::Context,
            old_line: Some(1),
            new_line: None,
        }),
        Style::default().fg(dim())
    );
    assert_eq!(exact_fit, "R...");
}

#[test]
fn tool_card_json_tree_and_parser_helpers_cover_empty_and_leaf_cases() {
    let mut keyed_lines = Vec::new();
    push_json_tree_lines(&json!({"nested": [1]}), "", Some("root"), &mut keyed_lines);
    let object_lines = render_json_tree_preview(&json!({}));
    let array_lines = render_json_tree_preview(&json!([]));
    let leaf_lines = render_json_tree_preview(&Value::Null);

    assert!(keyed_lines.iter().any(|line| line.contains("root: {}")));
    assert_eq!(object_lines[0], "{object}");
    assert_eq!(array_lines[0], "[array] 0");
    assert_eq!(leaf_lines[0], "null");
    assert_eq!(json_tree_leaf_text(&json!(true)), "true");
    assert_eq!(json_tree_leaf_text(&json!([1, 2])), "[2]");
    assert_eq!(json_tree_container_label(&json!({"a": 1})), "{1 keys}");
    assert_eq!(
        json_string_list(&json!(["a", 1, "b"])),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(
        infer_string_list_preview(&[
            "[".to_owned(),
            "\"src/lib.rs\",".to_owned(),
            "\"src/main.rs\"".to_owned(),
            "]".to_owned(),
        ]),
        vec!["src/lib.rs".to_owned(), "src/main.rs".to_owned()]
    );
    assert!(json_grep_matches(&json!([{}])).is_none());
    assert!(
        json_grep_matches(&json!([]))
            .expect("empty arrays should parse")
            .is_empty()
    );
    assert!(tool_name_matches("mcp_stat", "stat"));
}

#[test]
fn tool_card_parse_helpers_cover_fallbacks_defaults_and_metadata_sources() {
    let fallback = parse_tool_summary(
        "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9",
    );
    let nested_error = parsed_summary(json!({
        "tool_name": "bash",
        "status": "error",
        "error": {"kind": "exit_status"}
    }));
    let diff = parse_tool_diff(&json!({
        "files": [{"lines": ["+a"]}]
    }))
    .expect("expected diff defaults");
    let diff_file = parse_tool_diff_file(&json!({
        "lines": ["+a"]
    }))
    .expect("expected diff file defaults");
    let metadata = parse_tool_metadata(&json!({
        "details": {
            "mcp": {"server": "filesystem", "tool": "stat", "trust_class": "workspace"},
            "code_intelligence": {"server": "rust-analyzer", "capability": "workspace/symbol", "returned": 2, "total": 4},
            "execution": {
                "backend": "docker",
                "network": {"policy": "denied"},
                "resources": {
                    "timeout_source": "wall_clock",
                    "cleanup": {"status": "completed"}
                }
            },
            "terminal_task": {
                "task_id": "terminal-1",
                "status": "cancelled",
                "enforcement_backend": "local",
                "sandbox_profile": "unconfined",
                "cleanup": {"status": "completed"}
            }
        }
    }));
    let subjects = parse_mcp_call_subjects(Some(&json!({
        "subjects": [
            "malformed",
            "unknown:mcp_trust_class:bad-prefix",
            "unknown:other:value"
        ]
    })));

    assert_eq!(fallback.tool_name, "result");
    assert_eq!(fallback.preview_lines.len(), 8);
    assert_eq!(fallback.hidden_lines, 1);
    assert!(nested_error.is_error);
    assert_eq!(nested_error.error_kind.as_deref(), Some("exit_status"));
    assert_eq!(diff.summary, "file diff");
    assert_eq!(diff.original_line_count, 1);
    assert_eq!(diff.rendered_line_count, 1);
    assert_eq!(diff_file.path, "unknown");
    assert!(!diff_file.truncated);
    assert!(legacy_tool_preview_kind(&json!("plain text")) == ToolPreviewKind::Text);
    assert_eq!(
        legacy_tool_preview(None, ToolPreviewKind::Text),
        (Vec::new(), 0)
    );
    assert_eq!(metadata.mcp_server.as_deref(), Some("filesystem"));
    assert_eq!(metadata.mcp_tool.as_deref(), Some("stat"));
    assert_eq!(metadata.mcp_trust_class.as_deref(), Some("workspace"));
    assert_eq!(metadata.code_server.as_deref(), Some("rust-analyzer"));
    assert_eq!(metadata.returned_entries, Some(2));
    assert_eq!(metadata.total_entries, Some(4));
    assert_eq!(metadata.execution_backend.as_deref(), Some("docker"));
    assert_eq!(metadata.execution_network_policy.as_deref(), Some("denied"));
    assert_eq!(
        metadata.execution_timeout_source.as_deref(),
        Some("wall_clock")
    );
    assert_eq!(
        metadata.execution_cleanup_status.as_deref(),
        Some("completed")
    );
    assert_eq!(
        metadata.terminal_enforcement_backend.as_deref(),
        Some("local")
    );
    assert_eq!(
        metadata.terminal_sandbox_profile.as_deref(),
        Some("unconfined")
    );
    assert_eq!(
        metadata.terminal_cleanup_status.as_deref(),
        Some("completed")
    );
    assert_eq!(subjects, (None, None, None));
    assert_eq!(ToolPreviewKind::Markdown.label(), "md");
    assert_eq!(ToolPreviewKind::Json.description(), "structured preview");
    assert!(ToolPreviewKind::from_value("other") == ToolPreviewKind::Text);
}

#[test]
fn tool_card_action_title_helpers_cover_remaining_builtin_and_fallback_paths() {
    let code_symbols = ToolCardRender {
        metadata: ToolCardMetadata {
            call_summary: Some("path=src/lib.rs".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_symbols")
    };
    let code_workspace_symbols = ToolCardRender {
        metadata: ToolCardMetadata {
            call_summary: Some("query=AppState".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_workspace_symbols")
    };
    let code_definition = ToolCardRender {
        diff: Some(ToolCardDiff {
            summary: "diff".to_owned(),
            truncated: false,
            original_line_count: 1,
            rendered_line_count: 1,
            files: vec![ToolCardDiffFile {
                path: "src/lib.rs".to_owned(),
                lines: vec!["+a".to_owned()],
                truncated: false,
                original_line_count: 1,
                rendered_line_count: 1,
            }],
        }),
        ..base_summary("code_definition")
    };
    let code_references = ToolCardRender {
        metadata: ToolCardMetadata {
            changed_files: vec!["src/lib.rs".to_owned()],
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_references")
    };
    let code_diagnostics = ToolCardRender {
        ..base_summary("code_diagnostics")
    };
    let code_actions = ToolCardRender {
        metadata: ToolCardMetadata {
            call_summary: Some("path=src/lib.rs line=1 character=7".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_actions")
    };
    let code_action = ToolCardRender {
        metadata: ToolCardMetadata {
            changed_files: vec!["src/lib.rs".to_owned()],
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_action")
    };
    let code_rename = ToolCardRender {
        metadata: ToolCardMetadata {
            changed_files: vec!["src/lib.rs".to_owned()],
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_rename")
    };
    let fallback = ToolCardRender {
        tool_name: "custom_tool".to_owned(),
        metadata: ToolCardMetadata {
            call_summary: Some("id=abc call_123 mode=fast target=src".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("custom_tool")
    };

    assert_eq!(
        tool_action_title(&code_symbols).plain(),
        "Inspected src/lib.rs symbols"
    );
    assert_eq!(
        tool_action_title(&code_workspace_symbols).plain(),
        "Searched AppState workspace"
    );
    assert_eq!(
        tool_action_title(&code_definition).plain(),
        "Located src/lib.rs definition"
    );
    assert_eq!(
        tool_action_title(&code_references).plain(),
        "Searched src/lib.rs references"
    );
    assert_eq!(
        tool_action_title(&code_diagnostics).plain(),
        "Checked workspace diagnostics"
    );
    assert_eq!(
        tool_action_title(&code_actions).plain(),
        "Inspected src/lib.rs actions"
    );
    assert_eq!(
        tool_action_title(&code_action).plain(),
        "Applied src/lib.rs code action"
    );
    assert_eq!(
        tool_action_title(&code_rename).plain(),
        "Renamed src/lib.rs symbol"
    );
    assert!(file_change_tool(&code_action));
    assert!(file_change_tool(&code_rename));
    assert!(code_intelligence_tool(&code_actions));
    assert_eq!(
        tool_action_title(&fallback).plain(),
        "Called custom_tool mode=fast target=src"
    );
    assert_eq!(primary_path(&code_definition), "src/lib.rs");
    assert_eq!(
        call_summary_argument("command=cargo test -p sigil-tui", "command").as_deref(),
        Some("cargo test -p sigil-tui")
    );
    assert_eq!(call_summary_argument("path=", "path"), None);
    assert_eq!(
        parse_mcp_provider_name("mcp__filesystem__stat")
            .as_ref()
            .map(|(server, tool)| (server.as_str(), tool.as_str())),
        Some(("filesystem", "stat"))
    );
    assert_eq!(parse_mcp_provider_name("mcp____"), None);
    assert_eq!(
        sanitize_call_summary("id=123 call_456 path=src/lib.rs mode=fast"),
        "path=src/lib.rs mode=fast"
    );
}

#[test]
fn tool_card_read_file_preview_uses_document_and_file_sections() {
    let markdown_summary = ToolCardRender {
        preview_kind: ToolPreviewKind::Markdown,
        preview_lines: vec!["# Title".to_owned()],
        ..base_summary("read_file")
    };
    let text_summary = ToolCardRender {
        preview_kind: ToolPreviewKind::Text,
        preview_lines: vec!["fn main() {}".to_owned()],
        ..base_summary("read_file")
    };

    let markdown_text = plain_text(&render_read_file_preview(
        &markdown_summary,
        accent_rose(),
        80,
    ));
    let text = plain_text(&render_read_file_preview(&text_summary, accent_rose(), 80));

    assert!(markdown_text.contains("document excerpt"));
    assert!(text.contains("file excerpt"));
}

#[test]
fn tool_card_grep_bash_and_file_change_helpers_cover_remaining_labels() {
    let grep_summary = ToolCardRender {
        preview_value: Some(json!([])),
        ..base_summary("grep")
    };
    assert!(render_grep_preview(&grep_summary, accent_rose()).is_none());

    let bash_summary = ToolCardRender {
        is_error: true,
        metadata: ToolCardMetadata {
            exit_code: Some(7),
            stdout_bytes: Some(4),
            ..ToolCardMetadata::default()
        },
        ..base_summary("bash")
    };
    let bash_text = plain_text(&render_bash_preview(&bash_summary, accent_rose()));
    assert!(bash_text.contains("stdout"));
    assert!(bash_text.contains("exit 7"));

    let delete_summary = ToolCardRender {
        metadata: ToolCardMetadata {
            action: Some("delete".to_owned()),
            ..ToolCardMetadata::default()
        },
        ..base_summary("write_file")
    };
    let edit_summary = base_summary("edit_file");
    let write_summary = base_summary("write_file");
    let other_summary = base_summary("custom_tool");

    assert_eq!(file_change_count_label(&delete_summary), "deleted");
    assert_eq!(file_change_result_label(&delete_summary), "delete summary");
    assert_eq!(file_change_result_label(&edit_summary), "edit summary");
    assert_eq!(file_change_result_label(&write_summary), "write summary");
    assert_eq!(file_change_result_label(&other_summary), "file summary");
}

#[test]
fn tool_card_code_intelligence_helpers_cover_custom_sources_and_server_rollups() {
    let single_server = json!({
        "servers": [{ "server": "rust-analyzer", "status": "ready", "languages": ["rust"] }]
    });
    assert!(code_intelligence_servers_line(&single_server).is_none());

    let many_servers = json!({
        "servers": [
            { "server": "rust-analyzer", "status": "ready", "languages": ["rust", "toml", "json"] },
            { "server": "pyright", "status": "fallback" },
            { "server": "tsserver", "status": "ready", "languages": ["ts", "js"] },
            { "server": "clangd", "status": "ready", "languages": ["c", "cpp"] }
        ]
    });
    let server_text = code_intelligence_servers_line(&many_servers)
        .expect("expected summarized server line")
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(server_text.contains("rust-analyzer ready (rust,toml)"));
    assert!(server_text.contains("pyright fallback"));
    assert!(server_text.contains("+1 more"));

    let summary = ToolCardRender {
        tool_name: "code_references".to_owned(),
        preview_value: Some(json!({
            "server": "custom-index",
            "capability": "custom/capability",
            "metadata": { "returned": 1, "total": 3 },
            "results": [
                {
                    "path": "src/lib.rs",
                    "preview": "fn demo()",
                    "container_name": "crate::demo"
                }
            ]
        })),
        metadata: ToolCardMetadata {
            returned_entries: Some(1),
            total_entries: Some(3),
            ..ToolCardMetadata::default()
        },
        ..base_summary("code_references")
    };
    let text = plain_text(&render_code_intelligence_preview(
        &summary,
        accent_rose(),
        80,
    ));
    assert!(text.contains("custom / capability"));
    assert!(text.contains("src/lib.rs:1"));
    assert!(text.contains("fn demo()"));
    assert!(text.contains("in crate::demo"));
    assert!(text.contains("2 more lines hidden"));
}

#[test]
fn tool_card_diff_and_json_helpers_cover_deleted_lines_and_nested_arrays() {
    let diff_file = ToolCardDiffFile {
        path: "old.txt".to_owned(),
        lines: vec!["-gone".to_owned()],
        truncated: false,
        original_line_count: 1,
        rendered_line_count: 1,
    };
    assert_eq!(
        tool_diff_file_label(&base_summary("edit_file"), &diff_file),
        "deleted"
    );

    let numbered = number_unified_diff_lines(["-gone"]);
    assert_eq!(
        tool_diff_old_line_number_style(numbered[0]),
        Style::default().fg(dim())
    );

    let tree = render_json_tree_preview(&json!([{ "nested": [1, { "leaf": true }] }]));
    let tree_text = tree.join("\n");
    assert!(tree_text.contains("[array] 1"));
    assert!(tree_text.contains("[0] {1 keys}"));
    assert!(tree_text.contains("nested: [2 items]"));
    assert!(tree_text.contains("leaf: true"));

    assert_eq!(
        json_string_list(&json!([1, true])).expect("array should still collect"),
        Vec::<String>::new()
    );
}
