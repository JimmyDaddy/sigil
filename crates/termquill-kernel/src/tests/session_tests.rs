use std::fs;

use anyhow::Result;

use crate::{
    CompactionRecord, MemoryConfig, ProviderContinuationState, ResponseHandle, ToolExecutionEntry,
    ToolExecutionStatus, ToolPreview, ToolPreviewFile, ToolPreviewSnapshot, ToolResultMeta,
    UsageStats, provider::ModelMessage,
};

use super::{
    CompactionConfig, ControlEntry, JsonlSessionStore, PrefixSnapshot, Session, SessionLogEntry,
    session_stats_from_entries,
};

#[test]
fn load_from_store_recovers_identity_from_prefix_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
            materialized_text: "prefix".to_owned(),
            sha256: "abc".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "none".to_owned(),
            tool_schema_fingerprint: "tools".to_owned(),
            skill_index_fingerprint: "skills".to_owned(),
        }),
    ))?;

    let session = Session::load_from_store("other-provider", "other-model", store)?;

    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name,
                model_name,
            }) if provider_name == "deepseek" && model_name == "deepseek-v4-flash"
        )
    }));
    Ok(())
}

#[test]
fn load_from_store_persists_identity_for_empty_log() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");
    assert_eq!(session.entries().len(), 1);
    assert!(matches!(
        session.entries()[0],
        SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
    ));
    Ok(())
}

#[test]
fn tool_preview_captured_control_entry_roundtrips() -> Result<()> {
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &ToolPreview {
            title: "Write file".to_owned(),
            summary: "Create a file".to_owned(),
            body: "preview body".to_owned(),
            changed_files: vec!["README.md".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "README.md".to_owned(),
                diff: "--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+hello".to_owned(),
            }],
        },
        Default::default(),
        Some("preview-hash".to_owned()),
    );
    let entry = SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot.clone()));

    let json = serde_json::to_string(&entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(restored))
            if restored == snapshot
    ));
    Ok(())
}

#[test]
fn build_request_persists_prefix_snapshot_in_memory_and_store() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    fs::write(temp.path().join("AGENTS.md"), "repo rules\n")?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_user_message(ModelMessage::user("hello"))?;

    let request = session.build_request(
        temp.path(),
        &MemoryConfig { enabled: true },
        Vec::new(),
        None,
        None,
        None,
    )?;

    assert_eq!(request.provider_name, "deepseek");
    assert!(
        request
            .messages
            .iter()
            .any(|message| matches!(message.role, crate::MessageRole::System))
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
        )
    }));

    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
        )
    }));
    Ok(())
}

#[test]
fn messages_repair_orphan_tool_call_projection() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_user_message(ModelMessage::user("continue"))?;

    let projected = session.messages();

    assert_eq!(projected.len(), 3);
    assert!(matches!(projected[0].role, crate::MessageRole::Assistant));
    assert!(matches!(projected[1].role, crate::MessageRole::Tool));
    assert_eq!(projected[1].id, "local_repair:missing_tool_result:call-1");
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call-1"));
    assert!(projected[1].content.as_deref().is_some_and(|content| {
        content.contains("did not return a result before the previous run stopped")
            && content.contains(r#""kind":"interrupted""#)
    }));
    assert!(matches!(projected[2].role, crate::MessageRole::User));
    Ok(())
}

#[test]
fn load_from_store_marks_started_tool_execution_as_interrupted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }),
    )))?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Interrupted
                    && execution.error.as_ref().is_some_and(|error| {
                        error.kind == crate::ToolErrorKind::Interrupted && error.retryable
                    })
        )
    }));
    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Interrupted
        )
    }));
    Ok(())
}

