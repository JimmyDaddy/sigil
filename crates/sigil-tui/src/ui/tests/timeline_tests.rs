use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::ui::theme::Theme;

use super::*;

fn rendered_plain_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

#[test]
fn strip_timeline_content_indent_spans_handles_empty_and_non_indent_lines() {
    assert!(strip_timeline_content_indent_spans(Vec::new()).is_empty());

    let spans = strip_timeline_content_indent_spans(vec![Span::raw("body")]);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content.as_ref(), "body");

    let stripped = strip_timeline_content_indent_spans(vec![Span::raw("  "), Span::raw("body")]);
    assert_eq!(stripped.len(), 1);
    assert_eq!(stripped[0].content.as_ref(), "body");
}

fn render_timeline_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    render_timeline_entry_lines_with_options(entry, &TimelineRenderOptions::default(), 0)
}

#[test]
fn render_timeline_entry_lines_preserves_multiline_blocks() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: "first line\nsecond line\nthird line".to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("text"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("first line"))
    }));
}

#[test]
fn render_timeline_entry_lines_handles_empty_user_assistant_and_system_entries() {
    let empty_user = TimelineEntry {
        role: TimelineRole::User,
        text: "   ".to_owned(),
    };
    let empty_assistant = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "\n".to_owned(),
    };
    let empty_system = TimelineEntry {
        role: TimelineRole::System,
        text: String::new(),
    };

    assert!(render_timeline_entry_lines(&empty_user).is_empty());
    assert!(render_timeline_entry_lines(&empty_assistant).is_empty());
    assert_eq!(render_timeline_entry_lines(&empty_system).len(), 2);
}

#[test]
fn render_timeline_entry_lines_highlights_indented_user_slash_command() {
    let entry = TimelineEntry {
        role: TimelineRole::User,
        text: "  /task fix typo".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    let command_span = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "/task")
        .expect("slash command span should render");

    assert_eq!(
        command_span.style.fg,
        Some(Theme::default().palette.accent_info)
    );
    assert!(command_span.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn render_timeline_entry_lines_marks_intermediate_assistant_info_only() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "checking **provider** shape".to_owned(),
    };

    let normal =
        render_timeline_entry_lines_with_options(&entry, &TimelineRenderOptions::default(), 0);
    let normal_plain = rendered_plain_lines(&normal).join("\n");
    assert!(normal_plain.contains("checking provider shape"));
    assert!(!normal_plain.contains("• checking provider shape"));

    let marked = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            intermediate_assistant_indices: std::collections::BTreeSet::from([0]),
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let marked_plain = rendered_plain_lines(&marked).join("\n");
    assert!(marked_plain.contains("• checking provider shape"));

    let thinking = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "checking provider shape".to_owned(),
    };
    let thinking_marked = render_timeline_entry_lines_with_options(
        &thinking,
        &TimelineRenderOptions {
            intermediate_assistant_indices: std::collections::BTreeSet::from([0]),
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let thinking_plain = rendered_plain_lines(&thinking_marked).join("\n");
    assert!(thinking_plain.contains("thought"));
    assert!(!thinking_plain.contains("•"));
}

#[test]
fn render_timeline_entry_lines_marks_flush_left_intermediate_assistant_heading() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "## Heading\nbody".to_owned(),
    };

    let marked = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            intermediate_assistant_indices: std::collections::BTreeSet::from([0]),
            ..TimelineRenderOptions::default()
        },
        0,
    );

    assert_eq!(marked[0].spans[0].content.as_ref(), "• ");
    assert_eq!(
        marked[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "• Heading"
    );
}

#[test]
fn render_timeline_entry_lines_separates_tool_header_and_json_body() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{"tool_name":"ls","status":"ok","call_id":"call_123","metadata":{"exit_code":0}}"#
            .to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(!lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("call_123"))
    }));
    assert!(!lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("meta"))
    }));
}

#[test]
fn render_timeline_entry_lines_shows_code_intelligence_tool_card() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: serde_json::json!({
            "tool_name": "code_symbols",
            "status": "ok",
            "call_id": "call-code",
            "summary": "1 lines · 100 B",
            "preview_kind": "json",
            "preview_lines": [],
            "hidden_lines": 0,
            "metadata": {
                "returned_entries": 1,
                "total_entries": 1,
                "details": {
                    "call": { "summary": "path=src/lib.rs query=AppState" },
                    "code_intelligence": {
                        "server": "tree-sitter-rust",
                        "capability": "tree_sitter/document_symbols"
                    }
                }
            },
            "preview_value": {
                "tool": "code_symbols",
                "server": "tree-sitter-rust",
                "capability": "tree_sitter/document_symbols",
                "symbols": [{
                    "name": "AppState",
                    "kind": "struct",
                    "path": "src/lib.rs",
                    "range": {
                        "start_line": 3,
                        "start_character": 0,
                        "end_line": 3,
                        "end_character": 18
                    }
                }],
                "metadata": { "returned": 1, "total": 1, "truncated": false, "elapsed_ms": 1 }
            }
        })
        .to_string(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = rendered_plain_lines(&lines).join("\n");

    assert!(plain.contains("Inspected"));
    assert!(plain.contains("symbols"));
    assert!(plain.contains("Tree-sitter"));
    assert!(plain.contains("document symbols"));
    assert!(plain.contains("src/lib.rs:3"));
    assert!(plain.contains("AppState"));
}

