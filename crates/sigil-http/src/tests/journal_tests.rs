use std::sync::Arc;

use sigil_kernel::resource::CanonicalHash;
use sigil_kernel::{
    ApprovalRequestIdentityV2, MAX_EVENT_BYTES, PublicRunEvent, PublicRunEventKind,
    PublicTaskPlanStep, TerminalLifecycleEvent, TerminalReadinessStatus, TerminalTaskId,
    TerminalTaskStatus, ToolApprovalSessionGrantUnavailableReason,
    ToolApprovalSessionGrantUnavailableReasonCode,
};

use super::*;
use crate::{HttpLiveEventBus, HttpPendingApproval, HttpProtocolEvent, HttpProtocolReplayError};

fn approval_identity() -> ApprovalRequestIdentityV2 {
    ApprovalRequestIdentityV2 {
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        call_id: "call-1".to_owned(),
        approval_request_id: "kernel-approval-v2:request-1".to_owned(),
        plan_hash: "a".repeat(64),
        policy_version: "permission-policy-v2".to_owned(),
        execution_binding_hash: "c".repeat(64),
        expires_at_ms: 10,
    }
}

fn unavailable_session_grant_reason() -> Option<ToolApprovalSessionGrantUnavailableReason> {
    Some(ToolApprovalSessionGrantUnavailableReason {
        code: ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
    })
}

fn durable_event(sequence: u64) -> HttpProtocolEvent {
    HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        sequence,
        if sequence == 1 {
            PublicRunEventKind::RunStarted {
                prompt: "hello".to_owned(),
            }
        } else {
            PublicRunEventKind::Notice {
                message: format!("event-{sequence}"),
            }
        },
    ))
    .expect("test event should have a durable cursor")
}

fn durable_event_for(session_id: &str, run_id: &str, sequence: u64) -> HttpProtocolEvent {
    HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        session_id,
        run_id,
        sequence,
        PublicRunEventKind::Notice {
            message: format!("event-{sequence}"),
        },
    ))
    .expect("test event should have a durable cursor")
}

fn managed_writer(
    temp: &tempfile::TempDir,
) -> Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Channel;
    let state = temp.path().join("state");
    let execution = temp.path().join("execution");
    std::fs::create_dir_all(state.join("cache")).expect("state roots");
    std::fs::create_dir_all(&execution).expect("execution root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
            .expect("state mode");
        std::fs::set_permissions(state.join("cache"), std::fs::Permissions::from_mode(0o700))
            .expect("cache mode");
        std::fs::set_permissions(&execution, std::fs::Permissions::from_mode(0o700))
            .expect("execution mode");
    }
    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let composition = sigil_runtime::r71_authority_composition::compose_runtime_authority(
        &state,
        &execution,
        CanonicalHash::from_bytes([0xa3; 32]),
        planner,
        &[Channel::AdapterDurableState],
    )
    .expect("managed protocol composition");
    composition.storage_writer
}

fn terminal_event(session_id: &str, run_id: &str) -> HttpProtocolEvent {
    HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        session_id,
        run_id,
        1,
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    ))
    .expect("terminal event should have a durable cursor")
}

fn terminal_lifecycle_event(sequence: u64, status: TerminalTaskStatus) -> HttpProtocolEvent {
    HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        sequence,
        PublicRunEventKind::TerminalLifecycle {
            event: TerminalLifecycleEvent {
                task_id: TerminalTaskId::new("terminal-1").expect("terminal task id"),
                execution_backend: None,
                sandbox_profile: None,
                generation: sequence,
                status,
                readiness: TerminalReadinessStatus::None,
                total_output_bytes: 0,
                emitted_at_ms: sequence,
            },
        },
    ))
    .expect("terminal lifecycle event should have a durable cursor")
}

