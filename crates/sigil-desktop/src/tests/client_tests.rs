use super::*;

#[test]
fn error_code_projection_accepts_only_bounded_machine_labels() {
    assert_eq!(
        safe_error_code("stale_cursor".to_owned()).as_deref(),
        Some("stale_cursor")
    );
    assert!(safe_error_code("contains space".to_owned()).is_none());
    assert!(safe_error_code("x".repeat(129)).is_none());
}

#[test]
fn event_stream_owner_revision_requires_exact_opaque_format() {
    assert!(validate_owner_revision(&format!("sha256:{}", "a".repeat(64))).is_ok());
    assert!(validate_owner_revision(&format!("sha256:{}", "A".repeat(64))).is_err());
    assert!(validate_owner_revision("sha256:short").is_err());
}

#[test]
fn typed_client_debug_never_projects_transport_or_bearer_material() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );
    let debug = format!("{client:?}");

    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("3210"));
}

#[test]
fn run_context_decodes_exact_typed_server_contract() {
    let context: crate::DesktopRunContextView = serde_json::from_value(serde_json::json!({
        "provider_name": "deepseek",
        "model_name": "deepseek-v4-flash",
        "model_selection": "per_run",
        "model_selection_binding": "model-binding",
        "available_models": ["deepseek-v4-flash", "deepseek-v4-pro"],
        "model_options": [
            {
                "model_name": "deepseek-v4-flash",
                "available_reasoning_efforts": ["low", "medium", "high", "max"],
                "default_reasoning_effort": "max",
                "reasoning_effort_binding": "effort-binding-flash"
            },
            {
                "model_name": "deepseek-v4-pro",
                "available_reasoning_efforts": ["low", "medium", "high", "max"],
                "default_reasoning_effort": "max",
                "reasoning_effort_binding": "effort-binding-pro"
            }
        ],
        "default_permission_mode": "manual",
        "available_permission_modes": ["read-only", "manual", "auto-edit", "danger-full-access"],
        "available_reasoning_efforts": ["low", "medium", "high", "max"],
        "default_reasoning_effort": "max",
        "reasoning_effort_binding": "effort-binding",
        "context_window_tokens": 1_000_000,
        "last_prompt_tokens": 42_000,
        "context_window_source": "provider",
        "extension_catalog": {
            "commands": [],
            "skills": [],
            "agents": []
        }
    }))
    .expect("run context should decode");

    assert_eq!(context.model_name, "deepseek-v4-flash");
    assert_eq!(context.last_prompt_tokens, Some(42_000));
    assert_eq!(context.available_reasoning_efforts.len(), 4);
    assert_eq!(context.model_options.len(), 2);
    assert_eq!(
        context.model_options[1].reasoning_effort_binding.as_deref(),
        Some("effort-binding-pro")
    );
    assert_eq!(
        context.reasoning_effort_binding.as_deref(),
        Some("effort-binding")
    );
    assert_eq!(
        context.model_selection,
        crate::DesktopModelSelectionPolicy::PerRun
    );
}

#[test]
fn continuity_decodes_nested_owner_and_redacts_durable_scope_from_debug() {
    let continuity: crate::DesktopSessionContinuityView =
        serde_json::from_value(serde_json::json!({
            "durable_session_scope_id": "durable-private-scope",
            "durable_frontier": { "through_stream_sequence": 17 },
            "foreground_owner": {
                "run_id": "http-run-7",
                "owner_revision": format!("sha256:{}", "a".repeat(64))
            },
            "recovery_actions": ["retry_current", "continue_read_only"]
        }))
        .expect("continuity should decode");

    assert_eq!(continuity.durable_frontier.through_stream_sequence, 17);
    assert_eq!(
        continuity
            .foreground_owner
            .as_ref()
            .map(|owner| owner.run_id.as_str()),
        Some("http-run-7")
    );
    assert_eq!(continuity.recovery_actions.len(), 2);
    assert!(!format!("{continuity:?}").contains("durable-private-scope"));
}