#[test]
fn render_timeline_entry_lines_shows_lsp_definition_source_and_preview() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: serde_json::json!({
            "tool_name": "code_definition",
            "status": "ok",
            "call_id": "call-code-def",
            "summary": "1 lines · 120 B",
            "preview_kind": "json",
            "preview_lines": [],
            "hidden_lines": 0,
            "metadata": {
                "returned_entries": 1,
                "total_entries": 1,
                "details": {
                    "call": { "summary": "path=src/app.rs line=42 character=9" },
                    "code_intelligence": {
                        "server": "rust-analyzer",
                        "capability": "textDocument/definition"
                    }
                }
            },
            "preview_value": {
                "tool": "code_definition",
                "server": "rust-analyzer",
                "capability": "textDocument/definition",
                "definition": [{
                    "path": "src/service.rs",
                    "range": {
                        "start_line": 440,
                        "start_character": 12,
                        "end_line": 448,
                        "end_character": 1
                    },
                    "preview": "async fn lsp_document_symbols("
                }],
                "metadata": { "returned": 1, "total": 1, "truncated": false, "elapsed_ms": 18 }
            }
        })
        .to_string(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = rendered_plain_lines(&lines).join("\n");

    assert!(plain.contains("Located"));
    assert!(plain.contains("LSP"));
    assert!(plain.contains("rust-analyzer"));
    assert!(plain.contains("definition"));
    assert!(plain.contains("src/service.rs:440:12"));
    assert!(plain.contains("async fn lsp_document_symbols"));
}

#[test]
fn render_timeline_entry_lines_shows_lsp_diagnostics_with_server_breakdown() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: serde_json::json!({
            "tool_name": "code_diagnostics",
            "status": "ok",
            "call_id": "call-code-diagnostics",
            "summary": "2 lines · 180 B",
            "preview_kind": "json",
            "preview_lines": [],
            "hidden_lines": 0,
            "metadata": {
                "returned_entries": 2,
                "total_entries": 2,
                "details": {
                    "call": { "summary": "paths=diagnostics" },
                    "code_intelligence": {
                        "server": "multiple",
                        "capability": "textDocument/diagnostic"
                    }
                }
            },
            "preview_value": {
                "tool": "code_diagnostics",
                "server": "multiple",
                "capability": "textDocument/diagnostic",
                "diagnostics": [
                    {
                        "path": "src/lib.rs",
                        "range": {
                            "start_line": 9,
                            "start_character": 4,
                            "end_line": 9,
                            "end_character": 14
                        },
                        "severity": "error",
                        "message": "cannot find value `state` in this scope",
                        "source": "rustc"
                    },
                    {
                        "path": "web/app.ts",
                        "range": {
                            "start_line": 2,
                            "start_character": 0,
                            "end_line": 2,
                            "end_character": 5
                        },
                        "severity": "warning",
                        "message": "unused import",
                        "source": "tsserver"
                    }
                ],
                "servers": [
                    {
                        "server": "rust-analyzer",
                        "languages": ["rust"],
                        "status": "ready",
                        "returned": 1,
                        "total": 1,
                        "truncated": false
                    },
                    {
                        "server": "typescript-language-server",
                        "languages": ["typescript", "javascript"],
                        "status": "ready",
                        "returned": 1,
                        "total": 1,
                        "truncated": false
                    }
                ],
                "metadata": { "returned": 2, "total": 2, "truncated": false, "elapsed_ms": 32 }
            }
        })
        .to_string(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = rendered_plain_lines(&lines).join("\n");

    assert!(plain.contains("Checked"));
    assert!(plain.contains("LSP"));
    assert!(plain.contains("rust-analyzer ready (rust)"));
    assert!(plain.contains("typescript-language-server ready (typescript,javascript)"));
    assert!(plain.contains("src/lib.rs:9:4"));
    assert!(plain.contains("rustc: cannot find value"));
    assert!(plain.contains("web/app.ts:2"));
    assert!(plain.contains("tsserver: unused import"));
}

#[test]
fn render_timeline_entry_lines_styles_basic_markdown() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "## Title\n- **bold** and `code`".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Title"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("─"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("bold"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("code"))
    }));
}

#[test]
fn render_timeline_entry_lines_compacts_assistant_blank_lines() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "hello\n\n## Title\n\n- item".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("hello"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Title"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("─"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("item"))
    }));
}

#[test]
fn render_timeline_entry_lines_preserves_cjk_adjacency() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "你好！很高兴再次见到你。".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    let rendered = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(rendered.contains("你好！很高兴再次见到你。"));
    assert!(!rendered.contains("你 好"));
}

#[test]
fn render_timeline_entry_lines_show_phase_block() {
    let entry = TimelineEntry {
        role: TimelineRole::Phase,
        text: "thinking|deepseek-v4-flash".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("thinking"))
    );
    assert!(lines[1].spans.iter().any(|span| {
        span.content
            .as_ref()
            .contains("reasoning with deepseek-v4-flash")
    }));
}

#[test]
fn render_timeline_entry_lines_show_phase_tool_streaming_and_unknown_blocks() {
    let cases = [
        (
            TimelineEntry {
                role: TimelineRole::Phase,
                text: "tool|bash".to_owned(),
            },
            "tool",
            "running bash",
        ),
        (
            TimelineEntry {
                role: TimelineRole::Phase,
                text: "streaming".to_owned(),
            },
            "streaming",
            "writing the reply",
        ),
        (
            TimelineEntry {
                role: TimelineRole::Phase,
                text: "compacting|now".to_owned(),
            },
            "phase",
            "compacting|now",
        ),
    ];

    for (entry, label, detail) in cases {
        let plain = rendered_plain_lines(&render_timeline_entry_lines(&entry)).join("\n");
        assert!(plain.contains(label));
        assert!(plain.contains(detail));
    }
}

#[test]
fn render_timeline_entry_lines_show_phase_default_summaries_without_detail() {
    for (kind, expected) in [
        ("thinking", "reasoning"),
        ("tool", "running tool"),
        ("streaming", "writing the reply"),
    ] {
        let entry = TimelineEntry {
            role: TimelineRole::Phase,
            text: kind.to_owned(),
        };

        let plain = rendered_plain_lines(&render_timeline_entry_lines(&entry)).join("\n");

        assert!(plain.contains(kind));
        assert!(plain.contains(expected));
    }
}