fn approval_event() -> HttpProtocolEvent {
    let call = sigil_kernel::ToolCall {
        id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
    };
    let mut event = HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            approval_identity: approval_identity(),
            session_grant_available: false,
            session_grant_unavailable_reason: unavailable_session_grant_reason(),
            effects: Default::default(),
            analysis: sigil_kernel::ToolAnalysisStatus::Complete,
            containment: Default::default(),
            safe_summary: Default::default(),
            decision_reasons: Vec::new(),
            call,
            spec: sigil_kernel::ToolSpec {
                name: "write_file".to_owned(),
                description: "write a file".to_owned(),
                input_schema: serde_json::json!({"type":"object"}),
                category: sigil_kernel::ToolCategory::File,
                access: sigil_kernel::ToolAccess::Write,
                network_effect: None,
                preview: sigil_kernel::ToolPreviewCapability::Required,
            },
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: true,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    ))
    .expect("approval event should project");
    event.approval_request = Some(HttpPendingApproval {
        call_id: "call-1".to_owned(),
        tool_name: "write_file".to_owned(),
        approval_request_id: approval_identity().approval_request_id,
        tool_call_hash: "b".repeat(64),
        policy_version: approval_identity().policy_version,
        expires_at_ms: 10,
        session_grant_available: false,
        session_grant_unavailable_reason: unavailable_session_grant_reason(),
        display: crate::HttpPendingApprovalDisplay {
            event_sequence: 1,
            ..Default::default()
        },
    });
    event
}

#[test]
fn durable_journal_replays_after_process_reopen() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("protocol-journal.json");
    {
        let journal =
            HttpDurableProtocolJournal::open(&path, 16).expect("journal should initialize");
        journal
            .append(durable_event(1))
            .expect("start should persist");
        journal
            .append(durable_event(2))
            .expect("notice should persist");
    }

    let reopened =
        HttpDurableProtocolJournal::open(&path, 16).expect("journal should recover after reopen");
    let replay = reopened
        .replay_run_after(
            "session-1",
            "run-1",
            Some("sigil-http-run-v1:session-1:run-1:1"),
        )
        .expect("retained suffix should replay");

    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].run_event.sequence, 2);
}

#[test]
fn current_schema_protocol_replay_uses_managed_namespace_and_reopens_from_it() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let legacy_path = temp.path().join("legacy-protocol.json");
    let writer = managed_writer(&temp);
    let managed_path = writer
        .managed_named_leaf_path(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::AdapterDurableState,
            "http-protocol-replay",
        )
        .expect("managed protocol leaf")
        .join("records.jsonl");
    {
        let journal =
            HttpDurableProtocolJournal::open(&legacy_path, 16).expect("journal should initialize");
        journal
            .attach_managed_writer(Arc::clone(&writer), "http-protocol-replay")
            .expect("managed writer should attach");
        journal
            .append(durable_event(1))
            .expect("event should persist");
        assert!(
            !legacy_path.exists(),
            "new current-schema boot must not create legacy state"
        );
        assert!(
            managed_path.exists(),
            "managed protocol record should exist"
        );
    }

    let fresh_writer = managed_writer(&temp);
    let reopened = HttpDurableProtocolJournal::open(&legacy_path, 16)
        .expect("journal should reopen from managed state");
    reopened
        .attach_managed_writer(fresh_writer, "http-protocol-replay")
        .expect("managed writer should reattach");
    let replay = reopened
        .replay_run_after("session-1", "run-1", None)
        .expect("managed event should replay");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].run_event.sequence, 1);
}

#[test]
fn durable_journal_rejects_a_second_process_owner_for_the_same_path() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("protocol-journal.json");
    let first =
        HttpDurableProtocolJournal::open(&path, 16).expect("first journal owner should initialize");

    assert!(matches!(
        HttpDurableProtocolJournal::open(&path, 16),
        Err(HttpProtocolJournalError::Io { .. })
    ));
    drop(first);
    HttpDurableProtocolJournal::open(path, 16)
        .expect("journal lease should release with its owner");
}