#[test]
fn compaction_review_and_apply_action_preserve_exact_preview_binding() {
    let review: crate::DesktopCompactionReview = serde_json::from_value(serde_json::json!({
        "preview_id": "compact-preview-1",
        "folded_event_count": 8,
        "retained_event_count": 4,
        "admission": {
            "kind": "ready",
            "economics": {
                "before_input_tokens": 12_000,
                "target_input_tokens": 4_000,
                "context_window_tokens": 128_000,
                "output_tokens": 8_000,
                "safety_buffer_tokens": 2_000,
                "savings_tokens": 8_000,
                "savings_ratio_ppm": 666_666,
                "minimum_savings_tokens": 1_000,
                "minimum_savings_ratio_ppm": 100_000
            }
        }
    }))
    .expect("compaction review should decode");

    assert_eq!(review.preview_id.as_deref(), Some("compact-preview-1"));
    assert!(matches!(
        review.admission,
        crate::DesktopCompactionAdmission::Ready { .. }
    ));
    assert_eq!(
        serde_json::to_value(
            crate::DesktopConversationRecoveryCommandAction::ApplyCompaction {
                preview_id: "compact-preview-1".to_owned(),
            }
        )
        .expect("compaction action should encode"),
        serde_json::json!({
            "kind": "apply_compaction",
            "preview_id": "compact-preview-1"
        })
    );
}

#[test]
fn recovery_receipt_decodes_compaction_without_weakening_durable_identity() {
    let receipt: crate::DesktopConversationRecoveryCommandReceipt =
        serde_json::from_value(serde_json::json!({
            "command_id": "command-1",
            "client_id": "desktop-1",
            "session_id": "session-1",
            "action": "apply_compaction",
            "compaction": {
                "compaction_id": "compaction-1",
                "attempt_id": "attempt-1",
                "task_memory_id": "memory-1",
                "folded_event_count": 8,
                "tool_output_projection_recorded": true
            },
            "recovery": {
                "checkpoints": [],
                "fork_points": [],
                "through_stream_sequence": 42
            },
            "correlation_id": "correlation-1",
            "replayed": false
        }))
        .expect("recovery receipt should decode");

    assert_eq!(
        receipt
            .compaction
            .as_ref()
            .map(|compaction| compaction.compaction_id.as_str()),
        Some("compaction-1")
    );
    assert_eq!(receipt.recovery.through_stream_sequence, 42);
    assert!(!receipt.replayed);
}

#[test]
fn conversation_display_decodes_exact_decimal_text_and_opaque_cursor() {
    let page: crate::DesktopConversationDisplayPage = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "request_scope": "http-session-1",
        "through_session_stream_sequence": "9007199254740993",
        "terminal_frontier": {
            "run_id": "run-1",
            "session_stream_sequence": "9007199254740994",
            "status": "succeeded"
        },
        "total_items": "9007199254740995",
        "items": [{
            "schema_version": 1,
            "display_id": "display-1",
            "display_order": {
                "session_stream_sequence": "9007199254740993",
                "subindex": 0
            },
            "source_event_id": "event-1",
            "kind": "assistant_message",
            "source": "durable_transcript",
            "run_id": "run-1",
            "run_sequence": "9007199254740996",
            "status": "completed",
            "content": {
                "type": "message",
                "role": "assistant",
                "text": "done",
                "assistant_phase": "final_answer",
                "image_attachment_count": 0,
                "truncated": false,
                "original_content_bytes": 4
            },
            "reconciles": ["live-1"]
        }],
        "next_cursor": "opaque_CURSOR-1",
        "has_more": true,
        "gap_facts": [{
            "kind": "retention",
            "after_session_stream_sequence": "9007199254740997"
        }],
        "live_provisional_anchor": {
            "durable_frontier": "9007199254740993",
            "run_id": "run-live",
            "run_sequence": "9007199254740998"
        }
    }))
    .expect("canonical display page should decode");

    assert_eq!(page.through_session_stream_sequence, "9007199254740993");
    assert_eq!(page.total_items, "9007199254740995");
    assert_eq!(
        page.items[0].display_order.session_stream_sequence,
        "9007199254740993"
    );
    assert_eq!(
        page.items[0].run_sequence.as_deref(),
        Some("9007199254740996")
    );
    assert_eq!(page.next_cursor.as_deref(), Some("opaque_CURSOR-1"));
    assert_eq!(
        page.live_provisional_anchor
            .as_ref()
            .map(|anchor| anchor.run_sequence.as_str()),
        Some("9007199254740998")
    );
}

