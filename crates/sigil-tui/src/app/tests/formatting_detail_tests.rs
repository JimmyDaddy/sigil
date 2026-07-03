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
        restored_tool_metadata(Some(&envelope), Some(&execution), None)
            .and_then(|metadata| metadata.bytes),
        Some(42)
    );
    let projected =
        restored_tool_metadata(Some(&envelope), None, None).expect("metadata should exist");
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

    let (json_kind, json_source) = tool_preview_source("ls", "ignored", Some(&json_value), None);
    assert_eq!(json_kind, "json");
    assert!(json_source.as_str().contains("\"items\""));

    let (markdown_kind, markdown_source) =
        tool_preview_source("read_file", "# Title\n\n- item", None, None);
    assert_eq!(markdown_kind, "markdown");
    assert!(markdown_source.as_str().contains("# Title"));

    let rust_metadata = ToolResultMeta {
        details: json!({ "call": { "path": "src/lib.rs", "summary": "path=src/lib.rs" } }),
        ..ToolResultMeta::default()
    };
    let (rust_kind, rust_source) = tool_preview_source(
        "read_file",
        "#[derive(Debug)]\nstruct Example;",
        None,
        Some(&rust_metadata),
    );
    assert_eq!(rust_kind, "code");
    assert!(rust_source.as_str().contains("struct Example"));

    let markdown_metadata = ToolResultMeta {
        details: json!({ "call": { "summary": "path=README.md" } }),
        ..ToolResultMeta::default()
    };
    let (markdown_path_kind, _) = tool_preview_source(
        "read_file",
        "# Title\n\nbody",
        None,
        Some(&markdown_metadata),
    );
    assert_eq!(markdown_path_kind, "markdown");

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

#[test]
fn agent_thread_tool_blocks_include_mode_source_status_and_background_hint() -> anyhow::Result<()> {
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_thread_1")?;
    let run_context = sigil_kernel::AgentRunContextSnapshot {
        profile_snapshot_id: snapshot_id.clone(),
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning_effort: None,
        workspace_root: sigil_kernel::WorkspaceRootSnapshot::new("/workspace")?,
        effective_tool_scope_hash: "tools".to_owned(),
        effective_permission_policy_hash: "permissions".to_owned(),
        effective_mcp_scope_hash: "mcp".to_owned(),
        provider_capability_hash: "provider".to_owned(),
        model_visible_agent_index_hash: None,
        budget_policy_hash: "budget".to_owned(),
        provider_background_handle_ref: None,
    };
    let started_entry = AgentThreadStartedEntry {
        thread_id: thread_id.clone(),
        parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
        parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
        thread_session_ref: sigil_kernel::SessionRef::new_relative("children/thread.jsonl")?,
        profile_id,
        profile_snapshot_id: snapshot_id,
        run_context,
        objective: "inspect runtime".to_owned(),
        prompt_hash: "sha256:prompt".to_owned(),
        invocation_mode: AgentInvocationMode::JoinBeforeFinal,
        invocation_source: AgentInvocationSource::Mention,
        display_name: Some("runtime scan".to_owned()),
        created_at_ms: Some(42),
    };

    let started: serde_json::Value =
        serde_json::from_str(&format_agent_thread_started_block(&started_entry))?;
    assert_eq!(started["tool_name"], "spawn_agent");
    assert_eq!(started["summary"], "join before final · Ctrl-B background");
    assert_eq!(started["preview_value"]["mode"], "join_before_final");
    assert_eq!(started["preview_value"]["source"], "mention");
    assert_eq!(started["preview_value"]["action_hint"], "Ctrl-B background");

    let mut foreground_entry = started_entry.clone();
    foreground_entry.invocation_mode = AgentInvocationMode::Foreground;
    foreground_entry.invocation_source = AgentInvocationSource::Skill;
    let foreground: serde_json::Value =
        serde_json::from_str(&format_agent_thread_started_block(&foreground_entry))?;
    assert_eq!(foreground["summary"], "foreground");
    assert_eq!(foreground["preview_value"]["source"], "skill");

    let mut background_entry = started_entry.clone();
    background_entry.invocation_mode = AgentInvocationMode::Background;
    background_entry.invocation_source = AgentInvocationSource::Plugin;
    let background: serde_json::Value =
        serde_json::from_str(&format_agent_thread_started_block(&background_entry))?;
    assert_eq!(background["summary"], "background");
    assert_eq!(background["preview_value"]["source"], "plugin");
    assert!(background["preview_value"].get("action_hint").is_none());

    let mut system_entry = started_entry.clone();
    system_entry.invocation_mode = AgentInvocationMode::Unknown;
    system_entry.invocation_source = AgentInvocationSource::System;
    let system: serde_json::Value =
        serde_json::from_str(&format_agent_thread_started_block(&system_entry))?;
    assert_eq!(system["summary"], "unknown");
    assert_eq!(system["preview_value"]["source"], "system");

    let mut task_entry = started_entry.clone();
    task_entry.invocation_source = AgentInvocationSource::Task;
    let task: serde_json::Value =
        serde_json::from_str(&format_agent_thread_started_block(&task_entry))?;
    assert_eq!(task["preview_value"]["source"], "task");

    let mut unknown_source_entry = started_entry.clone();
    unknown_source_entry.invocation_source = AgentInvocationSource::Unknown;
    let unknown_source: serde_json::Value =
        serde_json::from_str(&format_agent_thread_started_block(&unknown_source_entry))?;
    assert_eq!(unknown_source["preview_value"]["source"], "unknown");

    let status_entry = AgentThreadStatusChangedEntry {
        thread_id: thread_id.clone(),
        status: AgentThreadStatus::Running,
        reason: Some("agent moved to background".to_owned()),
        updated_at_ms: Some(43),
    };
    let status: serde_json::Value =
        serde_json::from_str(&format_agent_thread_status_block(&status_entry))?;
    assert_eq!(status["tool_name"], "wait_agent");
    assert_eq!(status["summary"], "agent moved to background");
    assert_eq!(status["preview_value"]["status"], "running");
    assert_eq!(
        status["preview_value"]["reason"],
        "agent moved to background"
    );
    for (agent_status, expected) in [
        (AgentThreadStatus::Started, "started"),
        (AgentThreadStatus::Blocked, "blocked"),
        (AgentThreadStatus::Completed, "completed"),
        (AgentThreadStatus::Failed, "failed"),
        (AgentThreadStatus::Cancelled, "cancelled"),
        (AgentThreadStatus::Interrupted, "interrupted"),
        (AgentThreadStatus::Closed, "closed"),
        (AgentThreadStatus::Unavailable, "unavailable"),
        (AgentThreadStatus::Unknown, "unknown"),
    ] {
        let status_entry = AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: agent_status,
            reason: None,
            updated_at_ms: None,
        };
        let status: serde_json::Value =
            serde_json::from_str(&format_agent_thread_status_block(&status_entry))?;
        assert_eq!(status["summary"], expected);
        assert_eq!(status["preview_value"]["status"], expected);
    }
    Ok(())
}