#[test]
fn render_timeline_entry_lines_show_thinking_trace_block() {
    let entry = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "step 1\nstep 2\nstep 3\nstep 4".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    let default_palette = TimelineRenderOptions::default().theme.palette;
    assert_eq!(lines[0].spans[0].content.as_ref(), "●");
    assert_eq!(
        lines[0].spans[0].style.fg,
        Some(default_palette.status_thinking)
    );
    assert_eq!(lines[1].spans[0].content.as_ref(), "└ ");
    assert_eq!(lines[1].spans[0].style.fg, lines[0].spans[0].style.fg);
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("thought"))
    );
    assert!(
        !lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("thinking"))
    );
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    );
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 1"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 3"))
    }));
    assert!(!lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 4"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("1 more lines hidden"))
    }));

    let expanded = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_thinking_blocks: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    assert!(
        expanded[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    );
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 1"))
    }));
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 2"))
    }));
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 4"))
    }));

    let streaming = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            streaming_reasoning_index: Some(0),
            ..TimelineRenderOptions::default()
        },
        0,
    );
    assert!(
        streaming[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    );
    assert!(
        streaming[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("thinking"))
    );
    assert!(
        !streaming[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("thought"))
    );
    assert!(streaming.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 4"))
    }));

    let hovered = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            hovered_thinking_entry_index: Some(0),
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let header_span = hovered[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref().contains("thought"))
        .expect("expected hovered thinking header span");
    assert_eq!(
        hovered[0].spans[0].style.fg,
        Some(default_palette.accent_warning)
    );
    assert!(
        header_span
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn thinking_trace_marker_and_branch_use_configured_theme_palette() {
    let theme = theme::Theme::builtin(sigil_kernel::ThemeId::SolarizedLight);
    let palette = theme.palette.clone();
    let entry = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "step 1\nstep 2".to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            theme,
            ..TimelineRenderOptions::default()
        },
        0,
    );

    assert_eq!(lines[0].spans[0].content.as_ref(), "●");
    assert_eq!(lines[0].spans[0].style.fg, Some(palette.status_thinking));
    assert_eq!(lines[1].spans[0].content.as_ref(), "└ ");
    assert_eq!(lines[1].spans[0].style.fg, Some(palette.status_thinking));
    assert!(
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content.as_ref().contains("step 1")
                && span.style.fg == Some(palette.text_secondary))
    );
}

#[test]
fn thinking_preview_lines_handles_tilde_fences_and_closing_fences() {
    let preview = thinking_preview_lines("~~~text\nfirst\n~~~\nafter", 1);

    assert_eq!(
        preview,
        vec!["~~~text".to_owned(), "first".to_owned(), "~~~".to_owned()]
    );
    assert!(is_markdown_fence("   ~~~text"));
}

#[test]
fn render_timeline_entry_lines_handles_empty_and_closed_fence_thinking_previews() {
    let empty = TimelineEntry {
        role: TimelineRole::Thinking,
        text: " \n ".to_owned(),
    };
    let empty_plain = rendered_plain_lines(&render_timeline_entry_lines(&empty)).join("\n");
    assert!(empty_plain.contains("1 line"));
    assert!(!empty_plain.contains("Ctrl-T"));
    assert!(!empty_plain.contains("more lines hidden"));

    let short = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "only step\nsecond step\nthird step".to_owned(),
    };
    let short_plain = rendered_plain_lines(&render_timeline_entry_lines(&short)).join("\n");
    assert!(short_plain.contains("3 lines"));
    assert!(short_plain.contains("third step"));
    assert!(!short_plain.contains("Ctrl-T"));
    assert!(!short_plain.contains("more lines hidden"));
    let expanded_short_plain = rendered_plain_lines(&render_timeline_entry_lines_with_options(
        &short,
        &TimelineRenderOptions {
            expand_thinking_blocks: true,
            ..TimelineRenderOptions::default()
        },
        0,
    ))
    .join("\n");
    assert!(expanded_short_plain.contains("3 lines"));
    assert!(!expanded_short_plain.contains("Ctrl-T"));

    let closed_fence = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "```rust\nfn main() {}\n```\nnext hidden".to_owned(),
    };
    let closed_plain = rendered_plain_lines(&render_timeline_entry_lines(&closed_fence)).join("\n");
    assert!(closed_plain.contains("fn main"));
    assert!(closed_plain.contains("rust"));
    assert!(closed_plain.contains("more lines hidden"));
}

#[test]
fn render_timeline_content_spans_covers_all_roles() {
    let mut state = MarkdownRenderState::default();
    let options = MarkdownRenderOptions::timeline(80);

    for role in [
        TimelineRole::Assistant,
        TimelineRole::Thinking,
        TimelineRole::Tool,
        TimelineRole::System,
        TimelineRole::Phase,
        TimelineRole::Notice,
        TimelineRole::User,
    ] {
        let spans = render_timeline_content_spans(
            role,
            "**body**",
            ratatui::style::Style::default(),
            &mut state,
            options,
        );

        assert!(!spans.is_empty());
    }
}

#[test]
fn render_timeline_entry_lines_extends_collapsed_thinking_code_preview() {
    let entry = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "Runtime has 4 tests.\nActually, looking at my earlier output:\n```plain\nrunning 1 test\nok\n```\nThe rest is hidden."
            .to_owned(),
    };

    let plain = rendered_plain_lines(&render_timeline_entry_lines(&entry)).join("\n");

    assert!(plain.contains("code"));
    assert!(plain.contains("plain"));
    assert!(plain.contains("running 1 test"));
    assert!(plain.contains("ok"));
    assert!(plain.contains("more lines hidden"));
}

#[test]
fn render_timeline_entry_lines_make_user_and_assistant_distinct() {
    let user = TimelineEntry {
        role: TimelineRole::User,
        text: "hello".to_owned(),
    };
    let assistant = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "hello back".to_owned(),
    };

    let user_lines = render_timeline_entry_lines(&user);
    let assistant_lines = render_timeline_entry_lines(&assistant);

    assert!(
        user_lines[0]
            .spans
            .iter()
            .any(|span| span.style.bg == Some(user_message_bg()))
    );
    assert!(
        !assistant_lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.bg == Some(user_message_bg()))
    );
}