#[test]
fn conversation_display_rejects_noncanonical_decimal_text() {
    for invalid in ["01", "18446744073709551616", "-1", "1.0"] {
        let result =
            serde_json::from_value::<crate::DesktopConversationDisplayPage>(serde_json::json!({
                "schema_version": 1,
                "request_scope": "http-session-1",
                "through_session_stream_sequence": invalid,
                "total_items": "0",
                "items": [],
                "has_more": false,
                "gap_facts": []
            }));
        assert!(result.is_err(), "{invalid} must be rejected");
    }
}

#[test]
fn conversation_queue_decodes_bounded_secret_free_rows() {
    let view: crate::DesktopConversationQueueView = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "session_id": "session-1",
        "generation": "queue-v1:17:event-1",
        "paused": false,
        "total_items": 1,
        "items": [{
            "entry_id": "queue-1",
            "order": 0,
            "kind": "chat",
            "status": "queued",
            "prompt_preview": "[redacted]",
            "prompt_preview_truncated": false,
            "prompt_material": "available_process_local",
            "dispatchable": false,
            "blocked_reason": "foreground_run_active",
            "created_at_ms": 10,
            "updated_at_ms": 11
        }],
        "truncated": false
    }))
    .expect("queue view should decode");

    validate_conversation_queue_view("session-1", &view)
        .expect("bounded queue view should validate");
    assert_eq!(view.generation.0, "queue-v1:17:event-1");
    assert_eq!(
        view.items[0].prompt_material,
        crate::DesktopConversationQueuePromptMaterial::AvailableProcessLocal
    );
    let debug = format!("{view:?}");
    assert!(!debug.contains("prompt_hash"));
    assert!(!debug.contains("exact private prompt"));
}

#[test]
fn conversation_queue_rejects_unbounded_or_inconsistent_server_views() {
    let item = crate::DesktopConversationQueueItem {
        entry_id: "queue-1".to_owned(),
        order: 0,
        kind: crate::DesktopConversationQueueItemKind::Chat,
        status: crate::DesktopConversationQueueItemStatus::Queued,
        prompt_preview: "safe".to_owned(),
        prompt_preview_truncated: false,
        prompt_material: crate::DesktopConversationQueuePromptMaterial::PersistedSafe,
        dispatchable: true,
        blocked_reason: None,
        created_at_ms: None,
        updated_at_ms: None,
    };
    let base = crate::DesktopConversationQueueView {
        schema_version: 1,
        session_id: "session-1".to_owned(),
        generation: crate::DesktopConversationQueueGeneration("queue-v1:0:initial".to_owned()),
        paused: false,
        total_items: 1,
        items: vec![item.clone()],
        truncated: false,
        next_dispatchable_entry_id: Some("queue-1".to_owned()),
    };
    validate_conversation_queue_view("session-1", &base).expect("consistent view should validate");

    let mut oversized = base.clone();
    oversized.total_items = 101;
    oversized.items = vec![item; 101];
    assert!(validate_conversation_queue_view("session-1", &oversized).is_err());

    let mut missing_block = base;
    missing_block.items[0].dispatchable = false;
    assert!(validate_conversation_queue_view("session-1", &missing_block).is_err());

    let truncated = crate::DesktopConversationQueueView {
        schema_version: 1,
        session_id: "session-1".to_owned(),
        generation: crate::DesktopConversationQueueGeneration("queue-v1:0:initial".to_owned()),
        paused: false,
        total_items: 2,
        items: vec![crate::DesktopConversationQueueItem {
            entry_id: "queue-1".to_owned(),
            order: 0,
            kind: crate::DesktopConversationQueueItemKind::Chat,
            status: crate::DesktopConversationQueueItemStatus::Queued,
            prompt_preview: "redacted".to_owned(),
            prompt_preview_truncated: false,
            prompt_material: crate::DesktopConversationQueuePromptMaterial::RequiresReentry,
            dispatchable: false,
            blocked_reason: Some(crate::DesktopConversationQueueBlockedReason::RequiresReentry),
            created_at_ms: None,
            updated_at_ms: None,
        }],
        truncated: true,
        next_dispatchable_entry_id: Some("queue-2".to_owned()),
    };
    validate_conversation_queue_view("session-1", &truncated)
        .expect("a bounded view may point at the next row beyond its returned window");
}

