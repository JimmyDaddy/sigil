use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

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
    assert!(plain.contains("src/lib.rs:3"));
    assert!(plain.contains("AppState"));
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
fn render_timeline_entry_lines_show_thinking_trace_block() {
    let entry = TimelineEntry {
        role: TimelineRole::Thinking,
        text: "step 1\nstep 2\nstep 3\nstep 4".to_owned(),
    };

    let lines = render_timeline_entry_lines(&entry);

    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("thought"))
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
    assert_eq!(summary_span.style.fg, Some(dim()));
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
    "details": {"call": {"summary": "command=cargo test -p termquill-tui"}}
  }
}"#,
            "Ran cargo test -p termquill-tui",
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
    "details": {"call": {"summary": "path=crates/termquill-tui"}}
  }
}"#,
            "Listed crates/termquill-tui",
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
    "details": {"call": {"summary": "path=README.md id=call_123"}}
  }
}"#,
            "Called mcp__filesystem__stat path=README.md",
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
fn render_timeline_entry_lines_hide_tool_preview_by_default() {
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
            .any(|span| span.content.as_ref().contains("preview hidden"))
    }));
    assert!(!lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("# Title"))
    }));
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

    assert!(plain.contains("diff hidden"));
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
        "summary": "command=cargo test -p termquill-tui"
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
        "summary": "path=crates/termquill-tui/src/runner/worker_loop.rs"
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

    assert!(bash_header.contains("Ran cargo test -p termquill-tui"));
    assert!(!bash_lines[0].spans.iter().any(|span| {
        span.content
            .as_ref()
            .contains("command=cargo test -p termquill-tui")
    }));
    assert!(!bash_lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("call-1"))
    }));
    assert!(read_header.contains("Read crates/termquill-tui/src/runner/worker_loop.rs"));
    assert!(
        !read_lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref() == "md")
    );
}
