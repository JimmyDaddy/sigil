use std::fs;

use anyhow::Result;

use crate::{
    CompactionRecord, McpElicitationDecision, McpElicitationEntry, MemoryConfig,
    ProviderContinuationState, ResponseHandle, ToolEgressEntry, ToolExecutionEntry,
    ToolExecutionStatus, ToolPreview, ToolPreviewFile, ToolPreviewSnapshot, ToolResultMeta,
    ToolSubjectAudit, ToolSubjectKind, ToolSubjectScope, UsageStats, provider::ModelMessage,
};

use super::{
    CompactionConfig, ControlEntry, JsonlSessionStore, PrefixSnapshot, Session, SessionLogEntry,
    session_stats_from_entries,
};

fn request_memory_text(request: &crate::CompletionRequest) -> String {
    request
        .messages
        .iter()
        .filter_map(|message| {
            message
                .id
                .starts_with("memory:")
                .then_some(message.content.as_deref())
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn memory_snapshot_count(entries: &[SessionLogEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(_))
            )
        })
        .count()
}

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
fn tool_egress_control_entry_roundtrips() -> Result<()> {
    let entry = ToolEgressEntry {
        call_id: "call-1".to_owned(),
        tool_name: "mcp__fake__echo".to_owned(),
        destination: "mcp:fake".to_owned(),
        operation: "tools/call".to_owned(),
        subjects: vec![ToolSubjectAudit {
            kind: ToolSubjectKind::McpTool,
            original: "mcp__fake__echo".to_owned(),
            normalized: "mcp__fake__echo".to_owned(),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }],
        payload: serde_json::json!({
            "server": "fake",
            "arguments": {"type": "object", "top_level_keys": ["value"]}
        }),
        redacted: true,
    };
    let session_entry = SessionLogEntry::Control(ControlEntry::ToolEgress(Box::new(entry.clone())));

    let json = serde_json::to_string(&session_entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::ToolEgress(restored))
            if *restored == entry
    ));
    Ok(())
}

#[test]
fn mcp_elicitation_control_entry_roundtrips_without_content_values() -> Result<()> {
    let entry = McpElicitationEntry::new(
        "filesystem",
        "Need an access token for workspace path",
        &serde_json::json!({
            "type": "object",
            "properties": {
                "token": { "type": "string", "title": "Token" },
                "path": { "type": "string", "title": "Path" }
            },
            "required": ["token"]
        }),
        McpElicitationDecision::Accepted,
        Some(&serde_json::json!({
            "token": "secret-token-value",
            "path": "src/lib.rs"
        })),
    );
    let session_entry =
        SessionLogEntry::Control(ControlEntry::McpElicitation(Box::new(entry.clone())));

    let json = serde_json::to_string(&session_entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&json)?;

    assert!(!json.contains("secret-token-value"));
    assert!(!json.contains("src/lib.rs"));
    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::McpElicitation(restored))
            if *restored == entry
                && restored.content_redacted
                && restored.content_field_names == vec!["path".to_owned(), "token".to_owned()]
                && restored.required_field_names == vec!["token".to_owned()]
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
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(_))
        )
    }));

    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
        )
    }));
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(_))
        )
    }));
    Ok(())
}