#[test]
fn bounded_journal_accepts_the_exact_eviction_boundary_cursor() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("protocol-journal.json");
    let journal = HttpDurableProtocolJournal::open(path, 2).expect("journal should initialize");
    for sequence in 1..=3 {
        journal
            .append(durable_event(sequence))
            .expect("event should persist");
    }

    let replay = journal
        .replay_run_after(
            "session-1",
            "run-1",
            Some("sigil-http-run-v1:session-1:run-1:1"),
        )
        .expect("the exact eviction boundary still has a complete suffix");
    assert_eq!(
        replay
            .iter()
            .map(|event| event.run_event.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn bounded_journal_reports_a_cursor_older_than_the_eviction_boundary() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("protocol-journal.json");
    let journal = HttpDurableProtocolJournal::open(path, 2).expect("journal should initialize");
    for sequence in 1..=4 {
        journal
            .append(durable_event(sequence))
            .expect("event should persist");
    }

    assert_eq!(
        journal
            .replay_run_after(
                "session-1",
                "run-1",
                Some("sigil-http-run-v1:session-1:run-1:1"),
            )
            .expect_err("cursor older than the eviction boundary must fail"),
        HttpProtocolReplayError::CursorExpired
    );
}

#[test]
fn bounded_journal_rotates_completed_stream_watermarks() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("protocol-journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 2).expect("journal should initialize");
    for index in 1..=4 {
        journal
            .append(terminal_event("session-1", &format!("run-{index}")))
            .expect("completed streams should rotate through bounded retention");
    }

    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("journal should remain readable"))
            .expect("journal should remain valid JSON");
    assert_eq!(persisted["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        persisted["high_watermarks"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn bounded_journal_rejects_more_concurrent_active_streams_without_mutating_state() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = HttpDurableProtocolJournal::open(temp.path().join("journal.json"), 2)
        .expect("journal should initialize");
    for run_id in ["run-1", "run-2"] {
        journal
            .append(
                HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                    "session-1",
                    run_id,
                    1,
                    PublicRunEventKind::RunStarted {
                        prompt: "hello".to_owned(),
                    },
                ))
                .expect("start event should be valid"),
            )
            .expect("active stream should fit");
    }
    let third = HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-3",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    ))
    .expect("start event should be valid");

    assert_eq!(
        journal
            .append(third)
            .expect_err("third active stream must exceed capacity"),
        HttpProtocolJournalError::StreamCapacity
    );
    assert_eq!(
        journal
            .replay_run_after("session-1", "run-1", None)
            .expect("rejected append must leave old state intact")
            .len(),
        1
    );
}

#[test]
fn restart_seals_and_rotates_orphaned_nonterminal_streams() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    {
        let journal =
            HttpDurableProtocolJournal::open(&path, 2).expect("journal should initialize");
        for run_id in ["run-1", "run-2"] {
            journal
                .append(
                    HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                        "session-1",
                        run_id,
                        1,
                        PublicRunEventKind::RunStarted {
                            prompt: "hello".to_owned(),
                        },
                    ))
                    .expect("start event should project"),
                )
                .expect("active stream should persist");
        }
    }

    let journal = HttpDurableProtocolJournal::open(&path, 2)
        .expect("restart should seal prior process streams");
    assert!(matches!(
        journal.append(durable_event_for("session-1", "run-1", 2)),
        Err(HttpProtocolJournalError::StreamAlreadyTerminal { .. })
    ));
    for run_id in ["run-3", "run-4"] {
        journal
            .append(
                HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                    "session-1",
                    run_id,
                    1,
                    PublicRunEventKind::RunStarted {
                        prompt: "hello".to_owned(),
                    },
                ))
                .expect("start event should project"),
            )
            .expect("new streams should rotate orphaned watermarks");
    }
}

#[test]
fn durable_journal_atomically_closes_a_retained_foreground_terminal_stream() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    journal
        .append(
            HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                "session-1",
                "run-1",
                1,
                PublicRunEventKind::RunStarted {
                    prompt: "start terminal task".to_owned(),
                },
            ))
            .expect("run start should project"),
        )
        .expect("run start should persist");
    journal
        .append_with_stream_continuation(
            HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                "session-1",
                "run-1",
                2,
                PublicRunEventKind::RunFinished {
                    final_text: "terminal remains active".to_owned(),
                },
            ))
            .expect("foreground terminal should project"),
            true,
        )
        .expect("foreground terminal should retain the stream");
    journal
        .append_and_close_stream(terminal_lifecycle_event(
            3,
            TerminalTaskStatus::Exited { exit_code: Some(0) },
        ))
        .expect("final lifecycle and close should commit together");
    assert!(matches!(
        journal.append(durable_event_for("session-1", "run-1", 4)),
        Err(HttpProtocolJournalError::StreamAlreadyTerminal { .. })
    ));
    assert_eq!(
        journal
            .replay_run_after("session-1", "run-1", None)
            .expect("retained stream should replay")
            .len(),
        3
    );
    drop(journal);

    let reopened =
        HttpDurableProtocolJournal::open(&path, 8).expect("closed stream should recover safely");
    assert_eq!(
        reopened
            .latest_run_sequence("session-1", "run-1")
            .expect("latest sequence should read"),
        Some(3)
    );
    assert!(matches!(
        reopened.append(durable_event_for("session-1", "run-1", 4)),
        Err(HttpProtocolJournalError::StreamAlreadyTerminal { .. })
    ));
}