#[test]
fn conversation_queue_command_serializes_cas_and_owner_binding() {
    let request = crate::DesktopConversationQueueCommandRequest {
        expected_generation: crate::DesktopConversationQueueGeneration(
            "queue-v1:17:event-1".to_owned(),
        ),
        action: crate::DesktopConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id: "run-7".to_owned(),
            foreground_owner_revision: format!("sha256:{}", "a".repeat(64)),
        },
    };
    validate_conversation_queue_command(&request).expect("exact owner binding should validate");
    let value = serde_json::to_value(&request).expect("queue command should serialize");
    assert_eq!(value["expected_generation"], "queue-v1:17:event-1");
    assert_eq!(value["action"]["action"], "interrupt_and_run_next");
    assert_eq!(value["action"]["foreground_run_id"], "run-7");

    let invalid = crate::DesktopConversationQueueCommandRequest {
        expected_generation: crate::DesktopConversationQueueGeneration(
            "queue-v1:17:event-1".to_owned(),
        ),
        action: crate::DesktopConversationQueueCommandAction::Reorder {
            entry_id: "queue-1".to_owned(),
            after_entry_id: Some("queue-1".to_owned()),
        },
    };
    assert!(validate_conversation_queue_command(&invalid).is_err());
}

#[test]
fn conversation_queue_receipt_echoes_cas_and_exact_interrupt_owner() {
    let receipt: crate::DesktopConversationQueueCommandReceipt =
        serde_json::from_value(serde_json::json!({
            "command_id": "command-queue-1",
            "client_id": "desktop-1",
            "session_id": "session-1",
            "action": "interrupt_and_run_next",
            "expected_generation": "queue-v1:17:event-1",
            "generation": "queue-v1:18:event-2",
            "interrupt_owner": {
                "run_id": "run-7",
                "owner_revision": format!("sha256:{}", "a".repeat(64))
            },
            "queue": {
                "schema_version": 1,
                "session_id": "session-1",
                "generation": "queue-v1:18:event-2",
                "paused": false,
                "total_items": 0,
                "items": [],
                "truncated": false,
                "next_dispatchable_entry_id": null
            },
            "correlation_id": "event-2",
            "replayed": false
        }))
        .expect("queue receipt should decode");

    assert_eq!(receipt.expected_generation.0, "queue-v1:17:event-1");
    let owner = receipt
        .interrupt_owner
        .as_ref()
        .expect("interrupt receipt should bind the exact foreground owner");
    assert_eq!(owner.run_id, "run-7");
    assert_eq!(owner.owner_revision, format!("sha256:{}", "a".repeat(64)));
    validate_conversation_queue_view("session-1", &receipt.queue)
        .expect("receipt queue projection should remain bounded and consistent");
}