#[test]
fn render_timeline_entry_lines_highlights_user_slash_command_token() {
    let theme = theme::Theme::default();
    let user = TimelineEntry {
        role: TimelineRole::User,
        text: "/trust-workspace".to_owned(),
    };
    let lines = render_timeline_entry_lines_with_options(
        &user,
        &TimelineRenderOptions {
            max_content_width: 80,
            theme: theme.clone(),
            ..TimelineRenderOptions::default()
        },
        0,
    );

    let command = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "/trust-workspace")
        .expect("slash command token should render as its own span");
    assert_eq!(command.style.fg, Some(theme.palette.accent_info));
    assert_eq!(command.style.bg, Some(user_message_bg()));
    assert!(command.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn user_bubble_body_spans_highlight_slash_command_after_padding() {
    let theme = theme::Theme::default();
    let spans = user_bubble_body_spans("  /task fix typo", 80, user_message_bg(), &theme.palette);

    assert_eq!(spans[0].content.as_ref(), "  ");
    let command = spans
        .iter()
        .find(|span| span.content.as_ref() == "/task")
        .expect("slash command should be a standalone highlighted span");
    assert_eq!(command.style.fg, Some(theme.palette.accent_info));
    assert_eq!(command.style.bg, Some(user_message_bg()));
    assert!(command.style.add_modifier.contains(Modifier::BOLD));
    assert!(
        spans
            .iter()
            .any(|span| span.content.as_ref().contains("fix typo"))
    );
}

#[test]
fn render_timeline_entry_lines_make_headings_primary_and_flush_left() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "## 关键架构决策\n正文内容".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    let heading_plain = lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let body_plain = lines[2]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(heading_plain.starts_with("关键架构决策"));
    assert!(!heading_plain.starts_with("▏"));
    assert_eq!(body_plain.trim_start(), "正文内容");
}

#[test]
fn render_timeline_entry_lines_supports_task_lists_and_links() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "- [x] shipped [README](https://example.com)".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("[x]"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("README"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("example.com"))
    }));
}

#[test]
fn render_timeline_entry_lines_groups_paragraphs_and_code_blocks() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "first line\nsecond line\n\n```rust\nfn main() {}\n```".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("first line"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("second line"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("rust"))
    }));
    let plain_lines = rendered_plain_lines(&lines);
    assert!(plain_lines.iter().any(|line| line.contains("fn main() {}")));
}

#[test]
fn render_timeline_entry_lines_formats_markdown_tables_as_grid() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "| file | role |\n| --- | --- |\n| Cargo.toml | root |\n| src/lib.rs | core |"
            .to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("table"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("┌"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Cargo.toml"))
    }));
}

#[test]
fn render_timeline_entry_lines_wrap_markdown_tables_to_panel_width() {
    let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "| Phase | 内容 | 状态 |\n| --- | --- | --- |\n| **Phase 3** | planner/executor + compaction + memory + subagent + workspace confinement | 部分完成 |".to_owned(),
        };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            max_content_width: 48,
            ..TimelineRenderOptions::default()
        },
        0,
    );

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planner/executor"))
    }));
    assert!(lines.iter().all(|line| {
        let plain = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        UnicodeWidthStr::width(plain.as_str()) <= 50
    }));
    assert!(lines.iter().all(|line| {
        let plain = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        !plain.contains("**Phase 3**")
    }));
}

#[test]
fn render_timeline_entry_lines_formats_tool_cards() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-1",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 2/4 lines · 64 B",
  "preview_lines": ["[", "  \".git\",", "  \"Cargo.toml\"", "]"],
  "preview_value": [".git", "Cargo.toml"],
  "hidden_lines": 0,
  "metadata": {
    "bytes": 64,
    "details": {
      "call": {"summary": "path=crates"}
    }
  }
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );

    let first_line = rendered_plain_lines(&lines)[0].clone();

    assert!(first_line.contains("Listed crates"));
    assert!(
        !lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("tool"))
    );
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("OK"))
    );
    let summary_span = lines[1]
        .spans
        .iter()
        .find(|span| span.content.as_ref().contains("64 B"))
        .expect("expected execution summary span");
    assert_eq!(summary_span.style.fg, Some(theme::dim()));
    assert!(summary_span.style.add_modifier.contains(Modifier::ITALIC));
    assert!(!summary_span.style.add_modifier.contains(Modifier::BOLD));
    assert!(!lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("bytes=64"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("files"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Cargo.toml"))
    }));
}