#[test]
fn durable_journal_never_persists_exact_prompt_or_final_secret_carriers() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    for (sequence, event) in [
        (
            1,
            PublicRunEventKind::RunStarted {
                prompt: "use https://example.test/search?token=super-secret".to_owned(),
            },
        ),
        (
            2,
            PublicRunEventKind::RunFinished {
                final_text: "result token=super-secret".to_owned(),
            },
        ),
    ] {
        journal
            .append(
                HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                    "session-1",
                    "run-1",
                    sequence,
                    event,
                ))
                .expect("event should project"),
            )
            .expect("safe event should persist");
    }

    let persisted = std::fs::read_to_string(path).expect("journal should remain readable");
    assert!(!persisted.contains("super-secret"));
    assert!(!persisted.contains("?token="));
    assert!(persisted.contains("[redacted]"));
}

#[test]
fn public_journal_append_reapplies_canonical_safe_projection() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    let mut event = HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "safe".to_owned(),
        },
    ))
    .expect("event should project");
    if let PublicRunEventKind::RunStarted { prompt } = &mut event.run_event.event {
        *prompt = "prompt token=raw-bypass-secret".to_owned();
    }

    journal
        .append(event)
        .expect("journal boundary should canonicalize public envelopes");

    let persisted = std::fs::read_to_string(path).expect("journal should remain readable");
    assert!(!persisted.contains("raw-bypass-secret"));
    assert!(persisted.contains("token=[redacted]"));
}

#[test]
fn public_journal_rejects_a_forged_live_provisional_identity() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    let mut event = durable_event(1);
    event.provisional_id = Some(format!("live-v1:{}", "0".repeat(64)));

    assert!(matches!(
        journal.append(event),
        Err(HttpProtocolJournalError::Corrupt { .. })
    ));
    assert!(
        journal
            .replay_run_after("session-1", "run-1", None)
            .expect("replay should remain readable")
            .is_empty()
    );
}

#[test]
fn journal_reopen_rejects_a_noncanonical_safe_persistence_bypass() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    {
        let journal =
            HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
        journal
            .append(durable_event(1))
            .expect("safe event should persist");
    }
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("journal should remain readable"))
            .expect("journal should remain valid JSON");
    persisted["events"][0]["run_event"]["event"]["prompt"] =
        serde_json::Value::String("prompt token=raw-reopen-secret".to_owned());
    std::fs::write(
        &path,
        serde_json::to_vec(&persisted).expect("tampered fixture should serialize"),
    )
    .expect("tampered fixture should write");

    assert!(matches!(
        HttpDurableProtocolJournal::open(path, 8),
        Err(HttpProtocolJournalError::Corrupt { .. })
    ));
}

#[test]
fn journal_reopen_rejects_a_forged_stream_high_watermark() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    {
        let journal =
            HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
        journal
            .append(durable_event(1))
            .expect("event should persist");
    }
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("journal should remain readable"))
            .expect("journal should parse");
    persisted["high_watermarks"][0]["latest_sequence"] = serde_json::Value::from(7_u64);
    std::fs::write(
        &path,
        serde_json::to_vec(&persisted).expect("tampered fixture should serialize"),
    )
    .expect("tampered fixture should write");

    assert!(matches!(
        HttpDurableProtocolJournal::open(path, 8),
        Err(HttpProtocolJournalError::Corrupt { .. })
    ));
}