#[test]
fn agent_result_tools_use_agent_preview_sources() -> anyhow::Result<()> {
    let read_payload = serde_json::json!({
        "thread_id": "thread_1",
        "status": "completed",
        "session_ref": "children/thread_1.jsonl",
        "output_hash": "hash",
        "page": {
            "offset_chars": 0,
            "returned_chars": 31,
            "total_chars": 31,
            "next_offset_chars": null,
            "truncated": false,
            "text_omitted": true,
            "text_delivery": "transient_context"
        }
    });
    let read_result = ToolResult::ok(
        "call-read-agent-result",
        "read_agent_result",
        read_payload.to_string(),
        ToolResultMeta::default(),
    );
    let read_display: serde_json::Value = serde_json::from_str(
        &format_tool_result_block_redacted(&read_result, None, &SecretRedactor::empty()),
    )?;

    assert_eq!(read_display["preview_kind"], "agent_result");
    assert_eq!(
        read_display["preview_lines"].as_array().map(Vec::len),
        Some(0)
    );
    assert!(read_display["preview_value"]["page"].get("text").is_none());
    assert_eq!(read_display["preview_value"]["page"]["text_omitted"], true);

    let spawn_payload = serde_json::json!({
        "thread_id": "thread_2",
        "status": "completed",
        "summary": "## Summary\n\nDone",
        "summary_truncated": false
    });
    let spawn_result = ToolResult::ok(
        "call-spawn-agent",
        "spawn_agent",
        spawn_payload.to_string(),
        ToolResultMeta::default(),
    );
    let spawn_display: serde_json::Value = serde_json::from_str(
        &format_tool_result_block_redacted(&spawn_result, None, &SecretRedactor::empty()),
    )?;

    assert_eq!(spawn_display["preview_kind"], "markdown");
    assert_eq!(spawn_display["preview_lines"][0], "## Summary");

    let running_payload = serde_json::json!({
        "thread_id": "agent_chat_1",
        "display_name": "mailbox audit",
        "status": "running",
        "terminal": false,
        "result_available": false,
        "coalescing_key": "wait_agent:agent_chat_1",
        "retry_after_ms": 5000,
        "next_action": "continue only non-overlapping parent work"
    });
    let running_result = ToolResult::ok(
        "call-wait-agent",
        "wait_agent",
        running_payload.to_string(),
        ToolResultMeta::default(),
    );
    let running_display: serde_json::Value = serde_json::from_str(
        &format_tool_result_block_redacted(&running_result, None, &SecretRedactor::empty()),
    )?;

    assert_eq!(running_display["preview_kind"], "text");
    assert_eq!(
        running_display["preview_lines"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        running_display["preview_value"]["coalescing_key"],
        "wait_agent:agent_chat_1"
    );
    Ok(())
}