#[test]
fn render_timeline_entry_lines_uses_action_first_tool_headers() {
    let cases = [
        (
            r#"{
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 3 B",
  "preview_lines": ["ok"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "command=cargo test -p sigil-tui"}}
  }
}"#,
            "Ran cargo test -p sigil-tui",
        ),
        (
            r#"{
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "2 lines · 118 B",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "command=grep -n 'needle' src/main.rs"}}
  }
}"#,
            "Searched needle in src/main.rs",
        ),
        (
            r#"{
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 24 B",
  "preview_lines": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "command=rg --glob '*.rs' needle src"}}
  }
}"#,
            "Searched needle in src",
        ),
        (
            r#"{
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 24 B",
  "preview_lines": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "command=grep needle src/main.rs | head"}}
  }
}"#,
            "Ran grep needle src/main.rs | head",
        ),
        (
            r##"{
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "markdown",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["# Title"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "path=README.md"}}
  }
}"##,
            "Read README.md",
        ),
        (
            r#"{
  "tool_name": "write_file",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 12 B",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "metadata": {
    "changed_files": ["note.txt"],
    "details": {"call": {"summary": "path=note.txt"}}
  }
}"#,
            "Wrote note.txt",
        ),
        (
            r#"{
  "tool_name": "edit_file",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 13 B",
  "preview_lines": ["edited note.txt"],
  "hidden_lines": 0,
  "metadata": {
    "changed_files": ["note.txt"],
    "details": {"call": {"summary": "path=note.txt"}}
  }
}"#,
            "Edited note.txt",
        ),
        (
            r#"{
  "tool_name": "delete_file",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 15 B",
  "preview_lines": ["deleted note.txt"],
  "hidden_lines": 0,
  "metadata": {
    "changed_files": ["note.txt"],
    "details": {"call": {"summary": "path=note.txt"}}
  }
}"#,
            "Deleted note.txt",
        ),
        (
            r#"{
  "tool_name": "grep",
  "status": "ok",
  "preview_kind": "json",
  "summary": "1 line · 2 B",
  "preview_lines": ["[]"],
  "preview_value": [],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "path=src pattern=needle"}}
  }
}"#,
            "Searched needle in src",
        ),
        (
            r#"{
  "tool_name": "glob",
  "status": "ok",
  "preview_kind": "json",
  "summary": "1 line · 2 B",
  "preview_lines": ["[]"],
  "preview_value": [],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "pattern=**/*.rs"}}
  }
}"#,
            "Searched **/*.rs",
        ),
        (
            r#"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "1 line · 2 B",
  "preview_lines": ["[]"],
  "preview_value": [],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "path=crates/sigil-tui"}}
  }
}"#,
            "Listed crates/sigil-tui",
        ),
        (
            r#"{
  "tool_name": "mcp__filesystem__stat",
  "status": "ok",
  "preview_kind": "json",
  "summary": "1 line · 15 B",
  "preview_lines": ["{}"],
  "preview_value": {"ok": true},
  "hidden_lines": 0,
  "metadata": {
    "details": {
      "call": {
        "summary": "path=README.md id=call_123",
        "subjects": [
          "unknown:mcp_tool:mcp__filesystem__stat",
          "unknown:mcp_trust_class:mcp_trust_class:third_party"
        ]
      }
    }
  }
}"#,
            "Called stat on filesystem",
        ),
    ];

    for (payload, expected_title) in cases {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: payload.to_owned(),
        };
        let lines = render_timeline_entry_lines(&entry);
        let first_line = rendered_plain_lines(&lines)
            .into_iter()
            .next()
            .expect("expected header line");

        assert!(
            first_line.contains(expected_title),
            "expected {first_line:?} to contain {expected_title:?}"
        );
        if expected_title == "Called stat on filesystem" {
            assert!(
                first_line.contains("trust third_party"),
                "MCP tool header should include trust class: {first_line}"
            );
        }
        assert!(
            !first_line.contains("path=") || expected_title.contains("Called"),
            "builtin action titles should not expose raw key-value call summaries: {first_line}"
        );
        assert!(
            !first_line.contains("call_123"),
            "tool call ids should stay hidden: {first_line}"
        );
    }
}

#[test]
fn render_timeline_entry_lines_styles_tool_header_segments() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "1 line · 3 B",
  "preview_lines": ["ok"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "command=cargo test --workspace"}}
  }
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    let action = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "Ran")
        .expect("expected action span");
    let subject = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "cargo")
        .expect("expected command span");
    let args = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref().contains("--workspace"))
        .expect("expected args span");

    assert_eq!(action.style.fg, Some(accent_gold()));
    assert!(action.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(subject.style.fg, Some(accent_blue()));
    assert!(subject.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(args.style.fg, Some(ink()));
    assert!(!args.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn render_timeline_entry_lines_simplifies_bash_no_output() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "0 lines · 0 B",
  "preview_lines": [],
  "hidden_lines": 0,
  "metadata": {
    "exit_code": 0,
    "stdout_bytes": 0,
    "stderr_bytes": 0,
    "details": {"call": {"summary": "command=cargo fmt --all --check"}}
  }
}"#
        .to_owned(),
    };

    let plain = rendered_plain_lines(&render_timeline_entry_lines(&entry)).join("\n");

    assert!(plain.contains("Ran cargo fmt --all --check"));
    assert!(plain.contains("OK"));
    assert!(plain.contains("exit 0"));
    assert!(plain.contains("(no output)"));
    assert!(!plain.contains("terminal tail"));
}

#[test]
fn render_timeline_entry_lines_prioritizes_bash_failure_output() {
    let stderr_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "bash",
  "status": "error",
  "error_kind": "exit_status",
  "preview_kind": "text",
  "summary": "last 1/1 lines · 21 B",
  "preview_lines": ["error: clippy failed"],
  "hidden_lines": 0,
  "metadata": {
    "exit_code": 101,
    "stdout_bytes": 0,
    "stderr_bytes": 21,
    "details": {"call": {"summary": "command=cargo clippy"}}
  }
}"#
        .to_owned(),
    };
    let stdout_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "bash",
  "status": "error",
  "error_kind": "exit_status",
  "preview_kind": "text",
  "summary": "last 1/1 lines · 16 B",
  "preview_lines": ["failed on stdout"],
  "hidden_lines": 0,
  "metadata": {
    "exit_code": 1,
    "stdout_bytes": 16,
    "stderr_bytes": 0,
    "details": {"call": {"summary": "command=./script"}}
  }
}"#
        .to_owned(),
    };

    let stderr_plain = rendered_plain_lines(&render_timeline_entry_lines_with_options(
        &stderr_entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    ))
    .join("\n");
    let stdout_plain = rendered_plain_lines(&render_timeline_entry_lines_with_options(
        &stdout_entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    ))
    .join("\n");

    assert!(stderr_plain.contains("ERROR"));
    assert!(stderr_plain.contains("exit 101"));
    assert!(stderr_plain.contains("stderr"));
    assert!(stderr_plain.contains("exit 101"));
    assert!(stderr_plain.contains("error: clippy failed"));
    assert!(stdout_plain.contains("ERROR"));
    assert!(stdout_plain.contains("exit 1"));
    assert!(stdout_plain.contains("stdout"));
    assert!(stdout_plain.contains("exit 1"));
    assert!(stdout_plain.contains("failed on stdout"));
}

