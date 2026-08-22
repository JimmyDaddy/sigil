use serde_json::json;

use super::*;

fn envelope(event_class: DesktopProtocolEventClass, event: Value) -> DesktopProtocolEvent {
    DesktopProtocolEvent {
        schema_version: DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION,
        event_class,
        replay_id: (event_class == DesktopProtocolEventClass::Durable)
            .then(|| "sigil-http-run-v1:session-1:run-1:1".to_owned()),
        approval_request: None,
        provisional_id: None,
        run_event: DesktopPublicRunEvent {
            schema_version: DESKTOP_PUBLIC_RUN_EVENT_SCHEMA_VERSION,
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            sequence: 1,
            event: serde_json::from_value(event).expect("public event should deserialize"),
        },
    }
}

#[test]
fn timeline_projection_keeps_conversation_text_and_drops_raw_tool_arguments() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "tool_call_started",
            "call": {
                "id": "call-1",
                "name": "write_file",
                "args_json": "{\"path\":\"/private/secret\"}"
            }
        }),
    );

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("event should project");
    let serialized = serde_json::to_string(&timeline).expect("timeline should serialize");

    assert_eq!(timeline.kind, DesktopTimelineEventKind::ToolStarted);
    assert_eq!(timeline.tool_name.as_deref(), Some("write_file"));
    assert_eq!(timeline.item_id.as_deref(), Some("call-1"));
    assert!(!serialized.contains("private"));
    assert!(!serialized.contains("args_json"));
}

#[test]
fn provider_turn_recovery_projects_typed_actions_without_attempt_identity() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "provider_turn_recovery_changed",
            "recovery": {
                "phase": "paused",
                "active_retry_count": 2,
                "active_max_retries": 2,
                "retry_count": 2,
                "max_transport_retries": 2,
                "reason_code": "provider_retry_budget_exhausted",
                "available_actions": ["retry_now", "cancel"],
                "user_attention_required": true
            }
        }),
    );

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("provider recovery should project");
    assert_eq!(
        timeline.kind,
        DesktopTimelineEventKind::ProviderTurnRecovery
    );
    assert_eq!(timeline.status.as_deref(), Some("paused"));
    let recovery = timeline
        .provider_turn_recovery
        .as_ref()
        .expect("typed provider recovery is forwarded");
    assert_eq!(recovery.retry_count, 2);
    assert_eq!(recovery.active_max_retries, 2);
    assert_eq!(
        recovery.available_actions,
        vec![
            DesktopProviderTurnRecoveryAction::RetryNow,
            DesktopProviderTurnRecoveryAction::Cancel,
        ]
    );
    let encoded = serde_json::to_string(&timeline).expect("timeline serializes");
    assert!(!encoded.contains("physical_attempt"));
}

#[test]
fn discarded_partial_output_projects_without_content_or_attempt_identity() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "provider_turn_partial_output_discarded",
            "output": {
                "text_discarded": true,
                "reasoning_discarded": true,
                "tool_request_discarded": false
            }
        }),
    );

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("discard signal should project");
    assert_eq!(
        timeline.kind,
        DesktopTimelineEventKind::ProviderTurnPartialOutputDiscarded
    );
    assert_eq!(timeline.status.as_deref(), Some("discarded"));
    let serialized = serde_json::to_string(&timeline).expect("timeline serializes");
    assert!(serialized.contains("incomplete response"));
    assert!(!serialized.contains("physical_attempt"));
    assert!(!serialized.contains("discarded text"));
}

#[test]
fn timeline_projection_keeps_allowlisted_shell_command_and_assistant_kind() {
    let shell = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "tool_call_started",
            "call": {
                "id": "call-1",
                "name": "bash",
                "args_json": "{\"command\":\"rg TODO crates\"}"
            }
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("shell event should project");
    assert_eq!(shell.tool_input.as_deref(), Some("rg TODO crates"));

    let assistant = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "assistant_message",
            "message": {
                "id": "message-1",
                "content": "Done",
                "assistant_kind": "final_answer"
            }
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("assistant event should project");
    assert_eq!(assistant.assistant_kind.as_deref(), Some("final_answer"));

    let credentialed = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "tool_call_started",
            "call": {
                "id": "call-2",
                "name": "bash",
                "args_json": "{\"command\":\"TOKEN=secret-value curl example.com\"}"
            }
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("credential-shaped command should project safely");
    assert_eq!(
        credentialed.tool_input.as_deref(),
        Some("[credential-shaped command arguments redacted]")
    );
}

