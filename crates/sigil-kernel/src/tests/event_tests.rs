use serde_json::json;

use crate::{
    AgentRole, ApprovalMode, BackgroundTaskHandle, ChangeSet, ChangeSetId, ChangeSetResult,
    ChangeSetResultStatus, ChangeSetRisk, CompactionRecord, ControlEntry, McpElicitationDecision,
    McpElicitationEntry, MemoryLoadReport, MemorySnapshot, ModelMessage,
    PUBLIC_RUN_EVENT_SCHEMA_VERSION, PrefixSnapshot, ProviderContinuationState, PublicControlEvent,
    PublicRunEvent, PublicRunEventKind, ResponseHandle, RunEvent, SessionRef, SkillDescriptor,
    SkillIndexSnapshot, SkillLoadEntry, SkillRunMode, SkillSource, SkillTrustState,
    TaskChildSessionEntry, TaskChildSessionStatus, TaskId, TaskPlanEntry, TaskPlanStatus,
    TaskRouteId, TaskRouteStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId,
    TaskStepStatus, TaskSubagentApprovalRouteEntry, TaskSubagentElicitationRouteEntry,
    TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus, ToolAccess,
    ToolApprovalAuditAction, ToolApprovalEntry, ToolCall, ToolCategory, ToolEgressEntry,
    ToolExecutionEntry, ToolExecutionStatus, ToolPreview, ToolPreviewCapability, ToolPreviewFile,
    ToolPreviewSnapshot, ToolResult, ToolResultMeta, ToolSpec, ToolSubject, UsageStats,
};

#[test]
fn public_run_event_serializes_stable_text_delta_envelope() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        7,
        RunEvent::TextDelta("hello".to_owned()),
    );

    let value = serde_json::to_value(event).expect("public run event should serialize");

    assert_eq!(value["schema_version"], PUBLIC_RUN_EVENT_SCHEMA_VERSION);
    assert_eq!(value["session_id"], "session-1");
    assert_eq!(value["run_id"], "run-1");
    assert_eq!(value["sequence"], 7);
    assert_eq!(value["event"]["type"], "text_delta");
    assert_eq!(value["event"]["text"], "hello");
}

#[test]
fn public_run_event_roundtrips_tool_call_args_delta() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        8,
        RunEvent::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: "{\"path\"".to_owned(),
        },
    );
    let value = serde_json::to_value(&event).expect("public run event should serialize");

    let roundtripped: PublicRunEvent =
        serde_json::from_value(value.clone()).expect("public run event should deserialize");
    let roundtripped_value =
        serde_json::to_value(roundtripped).expect("public run event should serialize again");

    assert_eq!(roundtripped_value, value);
    assert_eq!(roundtripped_value["event"]["type"], "tool_call_args_delta");
    assert_eq!(roundtripped_value["event"]["id"], "call-1");
}

#[test]
fn public_run_event_projects_approval_requested_details() {
    let call = ToolCall {
        id: "call-2".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{\"path\":\"README.md\"}".to_owned(),
    };
    let spec = ToolSpec {
        name: "read_file".to_owned(),
        description: "Read a file".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string"
                }
            }
        }),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        preview: ToolPreviewCapability::None,
    };
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        9,
        RunEvent::ToolApprovalRequested {
            call,
            spec,
            subjects: vec![ToolSubject::path("README.md", "README.md")],
            preview: None,
        },
    );

    let value = serde_json::to_value(event).expect("public run event should serialize");

    assert_eq!(value["event"]["type"], "approval_requested");
    assert_eq!(value["event"]["call"]["id"], "call-2");
    assert_eq!(value["event"]["call"]["name"], "read_file");
    assert_eq!(value["event"]["spec"]["category"], "file");
    assert_eq!(value["event"]["spec"]["access"], "read");
    assert_eq!(value["event"]["subjects"][0]["kind"], "path");
    assert_eq!(value["event"]["subjects"][0]["scope"], "workspace");
    assert!(value["event"]["preview"].is_null());
}

