use std::{io::Cursor, path::PathBuf};

use super::*;
use crate::app::tests::common::test_config;
use serde_json::json;
use sigil_kernel::ToolResultMeta;

#[test]
fn session_labels_and_identifiers_truncate_as_expected() {
    assert_eq!(
        session_id_from_path(std::path::Path::new("session-abcdef.jsonl")),
        Some("abcdef".to_owned())
    );
    assert_eq!(
        session_id_from_path(std::path::Path::new("other.jsonl")),
        None
    );
    assert_eq!(
        session_history_label("session-1234567890.jsonl"),
        "12345678"
    );
    assert_eq!(session_history_label("plain-label"), "plain-label");

    let titled = SessionHistoryEntry {
        path: PathBuf::from("session-alpha.jsonl"),
        label: "session-alpha.jsonl".to_owned(),
        title: Some("A very long title that should still be visible".to_owned()),
        modified_epoch_secs: 0,
        bytes: 0,
    };
    assert!(session_history_display_label(&titled).starts_with("A very long title"));
}

#[test]
fn bounded_line_reader_handles_short_long_and_eof_lines() -> Result<()> {
    let mut cursor = Cursor::new(b"short\nsecond line is long\nlast".to_vec());
    assert_eq!(
        read_bounded_line(&mut cursor, 10)?,
        Some("short\n".to_owned())
    );
    assert_eq!(read_bounded_line(&mut cursor, 6)?, Some(String::new()));
    assert_eq!(read_bounded_line(&mut cursor, 10)?, Some("last".to_owned()));
    assert_eq!(read_bounded_line(&mut cursor, 10)?, None);
    Ok(())
}

#[test]
fn render_model_and_session_entries_cover_tool_and_control_variants() {
    let tool_call_message = ModelMessage::assistant(
        None,
        vec![sigil_kernel::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    assert_eq!(
        render_model_message_line(&tool_call_message),
        "[assistant] tool_calls [read_file]"
    );
    assert_eq!(
        render_model_message_line(&ModelMessage::tool("call-1", "tool output")),
        "[tool] call-1 => tool output"
    );

    let egress = render_session_log_entry(&SessionLogEntry::Control(ControlEntry::ToolEgress(
        Box::new(ToolEgressEntry {
            call_id: "call-1".to_owned(),
            tool_name: "fetch_url".to_owned(),
            destination: "https://example.com/very/long/path".to_owned(),
            operation: "GET /resource".to_owned(),
            subjects: Vec::new(),
            payload: json!({}),
            redacted: true,
        }),
    )));
    assert!(egress.contains("[ctl] egress call-1 fetch_url"));
    assert!(egress.contains("redacted=true"));
}

#[test]
fn restored_indexes_and_reasoning_helpers_cover_restore_paths() {
    let preview = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &sigil_kernel::ToolPreview {
            title: "Preview".to_owned(),
            summary: "Summary".to_owned(),
            body: "--- current/a\n+++ proposed/a\n@@ -1 +1 @@\n-a\n+b".to_owned(),
            changed_files: vec!["a".to_owned()],
            file_diffs: vec![sigil_kernel::ToolPreviewFile {
                path: "a".to_owned(),
                diff: "--- current/a\n+++ proposed/a\n@@ -1 +1 @@\n-a\n+b".to_owned(),
            }],
        },
        sigil_kernel::ToolDiffBudget::default(),
        None,
    );
    let execution = ToolExecutionEntry {
        call_id: "call-1".to_owned(),
        tool_name: "bash".to_owned(),
        status: ToolExecutionStatus::Interrupted,
        duration_ms: None,
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta::default(),
        error: None,
        model_content_hash: None,
    };
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(execution.clone()))),
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(preview.clone())),
        SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "tool output")),
    ];

    assert_eq!(
        restored_tool_execution_index(&entries)["call-1"].tool_name,
        "bash"
    );
    assert_eq!(
        restored_tool_preview_snapshot_index(&entries)["call-1"].title,
        "Preview"
    );
    assert!(restored_tool_result_call_ids(&entries).contains("call-1"));
    assert!(!should_render_restored_tool_execution(
        &execution,
        &restored_tool_result_call_ids(&entries)
    ));

    let failed = ToolExecutionEntry {
        call_id: "call-2".to_owned(),
        tool_name: "bash".to_owned(),
        status: ToolExecutionStatus::Failed,
        duration_ms: None,
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta::default(),
        error: None,
        model_content_hash: None,
    };
    assert!(should_render_restored_tool_execution(
        &failed,
        &restored_tool_result_call_ids(&entries)
    ));
    assert!(restored_tool_execution_content(&failed).contains("status failed"));
    assert_eq!(
        restored_reasoning_note("reasoning_trace", &json!({ "text": "trace" })),
        Some("trace".to_owned())
    );
    assert_eq!(
        tool_approval_action_label(sigil_kernel::ToolApprovalAuditAction::PreviewFailed),
        "preview_failed"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Cancelled),
        "cancelled"
    );

    let preview_lines = render_compaction_preview_lines(&CompactionPreview {
        record: CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 1,
        },
        folded_messages: vec![ModelMessage::user("before")],
        projected_messages: vec![ModelMessage::assistant(
            Some("after".to_owned()),
            Vec::new(),
        )],
    });
    assert_eq!(preview_lines[0], "/compact preview: fold 2");
    assert!(
        preview_lines
            .iter()
            .any(|line| line.contains("[user] before"))
    );
    assert!(
        preview_lines
            .iter()
            .any(|line| line.contains("[assistant] after"))
    );
}