#[test]
fn render_timeline_entry_lines_labels_denied_and_interrupted_errors() {
    let denied_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "write_file",
  "status": "error",
  "error_kind": "approval_denied",
  "preview_kind": "text",
  "summary": "1 line · 37 B",
  "preview_lines": ["tool execution denied by user"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "path=note.txt"}}
  }
}"#
        .to_owned(),
    };
    let interrupted_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "bash",
  "status": "error",
  "error_kind": "interrupted",
  "preview_kind": "text",
  "summary": "1 line · 28 B",
  "preview_lines": ["tool execution interrupted"],
  "hidden_lines": 0,
  "metadata": {
    "details": {"call": {"summary": "command=cargo test"}}
  }
}"#
        .to_owned(),
    };

    let denied_plain = rendered_plain_lines(&render_timeline_entry_lines(&denied_entry)).join("\n");
    let interrupted_plain =
        rendered_plain_lines(&render_timeline_entry_lines(&interrupted_entry)).join("\n");

    assert!(denied_plain.contains("Wrote note.txt"));
    assert!(denied_plain.contains("DENIED"));
    assert!(!denied_plain.contains("path=note.txt"));
    assert!(interrupted_plain.contains("Ran cargo test"));
    assert!(interrupted_plain.contains("INTERRUPTED"));
}

#[test]
fn render_timeline_entry_lines_formats_grep_cards_by_file() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "grep",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 2/2 lines · 91 B",
  "preview_lines": ["[]"],
  "preview_value": [
    {"path": "src/lib.rs", "line": 12, "text": "fn helper()"},
    {"path": "src/lib.rs", "line": 29, "text": "helper();"}
  ],
  "hidden_lines": 0
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("matches"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("src/lib.rs"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("L12"))
    }));
}

#[test]
fn render_timeline_entry_lines_renders_generic_json_tree_preview() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "custom_tool",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 3/3 lines · 44 B",
  "preview_lines": ["{}"],
  "preview_value": {"root": {"leaf": "value"}},
  "hidden_lines": 0
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("tree"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("root"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("leaf"))
    }));
}

#[test]
fn render_timeline_entry_lines_show_short_tool_preview_by_default() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r##"{
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "markdown",
  "summary": "first 2/2 lines · 18 B",
  "preview_lines": ["# Title", "- item"],
  "hidden_lines": 0
}"##
        .to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Title"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("item"))
    }));
    assert!(
        !rendered_plain_lines(&lines)
            .join("\n")
            .contains("preview hidden")
    );
}

#[test]
fn render_timeline_entry_lines_expands_tool_diff_by_default() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-diff",
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 14 B · diff +1 -1 · 1 file",
  "metadata": {"changed_files": ["note.txt"]},
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "diff": {
    "summary": "+1 -1 · 1 file",
    "truncated": false,
    "original_line_count": 6,
    "rendered_line_count": 6,
    "files": [{
      "path": "note.txt",
      "lines": ["--- current/note.txt", "+++ proposed/note.txt", "@@ -1 +1 @@", "-old", "+new"],
      "truncated": false,
      "original_line_count": 5,
      "rendered_line_count": 5
    }]
  }
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    let visible_lines = rendered_plain_lines(&lines);
    let plain = visible_lines.join("\n");

    assert!(plain.contains("diff +1 -1"));
    assert!(plain.contains("--- current/note.txt"));
    assert!(plain.contains("1 hunk"));
    assert!(!plain.contains("@@ -1 +1 @@"));
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│ 1   │ -old"))
    );
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│    1│ +new"))
    );
    assert!(plain.contains("-old"));
    assert!(plain.contains("+new"));
    assert!(!plain.contains("diff hidden"));
}

#[test]
fn render_timeline_entry_lines_renders_delete_file_diff_as_file_change() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "delete_file",
  "status": "ok",
  "summary": "1 line · 16 B · diff +0 -2 · 1 file",
  "metadata": {
    "changed_files": ["note.txt"],
    "details": {
      "action": "delete",
      "call": {"summary": "path=note.txt"}
    }
  },
  "preview_kind": "text",
  "preview_lines": ["deleted /workspace/note.txt"],
  "hidden_lines": 0,
  "diff": {
    "summary": "+0 -2 · 1 file",
    "truncated": false,
    "original_line_count": 5,
    "rendered_line_count": 5,
    "files": [{
      "path": "note.txt",
      "lines": ["--- current/note.txt", "+++ proposed/note.txt", "@@ -1,2 +0,0 @@", "-alpha", "-beta"],
      "truncated": false,
      "original_line_count": 5,
      "rendered_line_count": 5
    }]
  }
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);
    let visible_lines = rendered_plain_lines(&lines);
    let plain = visible_lines.join("\n");

    assert!(plain.contains("Deleted note.txt"));
    assert!(!plain.contains("delete_file"));
    assert!(!plain.contains("path=note.txt"));
    assert!(plain.contains("1 deleted"));
    assert!(plain.contains("deleted"));
    assert!(plain.contains("--- current/note.txt"));
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│ 1   │ -alpha"))
    );
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│ 2   │ -beta"))
    );
    assert!(plain.contains("-alpha"));
    assert!(plain.contains("-beta"));
    assert!(plain.contains("result"));
    assert!(plain.contains("delete summary"));
    assert!(!plain.contains("tree"));
}

#[test]
fn render_timeline_entry_lines_summarizes_tool_diff_hunks_in_file_header() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 18 B · diff +2 -2 · 1 file",
  "metadata": {"changed_files": ["note.txt"]},
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "diff": {
    "summary": "+2 -2 · 1 file",
    "truncated": false,
    "original_line_count": 9,
    "rendered_line_count": 9,
    "files": [{
      "path": "note.txt",
      "lines": ["--- current/note.txt", "+++ proposed/note.txt", "@@ -1 +1 @@", "-old one", "+new one", "@@ -20 +20 @@", "-old two", "+new two"],
      "truncated": false,
      "original_line_count": 8,
      "rendered_line_count": 8
    }]
  }
}"#
        .to_owned(),
    };

    let visible_lines = rendered_plain_lines(&render_timeline_entry_lines(&entry));
    let plain = visible_lines.join("\n");

    assert!(plain.contains("2 hunks"));
    assert!(!plain.contains("@@ -1 +1 @@"));
    assert!(!plain.contains("@@ -20 +20 @@"));
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│ 1   │ -old one"))
    );
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│    1│ +new one"))
    );
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│20   │ -old two"))
    );
    assert!(
        visible_lines
            .iter()
            .any(|line| line.contains("│   20│ +new two"))
    );
}