#[test]
fn public_run_event_projects_all_internal_run_event_variants() {
    let cases = vec![
        (
            RunEvent::ReasoningDelta("thinking".to_owned()),
            "reasoning_delta",
        ),
        (
            RunEvent::ToolCallStarted(tool_call("call-start")),
            "tool_call_started",
        ),
        (
            RunEvent::ToolCallCompleted(tool_call("call-complete")),
            "tool_call_completed",
        ),
        (
            RunEvent::ToolApprovalResolved {
                call_id: "call-approval".to_owned(),
                approved: true,
                reason: Some("ok".to_owned()),
            },
            "approval_resolved",
        ),
        (
            RunEvent::ToolResult(ToolResult::ok(
                "call-result",
                "read_file",
                "done",
                ToolResultMeta::default(),
            )),
            "tool_result",
        ),
        (RunEvent::Usage(UsageStats::default()), "usage"),
        (
            RunEvent::ContinuationState(continuation_state("cursor")),
            "continuation_state",
        ),
        (RunEvent::Notice("heads up".to_owned()), "notice"),
    ];

    for (index, (event, expected_type)) in cases.into_iter().enumerate() {
        let value = serde_json::to_value(PublicRunEvent::from_run_event(
            "session-1",
            "run-1",
            index as u64,
            event,
        ))
        .expect("public run event should serialize");

        assert_eq!(value["event"]["type"], expected_type);
    }
}

#[test]
fn public_run_event_supports_adapter_lifecycle_events() {
    let started = PublicRunEvent::new(
        "session-1",
        "run-1",
        1,
        PublicRunEventKind::RunStarted {
            prompt: "inspect workspace".to_owned(),
        },
    );
    let cancelled = PublicRunEvent::new("session-1", "run-1", 2, PublicRunEventKind::RunCancelled);

    let started_value = serde_json::to_value(started).expect("started event should serialize");
    let cancelled_value =
        serde_json::to_value(cancelled).expect("cancelled event should serialize");

    assert_eq!(started_value["event"]["type"], "run_started");
    assert_eq!(started_value["event"]["prompt"], "inspect workspace");
    assert_eq!(cancelled_value["event"]["type"], "run_cancelled");
}

#[test]
fn public_run_event_supports_task_lifecycle_events() {
    let started = PublicRunEvent::new(
        "session-1",
        "run-1",
        3,
        PublicRunEventKind::TaskRunStarted {
            task_id: "task-1".to_owned(),
            objective: "ship public events".to_owned(),
        },
    );
    let finished = PublicRunEvent::new(
        "session-1",
        "run-1",
        4,
        PublicRunEventKind::TaskRunFinished {
            task_id: "task-1".to_owned(),
            status: "completed".to_owned(),
        },
    );

    let started_value = serde_json::to_value(started).expect("task start event should serialize");
    let finished_value =
        serde_json::to_value(finished).expect("task finish event should serialize");

    assert_eq!(started_value["event"]["type"], "task_run_started");
    assert_eq!(started_value["event"]["task_id"], "task-1");
    assert_eq!(started_value["event"]["objective"], "ship public events");
    assert_eq!(finished_value["event"]["type"], "task_run_finished");
    assert_eq!(finished_value["event"]["task_id"], "task-1");
    assert_eq!(finished_value["event"]["status"], "completed");
}

#[test]
fn public_run_event_wraps_control_entries_behind_public_boundary() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        10,
        RunEvent::Control(ControlEntry::Note {
            kind: "diagnostic".to_owned(),
            data: json!({ "value": 1 }),
        }),
    );

    let value = serde_json::to_value(event).expect("control event should serialize");

    assert_eq!(value["event"]["type"], "control");
    assert_eq!(value["event"]["control"]["kind"], "note");
    assert!(value["event"]["control"]["payload"].is_object());
}