#[test]
fn build_request_refreshes_session_memory_snapshot_after_disk_memory_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    fs::write(temp.path().join("AGENTS.md"), "repo rules v1\n")?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let memory_config = MemoryConfig { enabled: true };

    session.append_user_message(ModelMessage::user("first"))?;
    let first = session.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    assert!(request_memory_text(&first).contains("repo rules v1"));

    fs::write(temp.path().join("AGENTS.md"), "repo rules v2\n")?;
    session.append_user_message(ModelMessage::user("second"))?;
    let second =
        session.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    let second_memory = request_memory_text(&second);
    assert!(second_memory.contains("repo rules v2"));
    assert!(!second_memory.contains("repo rules v1"));

    let fingerprints = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
                Some(snapshot.memory_fingerprint.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(fingerprints.len(), 2);
    assert_ne!(fingerprints[0], fingerprints[1]);
    assert_eq!(memory_snapshot_count(session.entries()), 2);

    session.append_user_message(ModelMessage::user("third"))?;
    let third = session.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    assert!(request_memory_text(&third).contains("repo rules v2"));
    assert_eq!(memory_snapshot_count(session.entries()), 2);

    let mut restored = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    restored.append_user_message(ModelMessage::user("after restore"))?;
    let restored_request =
        restored.build_request(temp.path(), &memory_config, Vec::new(), None, None, None)?;
    let restored_memory = request_memory_text(&restored_request);
    assert!(restored_memory.contains("repo rules v2"));
    assert!(!restored_memory.contains("repo rules v1"));

    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert_eq!(memory_snapshot_count(&reloaded), 2);
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

#[test]
fn continuation_states_keep_latest_state_per_key_and_provider() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "cursor".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"old"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "cursor".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"new"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "other".to_owned(),
            state_kind: "cursor".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"other"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: None,
            opaque_blob: serde_json::json!({"trace":"kept"}),
        },
    ))?;

    let mut states = session.continuation_states("deepseek");
    states.sort_by(|left, right| left.state_kind.cmp(&right.state_kind));

    assert_eq!(states.len(), 2);
    assert_eq!(states[0].state_kind, "cursor");
    assert_eq!(states[0].opaque_blob["cursor"], "new");
    assert_eq!(states[1].state_kind, "reasoning");
    Ok(())
}

#[test]
fn build_request_only_includes_matching_provider_continuation_states() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("hello"))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-1".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"keep"}),
        },
    ))?;
    session.append_control(ControlEntry::ContinuationStateSaved(
        ProviderContinuationState {
            provider_name: "other-provider".to_owned(),
            state_kind: "reasoning".to_owned(),
            message_id: Some("message-2".to_owned()),
            opaque_blob: serde_json::json!({"cursor":"drop"}),
        },
    ))?;

    let request = session.build_request(
        std::env::temp_dir().as_path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;

    assert_eq!(request.continuation_states.len(), 1);
    assert_eq!(request.continuation_states[0].provider_name, "deepseek");
    assert_eq!(
        request.continuation_states[0].opaque_blob,
        serde_json::json!({"cursor":"keep"})
    );
    Ok(())
}

#[test]
fn ensure_identity_entry_is_idempotent() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    session.ensure_identity_entry()?;
    session.ensure_identity_entry()?;

    let identity_entries = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        })
        .count();
    assert_eq!(identity_entries, 1);
    Ok(())
}

#[test]
fn compaction_preview_returns_none_for_insufficient_history() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_user_message(ModelMessage::user("only one"))?;

    let preview = session.compaction_preview(&CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(1000),
        tail_messages: 2,
    })?;

    assert!(preview.is_none());
    Ok(())
}

#[test]
fn compact_now_rejects_disabled_config() {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    let error = session
        .compact_now(&CompactionConfig {
            enabled: false,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(1000),
            tail_messages: 2,
        })
        .expect_err("disabled compaction should fail");

    assert!(error.to_string().contains("compaction is disabled"));
}

#[test]
fn load_from_store_does_not_duplicate_closed_tool_execution() -> Result<()> {
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
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(12),
            subjects: Vec::new(),
            changed_files: vec!["file.txt".to_owned()],
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }),
    )))?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let interrupted_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == "call-1"
                        && execution.status == ToolExecutionStatus::Interrupted
            )
        })
        .count();

    assert_eq!(interrupted_count, 0);
    Ok(())
}

#[test]
fn jsonl_session_store_ignores_blank_lines_and_reports_parse_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(
        &path,
        format!(
            "\n{}\nnot-json\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("hello")))?,
        ),
    )?;

    let error = JsonlSessionStore::read_entries(&path).expect_err("invalid json should fail");
    assert!(error.to_string().contains("line 3"));
    assert!(error.to_string().contains("session.jsonl"));

    fs::write(
        &path,
        format!(
            "\n{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("hello")))?,
        ),
    )?;
    let entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(entries.len(), 1);
    Ok(())
}

