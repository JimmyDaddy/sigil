use super::super::{
    build_model_picker_options, char_to_byte_index, format_terminal_task_block_redacted,
    format_token_compact, format_token_count, format_tool_content_block_redacted_for_restore,
    format_tool_result_block_redacted, hash_timeline_line, human_file_size,
    line_has_visible_content, non_empty_or, normalize_command_prefix_character,
    normalize_runtime_model, parse_reasoning_effort, persisted_root_config, plain_line_text,
    ratio_to_percent, sidebar_width_for_terminal, summarize_error,
};
use super::*;
use ratatui::text::{Line, Span};
use sigil_kernel::{SecretRedactor, ToolDiffBudget};

#[test]
fn summarize_error_prefers_last_non_empty_cause_line() {
    assert_eq!(
        summarize_error(
            "request failed\n\nCaused by:\n  0: transport error\n  1: upstream timed out"
        ),
        "upstream timed out"
    );
    assert_eq!(
        summarize_error("  single line failure  "),
        "single line failure"
    );
}

#[test]
fn formatting_helpers_cover_normalization_and_layout() {
    assert_eq!(human_file_size(999), "999 B");
    assert_eq!(human_file_size(1_536), "1.5 KB");
    assert_eq!(human_file_size(2 * 1024 * 1024), "2.0 MB");

    assert_eq!(parse_reasoning_effort("LOW"), Some(ReasoningEffort::Low));
    assert_eq!(parse_reasoning_effort("med"), Some(ReasoningEffort::Medium));
    assert_eq!(parse_reasoning_effort("unknown"), None);

    assert_eq!(sidebar_width_for_terminal(95), 0);
    assert_eq!(sidebar_width_for_terminal(120), 24);
    assert_eq!(sidebar_width_for_terminal(220), 42);

    assert_eq!(
        normalize_runtime_model(" flash "),
        Some("deepseek-v4-flash".to_owned())
    );
    assert_eq!(
        normalize_runtime_model("v4-pro"),
        Some("deepseek-v4-pro".to_owned())
    );
    assert_eq!(normalize_runtime_model(" "), None);

    assert_eq!(normalize_command_prefix_character('/'), Some('/'));
    assert_eq!(normalize_command_prefix_character('、'), Some('/'));
    assert_eq!(normalize_command_prefix_character('x'), None);

    assert_eq!(format_token_count(1_234_567), "1,234,567");
    assert_eq!(format_token_compact(987), "987");
    assert_eq!(format_token_compact(1_234), "1.2K");
    assert_eq!(format_token_compact(2_000_000), "2.0M");

    let blank = Line::from(vec![Span::raw("  "), Span::raw("▌")]);
    let visible = Line::from(vec![Span::raw("  "), Span::raw("value")]);
    assert!(!line_has_visible_content(&blank));
    assert!(line_has_visible_content(&visible));
    assert_eq!(plain_line_text(&visible), "  value");
    assert_ne!(
        hash_timeline_line(7, "alpha"),
        hash_timeline_line(7, "beta")
    );
    assert_eq!(ratio_to_percent(-1.0), 0);
    assert_eq!(ratio_to_percent(1.234), 123);

    assert_eq!(
        build_model_picker_options(" custom-model ", Vec::new()).last(),
        Some(&"custom-model".to_owned())
    );
    assert_eq!(non_empty_or("  value  ", "fallback"), "value");
    assert_eq!(non_empty_or("   ", "fallback"), "fallback");
    assert_eq!(char_to_byte_index("a中b", 2), "a中".len());
    assert_eq!(
        persisted_root_config(&test_config()).agent.model,
        "deepseek-v4-flash"
    );
}

#[test]
fn format_tool_result_block_redacted_includes_json_preview_diff_and_metadata() -> Result<()> {
    let preview = sample_approval_preview();
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-write-1",
        "write_file",
        &preview,
        ToolDiffBudget::default(),
        Some("preview-hash".to_owned()),
    );
    let result = ToolResult::ok(
        "call-write-1",
        "write_file",
        json!({
            "secret": "supersecret-token",
            "items": [1, 2, 3],
            "nested": { "api_key": "supersecret-token" }
        })
        .to_string(),
        ToolResultMeta {
            bytes: Some(2_048),
            changed_files: vec!["note.txt".to_owned()],
            details: json!({
                "api_key": "supersecret-token",
                "call": { "summary": "path=note.txt" }
            }),
            ..ToolResultMeta::default()
        },
    );
    let redactor = SecretRedactor::from_values(["supersecret-token"]);

    let payload: serde_json::Value = serde_json::from_str(&format_tool_result_block_redacted(
        &result,
        Some(&snapshot),
        &redactor,
    ))?;

    assert_eq!(payload["tool_name"], "write_file");
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["preview_kind"], "json");
    assert_eq!(payload["metadata"]["details"]["api_key"], "[redacted]");
    assert_eq!(payload["preview_value"]["secret"], "[redacted]");
    assert!(
        payload["summary"]
            .as_str()
            .is_some_and(|summary| { summary.contains("2.0 KB") && summary.contains("diff") })
    );
    assert_eq!(payload["diff"]["files"][0]["path"], "note.txt");
    Ok(())
}