#[test]
fn tool_result_projection_keeps_bounded_safe_output_for_the_tool_card() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "tool_result",
            "result": {
                "call_id": "call-1",
                "tool_name": "read_file",
                "content": "{\"content\":\"file body\",\"status\":\"ok\"}",
                "status": "ok"
            }
        }),
    );

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("tool result should project");

    assert_eq!(timeline.kind, DesktopTimelineEventKind::ToolResult);
    assert_eq!(timeline.tool_name.as_deref(), Some("read_file"));
    assert_eq!(
        timeline.text.as_deref(),
        Some("{\"content\":\"file body\",\"status\":\"ok\"}")
    );
    assert_eq!(timeline.status.as_deref(), Some("ok"));
}

#[test]
fn approval_resolution_and_tool_execution_keep_exact_typed_identity() {
    let resolved = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "approval_resolved",
            "call_id": "call-1",
            "approval_request_id": "approval-1",
            "approved": true
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("approval resolution should project");
    assert_eq!(resolved.approval_request_id.as_deref(), Some("approval-1"));

    let execution = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "control",
            "control": {
                "kind": "tool_execution",
                "payload": {
                    "tool_execution": {
                        "call_id": "call-1",
                        "tool_name": "bash",
                        "status": "started"
                    }
                }
            }
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("tool execution should project");
    assert_eq!(
        execution.tool_execution,
        Some(crate::DesktopTimelineToolExecution {
            call_id: "call-1".to_owned(),
            tool_name: "bash".to_owned(),
            status: "started".to_owned(),
        })
    );
}

#[test]
fn redacted_durable_tool_execution_control_does_not_block_later_replay() {
    let execution = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "control",
            "control": {
                "kind": "tool_execution"
            }
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("redacted durable control should remain replayable");

    assert_eq!(execution.kind, DesktopTimelineEventKind::Control);
    assert_eq!(execution.item_id.as_deref(), Some("tool_execution"));
    assert_eq!(execution.tool_execution, None);
}

#[test]
fn approval_projection_requires_exact_guard_and_bounds_preview() {
    let mut event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "approval_requested",
            "call": {"id": "call-1", "name": "write_file", "args_json": "{}"},
            "session_grant_available": false,
            "session_grant_unavailable_reason": {"code": "operation_not_grantable"},
            "effects": ["file_write"],
            "analysis": {"status": "complete"},
            "containment": {
                "filesystem": "workspace_write",
                "network": "deny",
                "process": "owned_tree",
                "environment": "restricted",
                "persistent_process": false
            },
            "safe_summary": {"title": "Edit file", "detail": "Writes one workspace file"},
            "decision_reasons": [{"source": "local_policy", "code": "workspace_write", "detail": "Workspace writes require review"}],
            "subjects": [{"normalized": "workspace:README.md"}],
            "operation": "edit_file",
            "risk": "medium",
            "snapshot_required": true,
            "preview": {"title": "Edit file", "summary": "One change", "body": "diff"}
        }),
    );
    event.approval_request = Some(DesktopPendingApproval {
        call_id: "call-1".to_owned(),
        tool_name: "write_file".to_owned(),
        approval_request_id: "request-1".to_owned(),
        tool_call_hash: "hash-1".to_owned(),
        policy_version: "policy-1".to_owned(),
        expires_at_ms: 42,
        session_grant_available: false,
        session_grant_unavailable_reason: Some(crate::DesktopSessionGrantUnavailableReason {
            code: crate::DesktopSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        display: crate::DesktopPendingApprovalDisplay {
            event_sequence: 1,
            effects: vec!["file_write".to_owned()],
            subjects: vec![crate::DesktopPendingApprovalSubject {
                kind: "path".to_owned(),
                scope: "workspace".to_owned(),
                workspace_label: Some("README.md".to_owned()),
            }],
            analysis_status: "complete".to_owned(),
            analysis_reason_codes: Vec::new(),
            analysis_reasons: Vec::new(),
            containment: vec!["network=deny".to_owned()],
            decision_reasons: vec!["Workspace writes require review".to_owned()],
            safe_summary_title: "Edit file".to_owned(),
            safe_summary_detail: "Writes one workspace file".to_owned(),
            operation: Some("edit_file".to_owned()),
            risk: Some("medium".to_owned()),
            snapshot_required: true,
        },
    });

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("approval should project");
    let approval = timeline.approval.expect("approval guard should remain");
    assert_eq!(approval.tool_name, "write_file");
    assert_eq!(approval.operation.as_deref(), Some("edit_file"));
    assert_eq!(approval.effects, vec!["file_write"]);
    assert_eq!(approval.subjects, vec!["workspace:README.md"]);
    assert_eq!(approval.analysis_status, "complete");
    assert!(approval.containment.contains(&"network=deny".to_owned()));
    assert_eq!(approval.safe_summary_detail, "Writes one workspace file");
    assert_eq!(
        approval
            .session_grant_unavailable_reason
            .map(|reason| reason.code),
        Some(crate::DesktopSessionGrantUnavailableReasonCode::OperationNotGrantable)
    );
    assert_eq!(
        approval.decision_reasons,
        vec!["Workspace writes require review"]
    );
    assert!(approval.snapshot_required);
    assert_eq!(approval.preview_body.as_deref(), Some("diff"));
}

#[test]
fn protocol_projection_rejects_wrong_stream_and_invalid_replay_shape() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({"type": "run_started", "prompt": "hello"}),
    );
    assert_eq!(
        event
            .clone()
            .into_timeline("workspace-1", "session-other", "run-1", "http-session-1"),
        Err(DesktopProtocolEventError::WrongStream)
    );

    let mut transient = event;
    transient.event_class = DesktopProtocolEventClass::Transient;
    assert_eq!(
        transient.into_timeline("workspace-1", "session-1", "run-1", "http-session-1"),
        Err(DesktopProtocolEventError::InvalidReplayCursor)
    );
}