#[test]
fn render_timeline_entry_lines_can_collapse_default_expanded_tool_diff() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-diff",
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 14 B · diff +1 -1 · 1 file",
  "metadata": {"changed_files": ["note.txt"]},
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "diff": {
    "summary": "+1 -1 · 1 file",
    "truncated": false,
    "original_line_count": 6,
    "rendered_line_count": 6,
    "files": [{
      "path": "note.txt",
      "lines": ["--- current/note.txt", "+++ proposed/note.txt", "@@ -1 +1 @@", "-old", "+new"],
      "truncated": false,
      "original_line_count": 5,
      "rendered_line_count": 5
    }]
  }
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            collapsed_tool_activity_keys: BTreeSet::from(["call:call-diff".to_owned()]),
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(plain.contains("note.txt"));
    assert!(plain.contains("diff"));
    assert!(plain.contains("more lines hidden"));
    assert!(!plain.contains("--- current/note.txt"));
}

#[test]
fn render_timeline_entry_lines_renders_expanded_tool_diff() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 14 B · diff +2 -1 · 1 file · truncated",
  "metadata": {"changed_files": ["note.txt"]},
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "diff": {
    "summary": "+2 -1 · 1 file · truncated",
    "truncated": true,
    "original_line_count": 8,
    "rendered_line_count": 5,
    "files": [{
      "path": "note.txt",
      "lines": ["--- current/note.txt", "+++ proposed/note.txt", "@@ -1 +1 @@", "-old", "+new"],
      "truncated": true,
      "original_line_count": 8,
      "rendered_line_count": 5
    }]
  }
}"#
        .to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = lines
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(plain.contains("files"));
    assert!(plain.contains("diff"));
    assert!(plain.contains("note.txt"));
    assert!(plain.contains("--- current/note.txt"));
    assert!(plain.contains("-old"));
    assert!(plain.contains("+new"));
    assert!(plain.contains("diff truncated"));
    assert!(plain.contains("3 lines hidden"));
    assert!(plain.contains("result"));
}

#[test]
fn tool_activity_view_marks_read_list_and_simple_searches_as_inspection() {
    let read_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#
        .to_owned(),
    };
    let bash_search_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-search",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "command=grep -n needle src/main.rs"}}}
}"#
        .to_owned(),
    };
    let complex_bash_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-complex",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "command=grep needle src/main.rs | head"}}}
}"#
        .to_owned(),
    };

    let read_activity =
        crate::ui::tool_activity_view(&read_entry, 0).expect("read activity should parse");
    let search_activity =
        crate::ui::tool_activity_view(&bash_search_entry, 1).expect("search activity should parse");
    let complex_activity = crate::ui::tool_activity_view(&complex_bash_entry, 2)
        .expect("complex activity should parse");

    assert_eq!(read_activity.key, "call:call-read");
    assert_eq!(read_activity.title, "Read README.md");
    assert!(read_activity.is_inspection);
    assert_eq!(search_activity.title, "Searched needle in src/main.rs");
    assert!(search_activity.is_inspection);
    assert_eq!(complex_activity.title, "Ran grep needle src/main.rs | head");
    assert!(!complex_activity.is_inspection);
}

#[test]
fn render_timeline_entry_lines_show_tool_call_context_when_collapsed() {
    let bash_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
  "call_id": "call-1",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "summary": "last 1/1 lines · 12 B",
  "preview_lines": ["ok"],
  "hidden_lines": 0,
  "metadata": {
    "exit_code": 0,
    "details": {
      "call": {
        "summary": "command=cargo test -p sigil-tui"
      }
    }
  }
}"#
        .to_owned(),
    };
    let read_entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r##"{
  "call_id": "call-2",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "markdown",
  "summary": "first 2/12 lines · 2.4 KB",
  "preview_lines": ["# Title", "body"],
  "hidden_lines": 10,
  "metadata": {
    "details": {
      "call": {
        "summary": "path=crates/sigil-tui/src/runner/worker_loop.rs"
      }
    }
  }
}"##
        .to_owned(),
    };

    let options = TimelineRenderOptions {
        max_content_width: 120,
        ..TimelineRenderOptions::default()
    };
    let bash_lines = render_timeline_entry_lines_with_options(&bash_entry, &options, 0);
    let read_lines = render_timeline_entry_lines_with_options(&read_entry, &options, 0);

    let bash_header = rendered_plain_lines(&bash_lines)[0].clone();
    let read_header = rendered_plain_lines(&read_lines)[0].clone();

    assert!(bash_header.contains("Ran cargo test -p sigil-tui"));
    assert!(!bash_lines[0].spans.iter().any(|span| {
        span.content
            .as_ref()
            .contains("command=cargo test -p sigil-tui")
    }));
    assert!(!bash_lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("call-1"))
    }));
    assert!(read_header.contains("Read crates/sigil-tui/src/runner/worker_loop.rs"));
    assert!(
        !read_lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref() == "md")
    );
}