#[test]
fn public_control_event_kinds_cover_control_entry_variants() {
    let entries = vec![
        (
            ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-chat".to_owned(),
            },
            "session_identity",
        ),
        (
            ControlEntry::ContinuationStateSaved(continuation_state("saved")),
            "continuation_state_saved",
        ),
        (
            ControlEntry::ResponseHandleTracked(ResponseHandle {
                provider_name: "deepseek".to_owned(),
                response_id: "response-1".to_owned(),
                continuation_cursor: None,
            }),
            "response_handle_tracked",
        ),
        (
            ControlEntry::BackgroundTaskTracked(BackgroundTaskHandle {
                provider_name: "deepseek".to_owned(),
                task_id: "remote-task-1".to_owned(),
                resumable: true,
            }),
            "background_task_tracked",
        ),
        (
            ControlEntry::PrefixSnapshotCaptured(prefix_snapshot()),
            "prefix_snapshot_captured",
        ),
        (
            ControlEntry::MemorySnapshotCaptured(MemorySnapshot {
                messages: Vec::new(),
                report: MemoryLoadReport::default(),
            }),
            "memory_snapshot_captured",
        ),
        (
            ControlEntry::UsageSnapshot(UsageStats::default()),
            "usage_snapshot",
        ),
        (
            ControlEntry::ToolApproval(ToolApprovalEntry {
                action: ToolApprovalAuditAction::Requested,
                call_id: "call-approval".to_owned(),
                tool_name: "read_file".to_owned(),
                access: ToolAccess::Read,
                subjects: Vec::new(),
                policy_decision: ApprovalMode::Ask,
                external_directory_required: false,
                user_decision: None,
                reason: None,
                preview_hash: None,
            }),
            "tool_approval",
        ),
        (
            ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
                call_id: "call-execution".to_owned(),
                tool_name: "read_file".to_owned(),
                status: ToolExecutionStatus::Started,
                duration_ms: None,
                subjects: Vec::new(),
                changed_files: Vec::new(),
                metadata: ToolResultMeta::default(),
                error: None,
                model_content_hash: None,
            })),
            "tool_execution",
        ),
        (
            ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
                call_id: "call-egress".to_owned(),
                tool_name: "mcp__server__tool".to_owned(),
                destination: "server".to_owned(),
                operation: "call_tool".to_owned(),
                subjects: Vec::new(),
                payload: json!({ "redacted": true }),
                redacted: true,
            })),
            "tool_egress",
        ),
        (
            ControlEntry::McpElicitation(Box::new(McpElicitationEntry::new(
                "server",
                "continue?",
                &json!({ "type": "object" }),
                McpElicitationDecision::Declined,
                None,
            ))),
            "mcp_elicitation",
        ),
        (
            ControlEntry::ToolPreviewCaptured(tool_preview_snapshot()),
            "tool_preview_captured",
        ),
        (
            ControlEntry::SkillIndexCaptured(
                SkillIndexSnapshot::new(vec![skill_descriptor()])
                    .expect("valid skill index snapshot"),
            ),
            "skill_index_captured",
        ),
        (
            ControlEntry::SkillLoaded(SkillLoadEntry {
                skill_id: "repo-review".to_owned(),
                sha256: "hash".to_owned(),
                source: SkillSource::Workspace,
                entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
                run_id: Some("run-1".to_owned()),
                call_id: Some("call-1".to_owned()),
                byte_count: 128,
                line_count: 7,
                loaded_at_ms: 42,
            }),
            "skill_loaded",
        ),
        (
            ControlEntry::ChangeSetProposed(ChangeSet {
                id: ChangeSetId::new("change-1").expect("valid change set id"),
                title: "Update README".to_owned(),
                summary: "Update project overview".to_owned(),
                risk: ChangeSetRisk::Low,
                files: Vec::new(),
                validations: Vec::new(),
            }),
            "change_set_proposed",
        ),
        (
            ControlEntry::ChangeSetApplied(ChangeSetResult {
                id: ChangeSetId::new("change-1").expect("valid change set id"),
                status: ChangeSetResultStatus::Applied,
                file_results: Vec::new(),
                message: None,
            }),
            "change_set_applied",
        ),
        (
            ControlEntry::TerminalTask(TerminalTaskEntry {
                handle: TerminalTaskHandle {
                    task_id: TerminalTaskId::new("terminal-1").expect("valid terminal task id"),
                    command: "cargo test".to_owned(),
                    cwd: ".".into(),
                    shell: "zsh".to_owned(),
                    log_path: ".sigil/terminal/terminal-1/output.log".into(),
                    created_at_ms: 100,
                },
                status: TerminalTaskStatus::Running,
                output_preview: Some("running".to_owned()),
                output_hash: Some("sha256:abc".to_owned()),
                output_truncated: false,
                updated_at_ms: 120,
            }),
            "terminal_task",
        ),
        (
            ControlEntry::CompactionApplied(CompactionRecord {
                summary: "summary".to_owned(),
                compacted_message_count: 2,
                retained_tail_message_count: 1,
            }),
            "compaction_applied",
        ),
        (ControlEntry::TaskRun(task_run_entry()), "task_run"),
        (
            ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id(),
                plan_version: 1,
                status: TaskPlanStatus::Accepted,
                steps: Vec::new(),
                reason: None,
            }),
            "task_plan",
        ),
        (
            ControlEntry::TaskStep(TaskStepEntry {
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                role: AgentRole::Executor,
                status: TaskStepStatus::Running,
                title: Some("implement".to_owned()),
                summary: None,
                reason: None,
            }),
            "task_step",
        ),
        (
            ControlEntry::TaskChildSession(TaskChildSessionEntry {
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                child_task_id: TaskId::new("child-task").expect("valid task id"),
                child_session_ref: session_ref(),
                role: AgentRole::SubagentRead,
                status: TaskChildSessionStatus::Started,
                summary_hash: None,
            }),
            "task_child_session",
        ),
        (
            ControlEntry::TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry {
                route_id: route_id(),
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                role: AgentRole::SubagentWrite,
                child_session_ref: session_ref(),
                call_id: "call-child".to_owned(),
                tool_name: "write_file".to_owned(),
                status: TaskRouteStatus::Registered,
            }),
            "task_subagent_approval_route",
        ),
        (
            ControlEntry::TaskSubagentElicitationRoute(TaskSubagentElicitationRouteEntry {
                route_id: route_id(),
                task_id: task_id(),
                plan_version: 1,
                step_id: step_id(),
                role: AgentRole::SubagentRead,
                child_session_ref: session_ref(),
                server_name: "server".to_owned(),
                status: TaskRouteStatus::Requested,
            }),
            "task_subagent_elicitation_route",
        ),
        (
            ControlEntry::Note {
                kind: "diagnostic".to_owned(),
                data: json!({ "value": 1 }),
            },
            "note",
        ),
    ];

    for (entry, expected_kind) in entries {
        let control = PublicControlEvent::from(entry);

        assert_eq!(control.kind, expected_kind);
        assert!(control.payload.is_some());
    }
}