#[test]
fn timeline_projection_preserves_exact_sequence_and_opaque_provisional_identity() {
    let mut event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({"type": "run_started", "prompt": "hello"}),
    );
    event.run_event.sequence = 9_007_199_254_740_993;
    event.provisional_id = Some(format!("live-v1:{}", "a".repeat(64)));

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
        .expect("event should project");
    let expected_provisional_id = format!("live-v1:{}", "a".repeat(64));
    assert_eq!(timeline.run_sequence, "9007199254740993");
    assert_eq!(
        timeline.provisional_id.as_deref(),
        Some(expected_provisional_id.as_str())
    );

    let mut invalid = envelope(
        DesktopProtocolEventClass::Durable,
        json!({"type": "run_started", "prompt": "hello"}),
    );
    invalid.provisional_id = Some("live-v1:not-hex".to_owned());
    assert_eq!(
        invalid.into_timeline("workspace-1", "session-1", "run-1", "http-session-1"),
        Err(DesktopProtocolEventError::InvalidProvisionalIdentity)
    );
}

#[test]
fn task_events_project_exact_typed_identity_without_private_execution_material() {
    let plan = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "task_plan_updated",
            "task_id": "task-1",
            "plan_version": 7,
            "status": "approved",
            "steps": [{
                "step_id": "step-1",
                "title": "Inspect routing",
                "role": "explorer",
                "depends_on": [],
                "mode": "read_only",
                "isolation": "shared"
            }]
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("task plan should project");

    assert_eq!(plan.kind, DesktopTimelineEventKind::TaskPlanUpdated);
    let task = plan.task.expect("typed task projection");
    assert_eq!(task.task_id.as_deref(), Some("task-1"));
    assert_eq!(task.plan_version, Some(7));
    assert_eq!(task.steps[0].step_id, "step-1");
    let serialized = serde_json::to_string(&task).expect("task projection should serialize");
    for private_field in [
        "prompt",
        "worktree",
        "workspace_path",
        "integration_ref",
        "mutation_authority",
    ] {
        assert!(!serialized.contains(private_field));
    }
}

#[test]
fn integration_event_projects_lane_identity_conflicts_and_plan_version() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "integration_lane_changed",
            "task_id": "task-1",
            "plan_version": 3,
            "plan_id": "plan-3",
            "lane_id": "lane-1",
            "status": "conflicted",
            "conflicts": ["src/lib.rs"]
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("integration event should project");

    assert_eq!(event.kind, DesktopTimelineEventKind::IntegrationLaneChanged);
    assert_eq!(event.status.as_deref(), Some("conflicted"));
    let task = event.task.expect("typed integration projection");
    assert_eq!(task.plan_version, Some(3));
    assert_eq!(task.plan_id.as_deref(), Some("plan-3"));
    assert_eq!(task.lane_id.as_deref(), Some("lane-1"));
    assert_eq!(task.conflicts, vec!["src/lib.rs"]);
}