#[test]
fn session_compaction_helpers_cover_disabled_and_insufficient_history_paths() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let disabled = CompactionConfig {
        enabled: false,
        ..CompactionConfig::default()
    };
    let enabled = CompactionConfig {
        enabled: true,
        ..CompactionConfig::default()
    };

    assert!(!session.can_compact(&disabled));
    assert!(
        session
            .compact_now(&disabled)
            .expect_err("disabled compaction should fail")
            .to_string()
            .contains("disabled")
    );
    assert!(
        session
            .compaction_preview(&disabled)
            .expect_err("disabled compaction preview should fail")
            .to_string()
            .contains("disabled")
    );
    assert!(session.compaction_preview(&enabled)?.is_none());

    session.append_user_message(ModelMessage::user("hello"))?;
    assert!(
        session
            .compact_now(&enabled)
            .expect_err("single-message history should be insufficient")
            .to_string()
            .contains("enough history")
    );
    assert!(!session.can_compact(&enabled));

    assert_eq!(session.store_path(), None);
    session.stats_mut().last_prompt_tokens = 9;
    assert_eq!(session.stats().last_prompt_tokens, 9);
    Ok(())
}

#[test]
fn session_projection_helpers_repair_orphans_and_ignore_empty_compaction_records() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
        summary: "   ".to_owned(),
        compacted_message_count: 1,
        retained_tail_message_count: 1,
    }))?;

    let projected = session.messages();
    assert_eq!(projected.len(), 2);
    assert_eq!(projected[1].tool_call_id.as_deref(), Some("call-1"));

    let summary = super::compaction_summary_message(&CompactionRecord {
        summary: "summary".to_owned(),
        compacted_message_count: 2,
        retained_tail_message_count: 1,
    });
    assert_eq!(summary.role, crate::MessageRole::Assistant);
    assert_eq!(summary.content.as_deref(), Some("summary"));
    assert!(summary.id.starts_with("compaction:"));
    Ok(())
}

#[test]
fn session_boundary_summary_and_identity_helpers_cover_tool_edges() {
    assert_eq!(super::compaction_boundary(&[], 2), 0);

    let assistant_call = ModelMessage::assistant(
        None,
        vec![crate::ToolCall {
            id: "call-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    let tool_message = ModelMessage::tool("call-1", "result");
    let user_message = ModelMessage::user("follow up");
    let boundary =
        super::compaction_boundary(&[assistant_call.clone(), tool_message, user_message], 1);
    assert_eq!(boundary, 0);

    let summary = super::summarize_messages(&[
        ModelMessage::system("system prompt"),
        ModelMessage::user("hello\nworld"),
        assistant_call.clone(),
        ModelMessage {
            id: "tool-no-id".to_owned(),
            role: crate::MessageRole::Tool,
            content: Some("content".repeat(80)),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
    ]);
    assert!(summary.contains("01. system system prompt"));
    assert!(summary.contains("03. assistant tool_calls [read_file]"));
    assert!(summary.contains("04. tool unknown =>"));
    assert!(summary.contains("..."));

    assert_eq!(super::truncate_stable("a   b", 10), "a b");
    assert!(super::truncate_stable(&"x".repeat(200), 12).ends_with("..."));

    let identity_entries = vec![SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4".to_owned(),
    })];
    assert_eq!(
        super::session_identity_from_entries(&identity_entries),
        Some(("deepseek".to_owned(), "deepseek-v4".to_owned()))
    );
}

#[test]
fn interrupted_tool_executions_only_keep_open_started_records() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "open".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: Some(5),
            subjects: Vec::new(),
            changed_files: vec!["note.txt".to_owned()],
            metadata: ToolResultMeta {
                changed_files: vec!["note.txt".to_owned()],
                ..ToolResultMeta::default()
            },
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }))),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "done".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }))),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "done".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(1),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: Some("done".to_owned()),
        }))),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "cancelled".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Cancelled,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }))),
    ];

    let interrupted = super::interrupted_tool_executions(&entries);
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].call_id, "open");
    assert_eq!(interrupted[0].status, ToolExecutionStatus::Interrupted);
    assert!(interrupted[0].changed_files.is_empty());
    assert!(interrupted[0].metadata.changed_files.is_empty());
    assert!(interrupted[0].error.is_some());
    assert_eq!(interrupted[0].model_content_hash, None);
}