#[test]
fn public_run_event_projects_assistant_message_to_public_dto() {
    let event = PublicRunEvent::from_run_event(
        "session-1",
        "run-1",
        11,
        RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("done".to_owned()),
            vec![ToolCall {
                id: "call-3".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{}".to_owned(),
            }],
        )),
    );

    let value = serde_json::to_value(event).expect("assistant event should serialize");

    assert_eq!(value["event"]["type"], "assistant_message");
    assert_eq!(value["event"]["message"]["content"], "done");
    assert_eq!(value["event"]["message"]["tool_calls"][0]["id"], "call-3");
    assert!(value["event"]["message"]["role"].is_null());
    assert!(value["event"]["message"]["tool_call_id"].is_null());
}

fn tool_call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "read_file".to_owned(),
        args_json: "{\"path\":\"README.md\"}".to_owned(),
    }
}

fn continuation_state(state_kind: &str) -> ProviderContinuationState {
    ProviderContinuationState {
        provider_name: "deepseek".to_owned(),
        state_kind: state_kind.to_owned(),
        message_id: None,
        opaque_blob: json!({ "cursor": "cursor-1" }),
    }
}

fn prefix_snapshot() -> PrefixSnapshot {
    PrefixSnapshot {
        materialized_text: "system".to_owned(),
        sha256: "hash".to_owned(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-chat".to_owned(),
        memory_fingerprint: "memory".to_owned(),
        tool_schema_fingerprint: "tools".to_owned(),
        skill_index_fingerprint: "skills".to_owned(),
    }
}

fn skill_descriptor() -> SkillDescriptor {
    SkillDescriptor {
        id: "repo-review".to_owned(),
        name: "Repo Review".to_owned(),
        description: "Review repository changes".to_owned(),
        when_to_use: Some("Use for repository code review.".to_owned()),
        root: ".sigil/skills/repo-review".into(),
        entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
        source: SkillSource::Workspace,
        sha256: "hash".to_owned(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::Inline,
        argument_hint: None,
        allowed_tools: Default::default(),
        disallowed_tools: Default::default(),
        path_patterns: Vec::new(),
    }
}

fn tool_preview_snapshot() -> ToolPreviewSnapshot {
    ToolPreviewSnapshot::from_preview(
        "call-preview",
        "write_file",
        &ToolPreview {
            title: "Write file".to_owned(),
            summary: "Create file".to_owned(),
            body: "preview".to_owned(),
            changed_files: vec!["README.md".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "README.md".to_owned(),
                diff: "--- /dev/null\n+++ b/README.md\n@@ -0,0 +1 @@\n+hello".to_owned(),
            }],
        },
        Default::default(),
        Some("preview-hash".to_owned()),
    )
}

fn task_id() -> TaskId {
    TaskId::new("task-1").expect("valid task id")
}

fn step_id() -> TaskStepId {
    TaskStepId::new("step-1").expect("valid step id")
}

fn route_id() -> TaskRouteId {
    TaskRouteId::new("route-1").expect("valid route id")
}

fn session_ref() -> SessionRef {
    SessionRef::new_relative("child.jsonl").expect("valid session ref")
}

fn task_run_entry() -> TaskRunEntry {
    TaskRunEntry {
        task_id: task_id(),
        parent_session_ref: session_ref(),
        objective: "implement public events".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }
}