#[test]
fn journal_append_and_reopen_reject_malformed_approval_guard_material() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    {
        let journal =
            HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
        let mut malformed = approval_event();
        malformed
            .approval_request
            .as_mut()
            .expect("guard should exist")
            .tool_call_hash = "token=raw-secret".to_owned();
        assert!(matches!(
            journal.append(malformed),
            Err(HttpProtocolJournalError::Corrupt { .. })
        ));
        assert!(
            journal
                .replay_run_after("session-1", "run-1", None)
                .expect("replay should remain valid")
                .is_empty()
        );
        journal
            .append(approval_event())
            .expect("canonical guard should persist");
    }
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("journal should remain readable"))
            .expect("journal should parse");
    persisted["events"][0]["approval_request"]["approval_request_id"] =
        serde_json::Value::String("http-approval-v1:not-a-digest".to_owned());
    std::fs::write(
        &path,
        serde_json::to_vec(&persisted).expect("tampered fixture should serialize"),
    )
    .expect("tampered fixture should write");

    assert!(matches!(
        HttpDurableProtocolJournal::open(path, 8),
        Err(HttpProtocolJournalError::Corrupt { .. })
    ));
}

#[test]
fn durable_journal_omits_opaque_provider_and_control_payloads_and_sanitizes_tool_results() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    let events = [
        PublicRunEventKind::ContinuationState {
            state: sigil_kernel::ProviderContinuationState {
                provider_name: "deepseek".to_owned(),
                state_kind: "reasoning".to_owned(),
                message_id: Some("message-1".to_owned()),
                opaque_blob: serde_json::json!({
                    "reasoning_content": "private-reasoning-blob",
                    "api_key": "super-secret",
                }),
            },
        },
        PublicRunEventKind::Control {
            control: sigil_kernel::PublicControlEvent {
                kind: "continuation_state_saved".to_owned(),
                payload: Some(serde_json::json!({
                    "opaque": "private-control-blob",
                })),
            },
        },
        PublicRunEventKind::ToolResult {
            result: sigil_kernel::ToolResult::ok(
                "call-1",
                "read_file",
                "tool result token=super-secret",
                sigil_kernel::ToolResultMeta {
                    details: serde_json::json!({"api_key": "super-secret"}),
                    ..sigil_kernel::ToolResultMeta::default()
                },
            ),
        },
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    ];
    for (index, event) in events.into_iter().enumerate() {
        journal
            .append(
                HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                    "session-1",
                    "run-1",
                    (index + 1) as u64,
                    event,
                ))
                .expect("event should project"),
            )
            .expect("safe event should persist");
    }

    let persisted = std::fs::read_to_string(path).expect("journal should remain readable");
    assert!(!persisted.contains("private-reasoning-blob"));
    assert!(!persisted.contains("private-control-blob"));
    assert!(!persisted.contains("super-secret"));
    assert!(persisted.contains("omitted_from_http_durable_event"));
    assert!(persisted.contains("[redacted]"));
}

#[test]
fn durable_journal_keeps_typed_task_events_without_secret_capable_fields() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    let event = PublicRunEventKind::TaskPlanUpdated {
        task_id: "task-public".to_owned(),
        plan_version: 2,
        status: "accepted".to_owned(),
        steps: vec![PublicTaskPlanStep {
            step_id: "step-public".to_owned(),
            title: "inspect token=private-task-secret".to_owned(),
            role: "executor".to_owned(),
            depends_on: Vec::new(),
            mode: "write".to_owned(),
            isolation: "sequential_workspace_write".to_owned(),
        }],
    };
    journal
        .append(
            HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                "session-1",
                "run-task",
                1,
                event,
            ))
            .expect("typed task event should project"),
        )
        .expect("typed task event should persist");

    let persisted = std::fs::read_to_string(path).expect("journal should remain readable");
    assert!(persisted.contains("task_plan_updated"));
    assert!(persisted.contains("step-public"));
    assert!(!persisted.contains("private-task-secret"));
    assert!(persisted.contains("[redacted]"));
    assert!(!persisted.contains("private_ref"));
    assert!(!persisted.contains("workspace_path"));
}