#[test]
fn latest_control_state_queries_return_latest_matching_records() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
        provider_name: "deepseek".to_owned(),
        response_id: "response-old".to_owned(),
        continuation_cursor: Some("cursor-old".to_owned()),
    }))?;
    session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
        provider_name: "other".to_owned(),
        response_id: "response-other".to_owned(),
        continuation_cursor: None,
    }))?;
    session.append_control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
        materialized_text: "prefix-old".to_owned(),
        sha256: "old".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        memory_fingerprint: "memory-old".to_owned(),
        tool_schema_fingerprint: "tools-old".to_owned(),
        skill_index_fingerprint: "skills-old".to_owned(),
    }))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"old"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"new"}),
        },
    ))?;
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "summary-old".to_owned(),
        compacted_message_count: 1,
        retained_tail_message_count: 2,
    }))?;
    session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
        provider_name: "deepseek".to_owned(),
        response_id: "response-new".to_owned(),
        continuation_cursor: Some("cursor-new".to_owned()),
    }))?;
    session.append_control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
        materialized_text: "prefix-new".to_owned(),
        sha256: "new".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        memory_fingerprint: "memory-new".to_owned(),
        tool_schema_fingerprint: "tools-new".to_owned(),
        skill_index_fingerprint: "skills-new".to_owned(),
    }))?;
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "summary-new".to_owned(),
        compacted_message_count: 3,
        retained_tail_message_count: 2,
    }))?;

    assert!(matches!(
        session.latest_response_handle("deepseek"),
        Some(handle) if handle.response_id == "response-new"
            && handle.continuation_cursor.as_deref() == Some("cursor-new")
    ));
    assert!(matches!(
        session.latest_response_handle("other"),
        Some(handle) if handle.response_id == "response-other"
    ));
    assert!(matches!(
        session.latest_prefix_snapshot(),
        Some(snapshot) if snapshot.sha256 == "new"
    ));
    assert!(matches!(
        session.latest_compaction_record(),
        Some(record) if record.summary == "summary-new"
    ));
    let states = session.continuation_states("deepseek");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].opaque_blob, serde_json::json!({"cursor":"new"}));
    Ok(())
}

#[test]
fn compaction_persists_record_and_projects_summary_plus_tail() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("step one"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("step two".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("step three"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("step four".to_owned()),
        Vec::new(),
    ))?;

    let record = session.compact_now(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 2,
    })?;

    assert_eq!(record.compacted_message_count, 2);
    assert_eq!(record.retained_tail_message_count, 2);
    assert!(session.entries().iter().any(|entry| {
        matches!(entry, SessionLogEntry::Control(ControlEntry::CompactionApplied(saved)) if saved == &record)
    }));

    let projected = session.messages();
    assert_eq!(projected.len(), 3);
    assert!(
        projected[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Compacted 2 earlier messages"))
    );
    assert_eq!(projected[1].content.as_deref(), Some("step three"));
    assert_eq!(projected[2].content.as_deref(), Some("step four"));
    Ok(())
}

#[test]
fn can_compact_requires_a_safe_boundary() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "tool-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{\"path\":\"README.md\"}".to_owned(),
        }],
    ))?;
    session.append_tool_message(ModelMessage::tool("tool-1", "ok"))?;

    assert!(!session.can_compact(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 1,
    }));
    Ok(())
}

#[test]
fn compaction_preview_reports_folded_messages_and_projected_after_state() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("alpha"))?;
    session
        .append_assistant_message(ModelMessage::assistant(Some("beta".to_owned()), Vec::new()))?;
    session.append_user_message(ModelMessage::user("gamma"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("delta".to_owned()),
        Vec::new(),
    ))?;

    let preview = session
        .compaction_preview(&CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(1000),
            tail_messages: 2,
        })?
        .expect("preview should exist");

    assert_eq!(preview.record.compacted_message_count, 2);
    assert_eq!(preview.folded_messages.len(), 2);
    assert_eq!(preview.projected_messages.len(), 3);
    assert!(
        preview.projected_messages[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Compacted 2 earlier messages"))
    );
    assert_eq!(
        preview.projected_messages[1].content.as_deref(),
        Some("gamma")
    );
    assert_eq!(
        preview.projected_messages[2].content.as_deref(),
        Some("delta")
    );
    Ok(())
}

#[test]
fn load_from_store_accepts_legacy_pascal_case_control_entries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("legacy-session.jsonl");
    fs::write(
        &path,
        "{\"Control\":{\"SessionIdentity\":{\"provider_name\":\"deepseek\",\"model_name\":\"deepseek-v4-flash\"}}}\n",
    )?;

    let store = JsonlSessionStore::new(&path)?;
    let session = Session::load_from_store("fallback-provider", "fallback-model", store)?;

    assert_eq!(session.provider_name(), "deepseek");
    assert_eq!(session.model_name(), "deepseek-v4-flash");
    Ok(())
}

#[test]
fn session_stats_are_restored_from_usage_snapshots() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 120,
            completion_tokens: 10,
            cache_hit_tokens: 90,
            cache_miss_tokens: 30,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        })),
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 48,
            completion_tokens: 6,
            cache_hit_tokens: 28,
            cache_miss_tokens: 20,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        })),
        SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 2,
        })),
    ];

    let stats = session_stats_from_entries(&entries);
    let session = Session::from_entries("deepseek", "deepseek-v4-flash", entries);

    assert_eq!(stats.prompt_tokens, 168);
    assert_eq!(stats.last_prompt_tokens, 0);
    assert_eq!(session.stats().prompt_tokens, 168);
    assert_eq!(session.stats().last_prompt_tokens, 0);
    Ok(())
}