#[test]
fn session_restore_and_projection_helpers_cover_empty_and_invalid_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..crate::app::tests::common::test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);

    assert!(!app.restore_latest_session_from_disk(&config));
    assert_eq!(
        app.provider_projection_lines(),
        vec!["no provider messages".to_owned()]
    );
    assert_eq!(app.audit_log_lines(), vec!["no audit entries".to_owned()]);

    app.is_busy = true;
    assert!(
        app.session_view_lines()
            .join("\n")
            .contains("running; durable view")
    );

    let invalid_path = temp.path().join(".sigil/sessions/session-invalid.jsonl");
    std::fs::create_dir_all(
        invalid_path
            .parent()
            .expect("invalid session path should have a parent"),
    )?;
    std::fs::write(&invalid_path, "not-json\n")?;
    assert!(!app.restore_session_path_from_disk(
        invalid_path,
        "fallback-provider",
        "fallback-model",
        "restored",
    ));
    Ok(())
}

#[test]
fn session_misc_helpers_cover_resume_ambiguity_and_empty_restore_data() -> Result<()> {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    app.session_history = vec![SessionHistoryEntry {
        path: PathBuf::from("session-current.jsonl"),
        label: "session-current.jsonl".to_owned(),
        title: Some("alpha".to_owned()),
        modified_epoch_secs: 0,
        bytes: 0,
    }];
    app.session_log_path = PathBuf::from("session-current.jsonl");
    assert_eq!(app.resume_candidate_indices(), vec![0]);
    assert_eq!(
        app.resolve_resume_target(""),
        Some(PathBuf::from("session-current.jsonl"))
    );

    app.session_history.push(SessionHistoryEntry {
        path: PathBuf::from("session-other.jsonl"),
        label: "session-other.jsonl".to_owned(),
        title: Some("alpha".to_owned()),
        modified_epoch_secs: 0,
        bytes: 0,
    });
    app.session_history.push(SessionHistoryEntry {
        path: PathBuf::from("session-third.jsonl"),
        label: "session-third.jsonl".to_owned(),
        title: Some("alpha".to_owned()),
        modified_epoch_secs: 0,
        bytes: 0,
    });
    assert_eq!(app.resolve_resume_target("alpha"), None);

    let mut cursor = Cursor::new(vec![b'a'; 16]);
    assert_eq!(read_bounded_line(&mut cursor, 8)?, Some(String::new()));

    let title_file = tempfile::NamedTempFile::new()?;
    std::fs::write(title_file.path(), "\nnot-json\n")?;
    assert_eq!(session_history_title_from_log(title_file.path()), None);

    let before = app.timeline.len();
    app.push_restored_reasoning_delta("");
    assert_eq!(app.timeline.len(), before);

    let mut activity_app =
        AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    activity_app.active_pane = PaneFocus::Activity;
    assert!(current_focus_label(&activity_app).starts_with("activity:"));
    assert_eq!(
        render_model_message_line(&ModelMessage::system("system prompt")),
        "[system] system prompt"
    );
    assert_eq!(
        render_session_log_entry(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("assistant answer".to_owned()),
            Vec::new(),
        ))),
        "[assistant] assistant answer"
    );
    assert_eq!(
        tool_approval_action_label(sigil_kernel::ToolApprovalAuditAction::Requested),
        "requested"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Started),
        "started"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Interrupted),
        "interrupted"
    );
    Ok(())
}