#[test]
fn durable_approval_projection_sanitizes_subjects_and_file_diffs() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let journal = HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize");
    let event = PublicRunEventKind::ApprovalRequested {
        approval_identity: approval_identity(),
        session_grant_available: false,
        session_grant_unavailable_reason: unavailable_session_grant_reason(),
        effects: Default::default(),
        analysis: sigil_kernel::ToolAnalysisStatus::Conservative {
            reasons: vec![sigil_kernel::ToolAnalysisReason::new(
                sigil_kernel::ToolAnalysisReasonCode::UnsupportedSyntax,
                Some("token=private-analysis-secret".to_owned()),
            )],
        },
        containment: Default::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary {
            title: "write token=private-plan-title-secret".to_owned(),
            detail: "detail token=private-plan-detail-secret".to_owned(),
            step_count: 1,
            workspace_code_steps: 0,
        },
        decision_reasons: vec![sigil_kernel::PermissionDecisionReason {
            source: sigil_kernel::PermissionDecisionSource::UserRule,
            code: "token=private-reason-code-secret".to_owned(),
            detail: "token=private-reason-detail-secret".to_owned(),
        }],
        call: sigil_kernel::ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: serde_json::json!({
                "path": "secret.txt",
                "token": "private-argument-secret",
            })
            .to_string(),
        },
        spec: sigil_kernel::ToolSpec {
            name: "write_file".to_owned(),
            description: "write token=private-description-secret".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "api_key": "private-schema-secret",
            }),
            category: sigil_kernel::ToolCategory::File,
            access: sigil_kernel::ToolAccess::Write,
            network_effect: None,
            preview: sigil_kernel::ToolPreviewCapability::Required,
        },
        subjects: vec![sigil_kernel::ToolSubject::command(
            "deploy --token private-command-secret",
            "deploy --token private-command-secret",
        )],
        network_effect: None,
        local_policy_decision: None,
        network_policy_decision: None,
        source_policy_decision: None,
        operation: None,
        risk: None,
        subject_zones: Vec::new(),
        confirmation: Some(sigil_kernel::PermissionConfirmation::TypePhrase {
            phrase: "confirm token=private-confirmation-secret".to_owned(),
        }),
        snapshot_required: true,
        command_permission_matches: vec![sigil_kernel::CommandPermissionMatch {
            group: sigil_kernel::CommandPermissionGroup::Ask,
            pattern: "deploy * token=private-pattern-secret".to_owned(),
            command: "deploy --token private-match-secret".to_owned(),
        }],
        preview: Some(sigil_kernel::ToolPreview {
            title: "write token=private-title-secret".to_owned(),
            summary: "summary token=private-summary-secret".to_owned(),
            body: "body token=private-body-secret".to_owned(),
            changed_files: vec!["file.txt?token=private-path-secret".to_owned()],
            file_diffs: vec![sigil_kernel::ToolPreviewFile {
                path: "file.txt?token=private-diff-path-secret".to_owned(),
                diff: "+token=private-diff-secret".to_owned(),
            }],
        }),
    };
    journal
        .append(
            HttpProtocolEvent::from_run_event(PublicRunEvent::new("session-1", "run-1", 1, event))
                .expect("approval should project"),
        )
        .expect("safe approval should persist");

    let persisted = std::fs::read_to_string(path).expect("journal should remain readable");
    for secret in [
        "private-argument-secret",
        "private-description-secret",
        "private-schema-secret",
        "private-command-secret",
        "private-confirmation-secret",
        "private-pattern-secret",
        "private-match-secret",
        "private-title-secret",
        "private-summary-secret",
        "private-body-secret",
        "private-path-secret",
        "private-diff-path-secret",
        "private-diff-secret",
        "private-analysis-secret",
        "private-plan-title-secret",
        "private-plan-detail-secret",
        "private-reason-code-secret",
        "private-reason-detail-secret",
    ] {
        assert!(!persisted.contains(secret), "journal leaked {secret}");
    }
    assert!(persisted.contains("[redacted]"));
}

#[test]
fn durable_journal_rejects_an_oversized_single_event() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = HttpDurableProtocolJournal::open(temp.path().join("journal.json"), 8)
        .expect("journal should initialize");
    let event = HttpProtocolEvent::from_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunFinished {
            final_text: "x".repeat(MAX_EVENT_BYTES + 1),
        },
    ))
    .expect("protocol projection should remain separate from durable size admission");

    assert!(matches!(
        journal.append(event),
        Err(HttpProtocolJournalError::EventTooLarge { .. })
    ));
}