#[test]
fn agent_activity_decodes_bounded_result_handoff_without_storage_identity() {
    let activity: crate::DesktopAgentActivityView = serde_json::from_value(serde_json::json!({
        "total_agents": 1,
        "active_agents": 0,
        "terminal_agents": 1,
        "items": [{
            "thread_id": "agent_review",
            "profile_id": "explore",
            "display_name": "Repository review",
            "objective": "Inspect the architecture",
            "status": "completed",
            "handoff_status": "returned",
            "result_summary": "The bounded result reached the parent conversation.",
            "result_summary_truncated": false,
            "usage": {
                "input_tokens": 240,
                "output_tokens": 80,
                "total_tokens": 320,
                "cached_tokens": 40
            }
        }]
    }))
    .expect("agent activity should decode");

    assert_eq!(activity.total_agents, 1);
    assert_eq!(activity.items[0].thread_id, "agent_review");
    assert_eq!(
        activity.items[0].handoff_status,
        crate::DesktopAgentHandoffStatus::Returned
    );
    assert_eq!(
        activity.items[0]
            .usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(320)
    );
    let debug = format!("{activity:?}");
    assert!(!debug.contains("session_ref"));
    assert!(!debug.contains("output_hash"));
    assert!(!debug.contains("changed_paths"));
}

#[test]
fn support_report_decodes_only_the_path_free_contract() {
    let report: crate::DesktopSupportDoctorReport = serde_json::from_value(serde_json::json!({
        "generated_at_unix_ms": 123,
        "version": "0.0.1-test",
        "commit": "abc123",
        "target": "aarch64-apple-darwin",
        "profile": "debug",
        "environment": {
            "os": "macos",
            "architecture": "aarch64",
            "terminal_family": "other"
        },
        "summary": { "overall_status": "warn", "ok": 4, "warn": 1, "error": 0 },
        "checks": [{
            "status": "warn",
            "name": "configuration",
            "summary": "review one setting",
            "remediation": "update configuration"
        }],
        "privacy": {
            "included": ["build metadata"],
            "excluded": ["local paths"],
            "review_before_sharing": true
        }
    }))
    .expect("support report should decode");

    assert_eq!(report.summary.warn, 1);
    assert_eq!(report.checks[0].name, "configuration");
    assert_eq!(report.privacy.excluded, ["local paths"]);
}

#[test]
fn session_management_contract_is_exact_and_path_free() {
    let rename = DesktopSessionRenameRequest {
        session_ref: "managed.jsonl".to_owned(),
        session_id: "durable-managed".to_owned(),
        display_name: "Readable name".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(rename).expect("rename should encode"),
        serde_json::json!({
            "session_ref": "managed.jsonl",
            "session_id": "durable-managed",
            "display_name": "Readable name"
        })
    );
    let receipt = serde_json::from_value::<DesktopSessionMutationReceipt>(serde_json::json!({
        "session_ref": "managed.jsonl",
        "session_id": "durable-managed",
        "operation_id": "session-display-name:1",
        "projection_generation": 2
    }))
    .expect("receipt should decode");
    assert_eq!(receipt.projection_generation, Some(2));
    assert!(!format!("{receipt:?}").contains('/'));

    let quarantine = DesktopSessionQuarantineRequest {
        session_ref: "broken.jsonl".to_owned(),
        source_bytes: 17,
        source_modified_at_unix_ms: 42,
    };
    assert_eq!(
        serde_json::to_value(quarantine).expect("quarantine should encode"),
        serde_json::json!({
            "session_ref": "broken.jsonl",
            "source_bytes": 17,
            "source_modified_at_unix_ms": 42
        })
    );
    let quarantine_receipt =
        serde_json::from_value::<DesktopSessionQuarantineReceipt>(serde_json::json!({
            "session_ref": "broken.jsonl",
            "operation_id": "session-quarantine:1",
            "quarantine_name": "1--broken.jsonl",
            "projection_generation": 3
        }))
        .expect("quarantine receipt should decode");
    assert_eq!(quarantine_receipt.projection_generation, Some(3));

    let delete_invalid = DesktopSessionInvalidSourceDeleteRequest {
        session_ref: "broken.jsonl".to_owned(),
        source_bytes: 17,
        source_modified_at_unix_ms: 42,
    };
    assert_eq!(
        serde_json::to_value(delete_invalid).expect("invalid source delete should encode"),
        serde_json::json!({
            "session_ref": "broken.jsonl",
            "source_bytes": 17,
            "source_modified_at_unix_ms": 42
        })
    );
    let delete_invalid_receipt =
        serde_json::from_value::<DesktopSessionInvalidSourceDeleteReceipt>(serde_json::json!({
            "session_ref": "broken.jsonl",
            "operation_id": "invalid-source-delete:1",
            "projection_generation": 4
        }))
        .expect("invalid source delete receipt should decode");
    assert_eq!(delete_invalid_receipt.projection_generation, Some(4));
}