#[test]
fn formatted_tool_restore_handles_large_error_payloads() -> Result<()> {
    let huge = "中".repeat(70_000);
    let envelope = ToolResult::error("call-bash-1", "bash", ToolErrorKind::Interrupted, huge)
        .to_model_content();
    let execution = ToolExecutionEntry {
        call_id: "call-bash-1".to_owned(),
        tool_name: "bash".to_owned(),
        status: ToolExecutionStatus::Interrupted,
        duration_ms: Some(9),
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta {
            details: json!({
                "call": { "summary": "command=printf boom" }
            }),
            ..ToolResultMeta::default()
        },
        error: Some(ToolError {
            kind: ToolErrorKind::Interrupted,
            message: "tool execution was interrupted before completion".to_owned(),
            retryable: true,
            details: serde_json::Value::Null,
        }),
        model_content_hash: None,
    };

    let payload: serde_json::Value =
        serde_json::from_str(&format_tool_content_block_redacted_for_restore(
            Some("call-bash-1"),
            &envelope,
            Some(&execution),
            None,
            &SecretRedactor::empty(),
        ))?;

    assert_eq!(payload["tool_name"], "bash");
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error_kind"], "interrupted");
    assert_eq!(payload["display_truncated"], true);
    assert!(
        payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("display truncated"))
    );
    Ok(())
}

#[test]
fn read_file_results_use_markdown_preview_kind() -> Result<()> {
    let payload: serde_json::Value = serde_json::from_str(&format_tool_result_block_redacted(
        &ToolResult::ok(
            "call-read-1",
            "read_file",
            "# Title\n\n- item 1\n- item 2".to_owned(),
            ToolResultMeta::default(),
        ),
        None,
        &SecretRedactor::empty(),
    ))?;

    assert_eq!(payload["preview_kind"], "markdown");
    assert_eq!(payload["hidden_lines"], 0);
    assert!(
        payload["preview_lines"][0]
            .as_str()
            .is_some_and(|line| line.starts_with("# Title"))
    );
    Ok(())
}

#[test]
fn format_terminal_task_block_redacted_summarizes_failed_and_exited_statuses() -> Result<()> {
    let failed: serde_json::Value = serde_json::from_str(&format_terminal_task_block_redacted(
        &format_terminal_entry(
            "terminal-failed",
            sigil_kernel::TerminalTaskStatus::Failed {
                reason: "command failed after waiting for child process".to_owned(),
            },
        )?,
        &SecretRedactor::empty(),
    ))?;
    let exited: serde_json::Value = serde_json::from_str(&format_terminal_task_block_redacted(
        &format_terminal_entry(
            "terminal-exited",
            sigil_kernel::TerminalTaskStatus::Exited { exit_code: Some(7) },
        )?,
        &SecretRedactor::empty(),
    ))?;

    assert_eq!(failed["status"], "error");
    assert_eq!(
        failed["metadata"]["details"]["terminal_task"]["status"],
        "failed"
    );
    assert!(
        failed["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("failed command failed"))
    );
    assert_eq!(
        failed["metadata"]["details"]["terminal_task"]["status_detail"]["reason"],
        "command failed after waiting for child process"
    );

    assert_eq!(exited["status"], "ok");
    assert!(
        exited["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("exited 7"))
    );
    assert_eq!(
        exited["metadata"]["details"]["terminal_task"]["status_detail"]["exit_code"],
        7
    );
    Ok(())
}

#[test]
fn build_model_picker_options_uses_known_models_and_appends_custom_current() {
    let options = build_model_picker_options(" custom-model ", Vec::new());

    assert!(options.iter().any(|option| option == "deepseek-v4-flash"));
    assert!(options.iter().any(|option| option == "custom-model"));
}

#[test]
fn normalize_runtime_model_maps_aliases_and_trims() {
    assert_eq!(
        normalize_runtime_model("  flash "),
        Some("deepseek-v4-flash".to_owned())
    );
    assert_eq!(
        normalize_runtime_model("v4-pro"),
        Some("deepseek-v4-pro".to_owned())
    );
    assert_eq!(normalize_runtime_model("   "), None);
}

#[test]
fn utility_formatters_cover_threshold_and_unicode_edges() {
    assert_eq!(sidebar_width_for_terminal(95), 0);
    assert_eq!(sidebar_width_for_terminal(96), 24);
    assert_eq!(sidebar_width_for_terminal(160), 42);
    assert_eq!(normalize_command_prefix_character('、'), Some('/'));
    assert_eq!(normalize_command_prefix_character('x'), None);
    assert_eq!(char_to_byte_index("a中b", 2), "a中".len());
    assert_eq!(non_empty_or("   ", "fallback"), "fallback");
}

fn format_terminal_entry(
    task_id: &str,
    status: sigil_kernel::TerminalTaskStatus,
) -> Result<sigil_kernel::TerminalTaskEntry> {
    Ok(sigil_kernel::TerminalTaskEntry {
        handle: sigil_kernel::TerminalTaskHandle {
            task_id: sigil_kernel::TerminalTaskId::new(task_id)?,
            command: "cargo test".to_owned(),
            cwd: std::path::Path::new(".").to_path_buf(),
            shell: "sh".to_owned(),
            log_path: std::path::Path::new(".sigil/tasks")
                .join(task_id)
                .join("output.log"),
            created_at_ms: 10,
        },
        status,
        output_preview: Some("line 1\nline 2".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        updated_at_ms: 20,
    })
}
