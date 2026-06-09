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
        lines[1]
            .spans
            .iter()
            .any(|span| span.content.as_ref().contains("64 B"))
    );
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

    assert!(plain.contains("delete_file"));
    assert!(plain.contains("path=note.txt"));
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
fn render_timeline_entry_lines_can_collapse_default_expanded_tool_diff() {
    let entry = TimelineEntry {
        role: TimelineRole::Tool,
        text: r#"{
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
            collapsed_tool_entries: BTreeSet::from([0]),
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
    assert!(plain.contains("result"));
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

    assert!(bash_lines[0].spans.iter().any(|span| {
        span.content
            .as_ref()
            .contains("command=cargo test -p termquill-tui")
    }));
    assert!(!bash_lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("call-1"))
    }));
    assert!(read_lines[0].spans.iter().any(|span| {
        span.content
            .as_ref()
            .contains("path=crates/termquill-tui/src/runner/worker_loop.rs")
    }));
    assert!(
        !read_lines[0]
            .spans
            .iter()
            .any(|span| span.content.as_ref() == "md")
    );
}