#[test]
fn unknown_future_event_type_degrades_without_forwarding_opaque_payload() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "future_public_event",
            "secret_payload": "must-not-reach-renderer"
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("unknown event should remain forward compatible");

    assert_eq!(event.kind, DesktopTimelineEventKind::Other);
    let serialized = serde_json::to_string(&event).expect("timeline should serialize");
    assert!(!serialized.contains("secret_payload"));
    assert!(!serialized.contains("must-not-reach-renderer"));
}

#[test]
fn terminal_lifecycle_projects_bounded_renderer_facts() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "terminal_lifecycle",
            "event": {
                "task_id": "terminal-1",
                "generation": 3,
                "status": {"state": "exited", "exit_code": 0},
                "readiness": {
                    "state": "ready",
                    "kind": "output_contains",
                    "ready_at_ms": 42
                },
                "total_output_bytes": 128,
                "emitted_at_ms": 43
            }
        }),
    )
    .into_timeline("workspace-1", "session-1", "run-1", "http-session-1")
    .expect("terminal lifecycle should project");

    assert_eq!(event.kind, DesktopTimelineEventKind::TerminalLifecycle);
    assert_eq!(event.item_id.as_deref(), Some("terminal-1"));
    let task = event
        .terminal_task
        .expect("terminal facts should be present");
    assert_eq!(task.task_id, "terminal-1");
    assert_eq!(task.generation, 3);
    assert_eq!(task.status, "exited");
    assert_eq!(task.exit_code, Some(0));
    assert_eq!(task.readiness, "ready");
    assert_eq!(task.readiness_kind.as_deref(), Some("output_contains"));
    assert_eq!(task.ready_at_ms, Some(42));
    assert_eq!(task.total_output_bytes, 128);
}