#[test]
fn durable_journal_rejects_invalid_capacity_and_oversized_input_files() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    assert!(matches!(
        HttpDurableProtocolJournal::open(temp.path().join("zero.json"), 0),
        Err(HttpProtocolJournalError::InvalidCapacity { .. })
    ));
    assert!(matches!(
        HttpDurableProtocolJournal::open(
            temp.path().join("too-many.json"),
            MAX_HTTP_PROTOCOL_JOURNAL_EVENTS + 1,
        ),
        Err(HttpProtocolJournalError::InvalidCapacity { .. })
    ));

    let oversized = temp.path().join("oversized.json");
    std::fs::write(&oversized, vec![b' '; MAX_HTTP_PROTOCOL_JOURNAL_BYTES + 1])
        .expect("oversized fixture should write");
    assert!(matches!(
        HttpDurableProtocolJournal::open(oversized, 8),
        Err(HttpProtocolJournalError::JournalTooLarge { .. })
    ));
}

#[test]
fn protocol_persistence_rejects_a_candidate_above_the_total_file_boundary() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let mut state = HttpProtocolJournalState::default();
    for sequence in 1..=20 {
        state
            .append(
                HttpProtocolEvent::from_run_event(PublicRunEvent::new(
                    "session-1",
                    "run-1",
                    sequence,
                    PublicRunEventKind::Notice {
                        message: "x".repeat(850_000),
                    },
                ))
                .expect("event should project"),
                false,
            )
            .expect("individual event should remain below its boundary");
    }

    assert!(matches!(
        persist_state(&temp.path().join("journal.json"), &state),
        Err(HttpProtocolJournalError::JournalTooLarge { .. })
    ));
    assert!(!temp.path().join("journal.json").exists());
}

#[test]
fn journal_rejects_non_monotonic_republication_without_mutating_replay() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = HttpDurableProtocolJournal::open(temp.path().join("journal.json"), 8)
        .expect("journal should initialize");
    journal
        .append(durable_event(1))
        .expect("first event should persist");

    assert!(matches!(
        journal.append(durable_event(1)),
        Err(HttpProtocolJournalError::NonMonotonicSequence { .. })
    ));
    assert_eq!(
        journal
            .replay_run_after("session-1", "run-1", None)
            .expect("replay should remain valid")
            .len(),
        1
    );
}

#[test]
fn durable_live_bus_persists_before_broadcast_and_recovers_with_a_new_bus() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("journal.json");
    let first_journal =
        Arc::new(HttpDurableProtocolJournal::open(&path, 8).expect("journal should initialize"));
    let first_bus = HttpLiveEventBus::with_durable_journal(8, first_journal);
    first_bus
        .publish_run_event(durable_event(1).run_event)
        .expect("durable event should publish");
    first_bus
        .publish_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            2,
            PublicRunEventKind::RunFinished {
                final_text: "done".to_owned(),
            },
        ))
        .expect("terminal event should publish");
    assert_eq!(first_bus.active_sequence_watermark_len(), 0);
    drop(first_bus);

    let second_journal =
        Arc::new(HttpDurableProtocolJournal::open(&path, 8).expect("journal should reopen"));
    let second_bus = HttpLiveEventBus::with_durable_journal(8, second_journal);

    assert_eq!(
        second_bus
            .replay_run_after("session-1", "run-1", None)
            .expect("new bus should use durable replay")
            .len(),
        2
    );
    assert_eq!(
        second_bus
            .latest_run_sequence("session-1", "run-1")
            .expect("new bus should recover the durable watermark"),
        Some(2)
    );
}

#[test]
fn durable_live_bus_never_accumulates_a_second_in_memory_history() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("journal.json"), 8)
            .expect("journal should initialize"),
    );
    let bus = HttpLiveEventBus::with_durable_journal(4, journal);
    for sequence in 1..=128 {
        bus.publish_run_event(PublicRunEvent::new(
            "session-1",
            "run-1",
            sequence,
            PublicRunEventKind::TextDelta {
                text: format!("delta-{sequence}"),
            },
        ))
        .expect("transient event should use only bounded live fan-out");
    }
    bus.publish_run_event(PublicRunEvent::new(
        "session-1",
        "run-1",
        129,
        PublicRunEventKind::Notice {
            message: "durable".to_owned(),
        },
    ))
    .expect("durable event should persist");

    assert_eq!(bus.synthetic_buffer_len(), 0);
    assert_eq!(
        bus.replay_run_after("session-1", "run-1", None)
            .expect("durable journal should replay")
            .len(),
        1
    );
}