#[test]
fn render_timeline_entry_lines_wrap_user_bubbles_on_narrow_widths() {
    let entry = TimelineEntry {
        role: TimelineRole::User,
        text: "this is a longer user prompt that should wrap".to_owned(),
    };

    let lines = render_timeline_entry_lines_with_options(
        &entry,
        &TimelineRenderOptions {
            max_content_width: 20,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let rows = rendered_plain_lines(&lines);

    assert!(rows.len() > 3);
    assert!(rows[0].starts_with("▌  "));
    assert!(rows.iter().any(|row| row.contains("longer")));
}

#[test]
fn render_timeline_entry_lines_label_notice_tones() {
    let ok_notice = TimelineEntry {
        role: TimelineRole::Notice,
        text: "saved: config updated".to_owned(),
    };
    let error_notice = TimelineEntry {
        role: TimelineRole::Notice,
        text: "error: missing token".to_owned(),
    };

    let ok_plain = rendered_plain_lines(&render_timeline_entry_lines(&ok_notice)).join("\n");
    let error_plain = rendered_plain_lines(&render_timeline_entry_lines(&error_notice)).join("\n");

    assert!(ok_plain.contains("done"));
    assert!(ok_plain.contains("saved: config updated"));
    assert!(error_plain.contains("error"));
    assert!(error_plain.contains("missing token"));
}

#[test]
fn render_timeline_entry_lines_uses_doctor_status_for_notice_tone() {
    let ok_doctor = TimelineEntry {
        role: TimelineRole::Notice,
        text: "doctor: ok\nsummary: 0 error · 0 warn · 8 ok".to_owned(),
    };
    let warn_doctor = TimelineEntry {
        role: TimelineRole::Notice,
        text: "doctor: warn\nsummary: 0 error · 1 warn · 7 ok".to_owned(),
    };
    let error_doctor = TimelineEntry {
        role: TimelineRole::Notice,
        text: "doctor: error\nsummary: 1 error · 0 warn · 7 ok".to_owned(),
    };

    let ok_plain = rendered_plain_lines(&render_timeline_entry_lines(&ok_doctor)).join("\n");
    let warn_plain = rendered_plain_lines(&render_timeline_entry_lines(&warn_doctor)).join("\n");
    let error_plain = rendered_plain_lines(&render_timeline_entry_lines(&error_doctor)).join("\n");

    assert!(ok_plain.contains("done"));
    assert!(!ok_plain.contains("error\n"));
    assert!(warn_plain.contains("notice"));
    assert!(!warn_plain.contains("error\n"));
    assert!(error_plain.contains("error"));
}

#[test]
fn render_timeline_entry_lines_keeps_notice_visible_without_info_subtitle() {
    let notice = TimelineEntry {
        role: TimelineRole::Notice,
        text: "info: **checking**\n\nplain".to_owned(),
    };
    let notice_lines = render_timeline_entry_lines(&notice);
    let notice_rows = rendered_plain_lines(&notice_lines);
    let notice_plain = notice_rows.join("\n");

    assert!(notice_rows.iter().any(|row| row.trim() == "notice"));
    assert!(notice_plain.contains("checking"));
    assert!(notice_plain.contains("plain"));
    assert!(!notice_plain.contains("notice info"));
    assert_eq!(notice_rows.len(), 4);
    assert_eq!(notice_lines[0].spans[0].content.as_ref(), "notice");
    assert_eq!(
        notice_lines[0].spans[0].style,
        Style::default().fg(accent_gold())
    );

    let mut markdown_state = MarkdownRenderState::default();
    let options = MarkdownRenderOptions::timeline(40);
    let assistant = render_timeline_content_spans(
        TimelineRole::Assistant,
        "**bold**",
        Style::default(),
        &mut markdown_state,
        options,
    );
    assert!(assistant.iter().any(|span| span.content.as_ref() == "bold"));

    let mut markdown_state = MarkdownRenderState::default();
    let thinking = render_timeline_content_spans(
        TimelineRole::Thinking,
        "`code`",
        Style::default(),
        &mut markdown_state,
        options,
    );
    assert!(thinking.iter().any(|span| span.content.as_ref() == "code"));

    let mut markdown_state = MarkdownRenderState::default();
    let tool = render_timeline_content_spans(
        TimelineRole::Tool,
        "{\"ok\":true}",
        Style::default(),
        &mut markdown_state,
        options,
    );
    assert!(tool.iter().any(|span| span.content.as_ref().contains("ok")));

    let mut markdown_state = MarkdownRenderState::default();
    let user = render_timeline_content_spans(
        TimelineRole::User,
        "plain user",
        Style::default(),
        &mut markdown_state,
        options,
    );
    assert_eq!(
        user.first().map(|span| span.content.as_ref()),
        Some("plain user")
    );
}

#[test]
fn timeline_render_options_theme_reaches_visible_content_spans() {
    let theme = theme::Theme::builtin(sigil_kernel::ThemeId::SolarizedLight);
    let palette = theme.palette.clone();
    let options = TimelineRenderOptions {
        theme,
        max_content_width: 48,
        intermediate_assistant_indices: std::collections::BTreeSet::from([0]),
        ..TimelineRenderOptions::default()
    };

    let user = TimelineEntry {
        role: TimelineRole::User,
        text: "`code`".to_owned(),
    };
    let user_lines = render_timeline_entry_lines_with_options(&user, &options, 1);
    let user_code = user_lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "code")
        .expect("user inline code should render");
    assert_eq!(user_code.style.fg, Some(palette.markdown_code_fg));
    assert_eq!(user_code.style.bg, Some(palette.surface_user_message));

    let assistant = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "ready".to_owned(),
    };
    let assistant_lines = render_timeline_entry_lines_with_options(&assistant, &options, 0);
    let marker = assistant_lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "• ")
        .expect("intermediate assistant marker should render");
    assert_eq!(marker.style.fg, Some(palette.text_muted));

    let system = TimelineEntry {
        role: TimelineRole::System,
        text: "`sys`".to_owned(),
    };
    let system_lines = render_timeline_entry_lines_with_options(&system, &options, 1);
    let system_code = system_lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "sys")
        .expect("system inline code should render");
    assert_eq!(system_code.style.fg, Some(palette.markdown_code_fg));
    assert_eq!(system_code.style.bg, Some(palette.markdown_code_bg));

    let notice = TimelineEntry {
        role: TimelineRole::Notice,
        text: "info: `notice`".to_owned(),
    };
    let notice_lines = render_timeline_entry_lines_with_options(&notice, &options, 1);
    let notice_code = notice_lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| {
            span.content.as_ref() == "notice" && span.style.bg == Some(palette.markdown_code_bg)
        })
        .expect("notice inline code should render");
    assert_eq!(notice_code.style.fg, Some(palette.markdown_code_fg));
    assert_eq!(notice_code.style.bg, Some(palette.markdown_code_bg));
}