#[tokio::test]
async fn transcript_query_rejects_unbounded_renderer_values_before_transport() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );

    assert!(matches!(
        client
            .transcript(
                "session-1",
                &DesktopTranscriptQuery {
                    before: None,
                    limit: Some(101),
                },
            )
            .await,
        Err(DesktopClientError::InvalidRoute)
    ));
    assert!(matches!(
        client
            .transcript(
                "session-1",
                &DesktopTranscriptQuery {
                    before: Some(0),
                    limit: Some(50),
                },
            )
            .await,
        Err(DesktopClientError::InvalidRoute)
    ));
}

#[tokio::test]
async fn conversation_display_query_rejects_unbounded_values_before_transport() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );

    for query in [
        DesktopConversationDisplayQuery {
            cursor: Some(String::new()),
            limit: Some(50),
        },
        DesktopConversationDisplayQuery {
            cursor: Some("bad\ncursor".to_owned()),
            limit: Some(50),
        },
        DesktopConversationDisplayQuery {
            cursor: Some("x".repeat(4_097)),
            limit: Some(50),
        },
        DesktopConversationDisplayQuery {
            cursor: None,
            limit: Some(0),
        },
        DesktopConversationDisplayQuery {
            cursor: None,
            limit: Some(101),
        },
    ] {
        assert!(matches!(
            client.conversation_display("session-1", &query).await,
            Err(DesktopClientError::InvalidRoute)
        ));
    }
}

#[test]
fn sse_decoder_accepts_durable_and_transient_frames_and_rejects_gaps() {
    let durable = br#"id: sigil-http-run-v1:session-1:run-1:1
event: run_event
data: {"schema_version":2,"event_class":"durable","replay_id":"sigil-http-run-v1:session-1:run-1:1","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-1","sequence":1,"event":{"type":"run_started","prompt":"hello"}}}
"#;
    let decoded = decode_sse_frame(durable, "session-1", "run-1")
        .expect("frame should decode")
        .expect("frame should contain an event");
    assert_eq!(decoded.run_event.sequence, 1);

    let transient = br#"event: run_event
data: {"schema_version":2,"event_class":"transient","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-1","sequence":2,"event":{"type":"text_delta","text":"live"}}}
"#;
    let decoded = decode_sse_frame(transient, "session-1", "run-1")
        .expect("frame should decode")
        .expect("frame should contain an event");
    assert_eq!(decoded.event_class, DesktopProtocolEventClass::Transient);

    let gap = br#"event: stream_gap
data: {"dropped_live_events":1}
"#;
    assert!(matches!(
        decode_sse_frame(gap, "session-1", "run-1"),
        Err(DesktopClientError::EventStreamGap)
    ));
}

#[test]
fn sse_decoder_rejects_cursor_or_stream_mismatch() {
    let mismatched_cursor = br#"id: cursor-other
event: run_event
data: {"schema_version":2,"event_class":"durable","replay_id":"sigil-http-run-v1:session-1:run-1:1","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-1","sequence":1,"event":{"type":"run_started","prompt":"hello"}}}
"#;
    assert!(matches!(
        decode_sse_frame(mismatched_cursor, "session-1", "run-1"),
        Err(DesktopClientError::InvalidEventStream)
    ));

    let wrong_run = br#"event: run_event
data: {"schema_version":2,"event_class":"transient","run_event":{"schema_version":1,"session_id":"session-1","run_id":"run-other","sequence":2,"event":{"type":"text_delta","text":"live"}}}
"#;
    assert!(matches!(
        decode_sse_frame(wrong_run, "session-1", "run-1"),
        Err(DesktopClientError::ProtocolEvent(
            DesktopProtocolEventError::WrongStream
        ))
    ));
}
