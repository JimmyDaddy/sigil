use unicode_width::UnicodeWidthStr;

use super::*;

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

    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("call_123"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("meta"))
    }));
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
        text: "step 1\nstep 2".to_owned(),
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
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("step 1"))
    );

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
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("fn main() {}"))
    }));
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
  "metadata_line": "bytes=64",
  "metadata": {"bytes": 64}
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

    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("ls"))
    );
    assert!(
        lines[1]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("OK"))
    );
    assert!(
        lines[2]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("bytes=64"))
    );
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