#[test]
fn every_current_public_event_variant_deserializes_without_opaque_event_parsing() {
    let events = [
        json!({"type": "run_started", "prompt": "prompt"}),
        json!({"type": "run_awaiting_user_input", "request_id": "request-1", "generation": 1, "request_hash": format!("sha256:{}", "a".repeat(64))}),
        json!({"type": "task_run_started", "task_id": "task-1", "objective": "objective"}),
        json!({"type": "run_finished", "final_text": "done"}),
        json!({"type": "task_run_finished", "task_id": "task-1", "status": "completed"}),
        json!({"type": "task_routing_changed", "handoff_id": "handoff-1", "status": "accepted", "task_id": "task-1"}),
        json!({"type": "conversation_route_changed", "decision_id": "route-1", "route": "plan_review", "status": "accepted"}),
        json!({"type": "plan_review_changed", "plan_review_id": "review-1", "plan_id": "plan-1", "status": "waiting_for_input"}),
        json!({
            "type": "user_input_changed",
            "request_id": "request-1",
            "generation": 1,
            "request_hash": format!("sha256:{}", "a".repeat(64)),
            "status": "requested",
            "request": {
                "identity": {
                    "session_scope_id": "session-1",
                    "root_logical_run_id": "run-1",
                    "source_thread_id": "main",
                    "request_id": "request-1",
                    "generation": 1,
                    "source_binding_hash": format!("sha256:{}", "b".repeat(64))
                },
                "request_hash": format!("sha256:{}", "a".repeat(64)),
                "source": "agent",
                "purpose": "clarification",
                "prompt": "Choose a workspace",
                "questions": [{
                    "id": "workspace",
                    "header": "Workspace",
                    "question": "Which workspace?",
                    "required": true,
                    "field": {"kind": "text", "multiline": false, "max_chars": 512}
                }],
                "allowed_actions": ["submit", "decline", "cancel_run"],
                "requested_at_unix_ms": 1,
                "status": "requested"
            }
        }),
        json!({"type": "task_phase_changed", "task_id": "task-1", "phase": "execution", "status": "running"}),
        json!({"type": "task_plan_updated", "task_id": "task-1", "plan_version": 1, "status": "approved", "steps": []}),
        json!({"type": "task_batch_changed", "task_id": "task-1", "plan_version": 1, "batch_id": "batch-1", "active": 1, "completed": 0, "failed": 0}),
        json!({"type": "task_step_changed", "task_id": "task-1", "plan_version": 1, "step_id": "step-1", "attempt_id": "attempt-1", "status": "running"}),
        json!({"type": "integration_lane_changed", "task_id": "task-1", "plan_version": 1, "plan_id": "plan-1", "lane_id": "lane-1", "status": "running", "conflicts": []}),
        json!({"type": "provider_turn_recovery_changed", "recovery": {"phase": "waiting", "active_retry_count": 1, "active_max_retries": 2, "retry_count": 1, "max_transport_retries": 2, "available_actions": [], "user_attention_required": false}}),
        json!({"type": "provider_turn_partial_output_discarded", "output": {"text_discarded": true, "reasoning_discarded": false, "tool_request_discarded": false}}),
        json!({"type": "run_failed", "error": "failed"}),
        json!({"type": "route_recovery_required", "code": "session_route_confirmation_required", "actions": ["repair_connection", "start_new_session"], "recovery_binding": "binding-1", "retryable": true}),
        json!({"type": "run_cancelled"}),
        json!({"type": "text_delta", "text": "text"}),
        json!({"type": "reasoning_delta", "text": "reasoning"}),
        json!({"type": "tool_call_started", "call": {"id": "call-1", "name": "bash", "args_json": "{}"}}),
        json!({"type": "tool_call_args_delta", "id": "call-1", "delta": "{}"}),
        json!({"type": "tool_call_completed", "call": {"id": "call-1", "name": "bash", "args_json": "{}"}}),
        json!({
            "type": "approval_requested",
            "call": {"id": "call-1", "name": "bash", "args_json": "{}"},
            "session_grant_available": false,
            "session_grant_unavailable_reason": {"code": "operation_not_grantable"},
            "effects": ["execute_workspace_code"],
            "analysis": {"status": "complete"},
            "containment": {
                "filesystem": "workspace_write",
                "network": "deny",
                "process": "owned_tree",
                "environment": "restricted",
                "persistent_process": false
            },
            "safe_summary": {"title": "Run validation", "detail": "Runs workspace code"},
            "decision_reasons": [],
            "subjects": [],
            "snapshot_required": false
        }),
        json!({"type": "approval_resolved", "call_id": "call-1", "approval_request_id": "approval-1", "approved": false, "reason": null}),
        json!({"type": "tool_result", "result": {"call_id": "call-1", "tool_name": "bash", "content": "failed", "status": {"error": {"kind": "internal", "message": "failed", "retryable": false}}, "metadata": {}}}),
        json!({"type": "tool_progress", "progress": {"execution_id": "execution-1", "call_id": "call-1", "tool_name": "bash", "sequence": 1, "status": "running", "details": {}}}),
        json!({"type": "terminal_lifecycle", "event": {"task_id": "terminal-1", "generation": 1, "status": {"state": "running"}, "readiness": {"state": "waiting", "kind": "output_contains"}, "total_output_bytes": 0, "emitted_at_ms": 1}}),
        json!({"type": "usage", "usage": {"prompt_tokens": 1}}),
        json!({"type": "continuation_state", "state": {"provider_name": "provider"}}),
        json!({"type": "control", "control": {"kind": "task_status_changed", "payload": {"private": "ignored"}}}),
        json!({"type": "assistant_message", "message": {"id": "message-1", "content": "done", "tool_calls": [], "assistant_kind": "final_answer"}}),
        json!({"type": "notice", "message": "notice"}),
    ];

    for event in events {
        serde_json::from_value::<DesktopPublicRunEventKind>(event)
            .expect("known public event should deserialize");
    }
}

#[test]
fn route_recovery_event_projects_bounded_renderer_actions() {
    let event = envelope(
        DesktopProtocolEventClass::Durable,
        json!({
            "type": "route_recovery_required",
            "code": "session_route_selection_required",
            "actions": ["select_replacement", "start_new_session", "back_to_session_library"],
            "recovery_binding": "route-binding-1",
            "retryable": true
        }),
    );

    let timeline = event
        .into_timeline("workspace-1", "session-1", "run-1", "session-1")
        .expect("route recovery event should project");

    assert_eq!(
        timeline.kind,
        DesktopTimelineEventKind::RouteRecoveryRequired
    );
    assert_eq!(timeline.status.as_deref(), Some("recovery_required"));
    let recovery = timeline.route_recovery.expect("typed route recovery");
    assert_eq!(
        recovery.code,
        DesktopRouteRecoveryCode::SessionRouteSelectionRequired
    );
    assert_eq!(
        recovery.actions,
        vec![
            DesktopRouteRecoveryAction::SelectReplacement,
            DesktopRouteRecoveryAction::StartNewSession,
            DesktopRouteRecoveryAction::BackToSessionLibrary,
        ]
    );
    assert_eq!(recovery.recovery_binding, "route-binding-1");
    assert!(recovery.retryable);
}
