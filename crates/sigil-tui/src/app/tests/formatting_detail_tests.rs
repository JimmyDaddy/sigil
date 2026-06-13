use super::*;
use serde_json::json;

#[test]
fn error_and_envelope_helpers_cover_bounds() {
    assert_eq!(strip_error_chain_prefix("1: failed"), "failed");
    assert_eq!(strip_error_chain_prefix(" plain "), "plain");

    let envelope = ToolResult::ok("call-1", "read_file", "hello", ToolResultMeta::default())
        .to_model_content();
    assert!(parse_tool_result_envelope(&envelope).is_some());
    assert!(parse_tool_result_envelope("not json").is_none());
    assert!(
        parse_restored_tool_result_envelope(
            &"x".repeat(RESTORED_TOOL_ENVELOPE_PARSE_MAX_BYTES + 1)
        )
        .is_none()
    );

    let (borrowed, truncated) = bounded_tool_display_content("plain");
    assert_eq!(borrowed, "plain");
    assert!(!truncated);

    let oversized = "中".repeat(70_000);
    let (owned, truncated) = bounded_tool_display_content(&oversized);
    assert!(truncated);
    assert!(owned.as_ref().ends_with("]"));
    assert_eq!(previous_char_boundary("a中b", 2), 1);
}

#[test]
fn restored_metadata_and_error_kind_prefer_execution_state() {
    let execution = ToolExecutionEntry {
        call_id: "call-1".to_owned(),
        tool_name: "bash".to_owned(),
        status: ToolExecutionStatus::Cancelled,
        duration_ms: Some(4),
        subjects: Vec::new(),
        changed_files: vec!["note.txt".to_owned()],
        metadata: ToolResultMeta {
            bytes: Some(42),
            details: json!({"call": {"summary": "command=echo hi"}}),
            ..ToolResultMeta::default()
        },
        error: Some(sigil_kernel::ToolError {
            kind: sigil_kernel::ToolErrorKind::Interrupted,
            message: "cancelled".to_owned(),
            retryable: true,
            details: serde_json::Value::Null,
        }),
        model_content_hash: None,
    };
    let envelope = json!({
        "meta": {
            "bytes": 7,
            "changed_files": ["other.txt"],
            "exit_code": 2,
            "details": { "call": { "summary": "path=other.txt" } }
        },
        "error": { "kind": "timeout" }
    });

    assert_eq!(restored_execution_status_label(&execution), Some("error"));
    assert_eq!(
        restored_tool_metadata(Some(&envelope), Some(&execution))
            .and_then(|metadata| metadata.bytes),
        Some(42)
    );
    let projected = restored_tool_metadata(Some(&envelope), None).expect("metadata should exist");
    assert_eq!(projected.bytes, Some(7));
    assert_eq!(projected.exit_code, Some(2));
    assert_eq!(projected.changed_files, vec!["other.txt".to_owned()]);
    assert_eq!(
        restored_tool_error_kind(Some(&envelope), Some(&execution)),
        Some("interrupted".to_owned())
    );
    assert_eq!(
        restored_tool_error_kind(Some(&envelope), None),
        Some("timeout".to_owned())
    );
}

#[test]
fn preview_helpers_cover_json_markdown_and_limits() {
    let invalid = parse_tool_content_value("plain text");
    assert_eq!(invalid, serde_json::Value::String("plain text".to_owned()));
    let json_value = tool_preview_value(r#"{"items":[1,2]}"#).expect("json preview should parse");
    assert!(matches!(json_value, serde_json::Value::Object(_)));

    let (json_kind, json_source) = tool_preview_source("ls", "ignored", Some(&json_value));
    assert_eq!(json_kind, "json");
    assert!(json_source.as_str().contains("\"items\""));

    let (markdown_kind, markdown_source) =
        tool_preview_source("read_file", "# Title\n\n- item", None);
    assert_eq!(markdown_kind, "markdown");
    assert!(markdown_source.as_str().contains("# Title"));

    let compacted = compact_preview_value(
        &json!({
            "items": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            "deep": { "nested": { "value": { "leaf": "x" } } },
            "text": "a".repeat(200)
        }),
        0,
    );
    assert_eq!(compacted["items"][10], "… 1 more items");
    assert_eq!(compacted["deep"]["nested"]["value"], "… 1 keys");
    assert!(
        compacted["text"]
            .as_str()
            .is_some_and(|text: &str| text.ends_with("..."))
    );

    let lines = (0..20)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>();
    assert_eq!(
        select_tool_preview_lines("bash", &lines).first(),
        Some(&"line 4".to_owned())
    );
    assert_eq!(tool_preview_limit("bash"), 16);
    assert_eq!(tool_preview_limit("ls"), 14);
    assert_eq!(tool_preview_limit("other"), 12);
    assert_eq!(
        format_tool_preview_summary("bash", 20, 16, 4, 2_048),
        "last 16/20 lines · 2.0 KB"
    );
    assert_eq!(
        format_tool_preview_summary("ls", 2, 2, 0, 800),
        "2 lines · 800 B"
    );
    assert_eq!(format_bytes(1_500), "1.5 KB");
    assert_eq!(format_bytes(2_000_000), "2.0 MB");
    assert!(looks_like_markdown_document("# Heading"));
    assert!(looks_like_markdown_document("| a | b |\n| --- | --- |"));
    assert!(!looks_like_markdown_document("plain text"));
}
