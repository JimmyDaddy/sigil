use std::{io::Cursor, path::PathBuf};

use super::*;
use crate::app::tests::common::test_config;
use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentProfileCapturedEntry, AgentProfileId, AgentProfilePolicyEntry, AgentProfileSnapshot,
    AgentProfileSnapshotId, AgentProfileSource, AgentProfileTrustEntry, AgentTrustState,
    ApprovalMode, CompactionConfig, CompactionRecord, DurableEventType, JsonlSessionStore,
    McpElicitationDecision, McpElicitationEntry, MemoryConfig, PlanApprovalExpiry,
    PlanApprovalPermission, PluginCapability, PluginManifestSnapshot, PluginTrustDecision,
    PluginTrustEntry, SessionStreamRecord, SkillDescriptor, SkillIndexSnapshot, SkillLoadEntry,
    SkillRunMode, SkillSource, SkillTrustState, ToolApprovalAuditAction, ToolApprovalEntry,
    ToolApprovalUserDecision, ToolError, ToolErrorKind, ToolResultMeta, WorkspaceConfig,
};

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
fn local_ui_control_preservation_keeps_only_display_overrides() -> Result<()> {
    let thread_id = sigil_kernel::AgentThreadId::new("thread_preserve")?;
    let task_id = sigil_kernel::TaskId::new("task_preserve")?;
    let step_id = sigil_kernel::TaskStepId::new("step_preserve")?;
    let local_entries = vec![
        SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
            sigil_kernel::AgentThreadClosedEntry {
                thread_id: thread_id.clone(),
                reason: Some("closed".to_owned()),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(
            sigil_kernel::AgentThreadDisplayNameEntry {
                thread_id: thread_id.clone(),
                display_name: "Reader".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(
            sigil_kernel::TaskChildSessionDisplayNameEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                child_task_id: sigil_kernel::TaskId::new("child_preserve")?,
                display_name: "Legacy Reader".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: sigil_kernel::AgentThreadStatus::Running,
                reason: None,
                updated_at_ms: None,
            },
        )),
    ];
    let incoming_entries = vec![
        SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
            sigil_kernel::AgentThreadClosedEntry {
                thread_id: thread_id.clone(),
                reason: Some("closed".to_owned()),
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "task".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
    ];

    let merged = preserve_local_ui_control_entries(&local_entries, incoming_entries);
    let closed_count = merged
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentThreadClosed(_))
            )
        })
        .count();
    assert_eq!(closed_count, 1);
    assert!(merged.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(rename))
                if rename.display_name == "Reader"
        )
    }));
    assert!(merged.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(rename))
                if rename.step_id == step_id && rename.display_name == "Legacy Reader"
        )
    }));
    assert!(!merged.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(status))
                if status.thread_id == thread_id
        )
    }));
    Ok(())
}

#[test]
fn local_ui_control_entry_equality_covers_task_child_display_identity() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_equal")?;
    let step_id = sigil_kernel::TaskStepId::new("step_equal")?;
    let child_task_id = sigil_kernel::TaskId::new("child_equal")?;
    let entry = SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(
        sigil_kernel::TaskChildSessionDisplayNameEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            child_task_id: child_task_id.clone(),
            display_name: "Reader".to_owned(),
        },
    ));
    let matching = SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(
        sigil_kernel::TaskChildSessionDisplayNameEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            child_task_id: child_task_id.clone(),
            display_name: "Reader".to_owned(),
        },
    ));
    let different = SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(
        sigil_kernel::TaskChildSessionDisplayNameEntry {
            task_id,
            plan_version: 1,
            step_id,
            child_task_id,
            display_name: "Writer".to_owned(),
        },
    ));

    assert!(local_ui_control_entries_equal(&entry, &matching));
    assert!(!local_ui_control_entries_equal(&entry, &different));
    Ok(())
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
    let tool_call_with_content = ModelMessage::assistant(
        Some("checking provider shape".to_owned()),
        vec![sigil_kernel::ToolCall {
            id: "call-2".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        }],
    );
    assert_eq!(
        render_model_message_line(&tool_call_with_content),
        "[assistant] checking provider shape tool_calls [read_file]"
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
fn render_task_control_entries_and_status_labels() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let route_id = sigil_kernel::TaskRouteId::new("route_1")?;
    let child_ref = sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;

    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Started),
        "started"
    );
    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Running),
        "running"
    );
    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Paused),
        "paused"
    );
    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Completed),
        "completed"
    );
    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Failed),
        "failed"
    );
    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        task_run_status_label(sigil_kernel::TaskRunStatus::Interrupted),
        "interrupted"
    );

    assert_eq!(
        task_plan_status_label(sigil_kernel::TaskPlanStatus::Proposed),
        "proposed"
    );
    assert_eq!(
        task_plan_status_label(sigil_kernel::TaskPlanStatus::Accepted),
        "accepted"
    );
    assert_eq!(
        task_plan_status_label(sigil_kernel::TaskPlanStatus::Superseded),
        "superseded"
    );
    assert_eq!(
        task_plan_status_label(sigil_kernel::TaskPlanStatus::Rejected),
        "rejected"
    );

    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Pending),
        "pending"
    );
    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Running),
        "running"
    );
    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Completed),
        "completed"
    );
    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Failed),
        "failed"
    );
    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Blocked),
        "blocked"
    );
    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        task_step_status_label(sigil_kernel::TaskStepStatus::Interrupted),
        "interrupted"
    );

    assert_eq!(
        task_child_session_status_label(sigil_kernel::TaskChildSessionStatus::Started),
        "started"
    );
    assert_eq!(
        task_child_session_status_label(sigil_kernel::TaskChildSessionStatus::Completed),
        "completed"
    );
    assert_eq!(
        task_child_session_status_label(sigil_kernel::TaskChildSessionStatus::Failed),
        "failed"
    );
    assert_eq!(
        task_child_session_status_label(sigil_kernel::TaskChildSessionStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        task_child_session_status_label(sigil_kernel::TaskChildSessionStatus::Interrupted),
        "interrupted"
    );
    assert_eq!(
        task_child_session_status_label(sigil_kernel::TaskChildSessionStatus::Unavailable),
        "unavailable"
    );

    assert_eq!(
        task_route_status_label(sigil_kernel::TaskRouteStatus::Registered),
        "registered"
    );
    assert_eq!(
        task_route_status_label(sigil_kernel::TaskRouteStatus::Requested),
        "requested"
    );
    assert_eq!(
        task_route_status_label(sigil_kernel::TaskRouteStatus::Resolved),
        "resolved"
    );
    assert_eq!(
        task_route_status_label(sigil_kernel::TaskRouteStatus::Rejected),
        "rejected"
    );
    assert_eq!(
        task_route_status_label(sigil_kernel::TaskRouteStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        task_route_status_label(sigil_kernel::TaskRouteStatus::Stale),
        "stale"
    );

    let rendered = [
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::PlanApproved(
            sigil_kernel::PlanApprovedEntry {
                plan_version: 1,
                plan_hash: sigil_kernel::plan_text_hash("inspect then edit"),
                approved_at_ms: 42,
                permission: sigil_kernel::PlanApprovalPermission::WorkspaceEdits,
                scope: sigil_kernel::PlanApprovalScope {
                    summary: "edit workspace files described by the plan".to_owned(),
                    workspace_paths: vec!["crates/sigil-tui".to_owned()],
                },
                expires: sigil_kernel::PlanApprovalExpiry::NextUserPrompt,
                clear_planning_context: true,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::TaskRun(
            sigil_kernel::TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                objective: "ship task".to_owned(),
                status: sigil_kernel::TaskRunStatus::Running,
                reason: None,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::TaskPlan(
            sigil_kernel::TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                status: sigil_kernel::TaskPlanStatus::Accepted,
                steps: vec![sigil_kernel::TaskStepSpec {
                    step_id: step_id.clone(),
                    title: "inspect".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                }],
                reason: None,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::TaskStep(
            sigil_kernel::TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                role: sigil_kernel::AgentRole::Executor,
                status: sigil_kernel::TaskStepStatus::Running,
                title: Some("inspect".to_owned()),
                summary: None,
                reason: None,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: child_ref.clone(),
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::TaskChildSessionDisplayName(
                sigil_kernel::TaskChildSessionDisplayNameEntry {
                    task_id: task_id.clone(),
                    plan_version: 1,
                    step_id: step_id.clone(),
                    child_task_id: sigil_kernel::TaskId::new("child_1")?,
                    display_name: "Repository Reader".to_owned(),
                },
            ),
        )),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::TaskSubagentApprovalRoute(sigil_kernel::TaskSubagentApprovalRouteEntry {
                route_id: route_id.clone(),
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                role: sigil_kernel::AgentRole::SubagentWrite,
                child_session_ref: child_ref.clone(),
                call_id: "call-1".to_owned(),
                tool_name: "write_file".to_owned(),
                status: sigil_kernel::TaskRouteStatus::Requested,
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::TaskSubagentElicitationRoute(
                sigil_kernel::TaskSubagentElicitationRouteEntry {
                    route_id,
                    task_id,
                    plan_version: 1,
                    step_id,
                    role: sigil_kernel::AgentRole::SubagentRead,
                    child_session_ref: child_ref,
                    server_name: "mcp".to_owned(),
                    status: sigil_kernel::TaskRouteStatus::Resolved,
                },
            ),
        )),
    ]
    .join("\n");

    assert!(
        rendered.contains("[ctl] plan approved v1 permission=workspace_edits expires=next_user")
    );
    assert!(rendered.contains("[ctl] task task_1 status=running"));
    assert!(rendered.contains("[ctl] plan task_1 v1 status=accepted steps=1"));
    assert!(rendered.contains("[ctl] step task_1 v1:step_1 status=running"));
    assert!(rendered.contains("[ctl] child task_1 v1:step_1 status=started"));
    assert!(rendered.contains("[ctl] child name child_1 v1:step_1 Repository Reader"));
    assert!(rendered.contains("[ctl] subagent approval route_1 call=call-1 status=requested"));
    assert!(rendered.contains("[ctl] subagent elicitation route_1 server=mcp status=resolved"));
    Ok(())
}

#[test]
fn render_agent_profile_control_entries_and_plan_approval_labels() -> Result<()> {
    let profile_id = AgentProfileId::new("review")?;
    let snapshot = AgentProfileSnapshot {
        snapshot_id: AgentProfileSnapshotId::new("snapshot_1")?,
        profile_id: profile_id.clone(),
        source: AgentProfileSource::Workspace,
        source_hash: "sha256:source".to_owned(),
        profile_hash: "sha256:profile-hash-long".to_owned(),
        resolved_tool_scope_hash: "sha256:tools".to_owned(),
        resolved_permission_policy_hash: "sha256:permissions".to_owned(),
        resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
        resolved_skill_hashes: vec!["sha256:skill".to_owned()],
        trust_state: AgentTrustState::NeedsReview,
    };

    let captured = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
            snapshot: snapshot.clone(),
        }),
    ));
    assert_eq!(captured, "[ctl] agent profile review trust=needs_review");

    let trusted = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::AgentProfileTrustDecision(AgentProfileTrustEntry {
            profile_id: profile_id.clone(),
            source: AgentProfileSource::Workspace,
            source_hash: "sha256:source".to_owned(),
            profile_hash: "sha256:profile-hash-long".to_owned(),
            decision: AgentTrustState::Trusted,
            reviewed_at_ms: 42,
        }),
    ));
    assert!(trusted.contains("trust=trusted"));
    assert!(trusted.contains("hash=sha256:profile"));

    let policy = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::AgentProfilePolicyDecision(AgentProfilePolicyEntry {
            profile_id,
            source: AgentProfileSource::Workspace,
            source_hash: "sha256:source".to_owned(),
            profile_hash: "sha256:profile-hash-long".to_owned(),
            enabled: Some(true),
            user_invocable: Some(false),
            model_invocable: None,
            reviewed_at_ms: 43,
        }),
    ));
    assert!(policy.contains("enabled=yes user=no model=inherit"));

    assert_eq!(
        plan_approval_permission_label(PlanApprovalPermission::Ask),
        "ask"
    );
    assert_eq!(
        plan_approval_permission_label(PlanApprovalPermission::WorkspaceEdits),
        "workspace_edits"
    );
    assert_eq!(
        plan_approval_expiry_label(&PlanApprovalExpiry::NextUserPrompt),
        "next_user_prompt"
    );
    assert_eq!(
        plan_approval_expiry_label(&PlanApprovalExpiry::Session),
        "session"
    );
    assert_eq!(
        plan_approval_expiry_label(&PlanApprovalExpiry::AtUnixMs(123)),
        "at_unix_ms"
    );
    assert_eq!(
        agent_trust_state_label(AgentTrustState::Disabled),
        "disabled"
    );
    assert_eq!(agent_trust_state_label(AgentTrustState::Unknown), "unknown");
    Ok(())
}

#[test]
fn render_agent_control_entries_and_status_labels() -> Result<()> {
    assert_eq!(
        agent_trust_state_label(sigil_kernel::AgentTrustState::Trusted),
        "trusted"
    );
    assert_eq!(
        agent_trust_state_label(sigil_kernel::AgentTrustState::NeedsReview),
        "needs_review"
    );
    assert_eq!(
        agent_trust_state_label(sigil_kernel::AgentTrustState::Disabled),
        "disabled"
    );
    assert_eq!(
        agent_trust_state_label(sigil_kernel::AgentTrustState::Unknown),
        "unknown"
    );

    assert_eq!(
        agent_invocation_mode_label(sigil_kernel::AgentInvocationMode::Foreground),
        "foreground"
    );
    assert_eq!(
        agent_invocation_mode_label(sigil_kernel::AgentInvocationMode::Background),
        "background"
    );
    assert_eq!(
        agent_invocation_mode_label(sigil_kernel::AgentInvocationMode::JoinBeforeFinal),
        "join_before_final"
    );
    assert_eq!(
        agent_invocation_mode_label(sigil_kernel::AgentInvocationMode::Unknown),
        "unknown"
    );

    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Started),
        "started"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Running),
        "running"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Blocked),
        "blocked"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Completed),
        "completed"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Failed),
        "failed"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Interrupted),
        "interrupted"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Closed),
        "closed"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Unavailable),
        "unavailable"
    );
    assert_eq!(
        agent_thread_status_label(sigil_kernel::AgentThreadStatus::Unknown),
        "unknown"
    );

    assert_eq!(
        agent_terminal_status_label(sigil_kernel::AgentThreadTerminalStatus::Completed),
        "completed"
    );
    assert_eq!(
        agent_terminal_status_label(sigil_kernel::AgentThreadTerminalStatus::Failed),
        "failed"
    );
    assert_eq!(
        agent_terminal_status_label(sigil_kernel::AgentThreadTerminalStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        agent_terminal_status_label(sigil_kernel::AgentThreadTerminalStatus::Interrupted),
        "interrupted"
    );
    assert_eq!(
        agent_terminal_status_label(sigil_kernel::AgentThreadTerminalStatus::Unknown),
        "unknown"
    );

    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Registered),
        "registered"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Requested),
        "requested"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Resolved),
        "resolved"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Rejected),
        "rejected"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Stale),
        "stale"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Closed),
        "closed"
    );
    assert_eq!(
        agent_route_status_label(sigil_kernel::AgentRouteStatus::Unknown),
        "unknown"
    );

    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_1")?;
    let thread_id = sigil_kernel::AgentThreadId::new("thread_1")?;
    let parent_thread_id = sigil_kernel::AgentThreadId::new("main")?;
    let attempt_id = sigil_kernel::AgentRunAttemptId::new("attempt_1")?;
    let route_id = sigil_kernel::AgentRouteId::new("route_1")?;
    let session_ref = sigil_kernel::SessionRef::new_relative("children/thread_1.jsonl")?;
    let run_context = sigil_kernel::AgentRunContextSnapshot {
        profile_snapshot_id: snapshot_id.clone(),
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning_effort: None,
        workspace_root: sigil_kernel::WorkspaceRootSnapshot::new("/workspace")?,
        effective_tool_scope_hash: "sha256:tools".to_owned(),
        effective_permission_policy_hash: "sha256:permissions".to_owned(),
        effective_mcp_scope_hash: "sha256:mcp".to_owned(),
        provider_capability_hash: "sha256:provider".to_owned(),
        model_visible_agent_index_hash: None,
        budget_policy_hash: "sha256:budget".to_owned(),
        provider_background_handle_ref: None,
    };
    let result = sigil_kernel::AgentThreadResult {
        thread_id: thread_id.clone(),
        session_ref: session_ref.clone(),
        status: sigil_kernel::AgentThreadTerminalStatus::Completed,
        summary: "done".to_owned(),
        summary_truncated: false,
        original_summary_chars: None,
        artifacts: Vec::new(),
        changed_paths: Vec::new(),
        risks: Vec::new(),
        followups: Vec::new(),
        usage: None,
        output_hash: "sha256:result".to_owned(),
        final_answer_ref: None,
    };

    let rendered = [
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentProfileCaptured(sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::Workspace,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(parent_thread_id.clone()),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: session_ref.clone(),
                profile_id: profile_id.clone(),
                profile_snapshot_id: snapshot_id.clone(),
                run_context,
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Foreground,
                invocation_source: sigil_kernel::AgentInvocationSource::Chat,
                display_name: Some("kernel map".to_owned()),
                created_at_ms: Some(42),
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentThreadStatusChanged(sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: sigil_kernel::AgentThreadStatus::Running,
                reason: None,
                updated_at_ms: Some(43),
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentThreadMessageRouted(sigil_kernel::AgentThreadMessageRoutedEntry {
                route_id: route_id.clone(),
                source_thread_id: parent_thread_id.clone(),
                target_thread_id: thread_id.clone(),
                prompt_hash: "sha256:steer".to_owned(),
                prompt: None,
                status: sigil_kernel::AgentRouteStatus::Resolved,
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentThreadResultRecorded(sigil_kernel::AgentThreadResultRecordedEntry {
                result,
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentThreadDisplayName(sigil_kernel::AgentThreadDisplayNameEntry {
                thread_id: thread_id.clone(),
                display_name: "kernel map".to_owned(),
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(
            sigil_kernel::AgentApprovalRouteEntry {
                route_id: route_id.clone(),
                source_thread_id: thread_id.clone(),
                target_thread_id: Some(parent_thread_id.clone()),
                call_id: "call-1".to_owned(),
                tool_name: "read_file".to_owned(),
                status: sigil_kernel::AgentRouteStatus::Requested,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentElicitationRoute(sigil_kernel::AgentElicitationRouteEntry {
                route_id: route_id.clone(),
                source_thread_id: thread_id.clone(),
                target_thread_id: Some(parent_thread_id.clone()),
                server_name: "filesystem".to_owned(),
                status: sigil_kernel::AgentRouteStatus::Registered,
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentRunAttemptStarted(sigil_kernel::AgentRunAttemptStartedEntry {
                thread_id: thread_id.clone(),
                attempt_id: attempt_id.clone(),
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                background: true,
                provider_background_handle_ref: Some("handle".to_owned()),
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::AgentRunHeartbeat(
            sigil_kernel::AgentRunHeartbeatEntry {
                thread_id: thread_id.clone(),
                attempt_id: attempt_id.clone(),
                updated_at_ms: 44,
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentRunInterrupted(sigil_kernel::AgentRunInterruptedEntry {
                thread_id: thread_id.clone(),
                attempt_id,
                reason: "restore".to_owned(),
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::AgentRouteClosed(
            sigil_kernel::AgentRouteClosedEntry {
                route_id: route_id.clone(),
                reason: "restore".to_owned(),
            },
        ))),
        render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::AgentMergeSafePoint(sigil_kernel::AgentMergeSafePointEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: parent_thread_id.clone(),
                result_hash: "sha256:result".to_owned(),
            }),
        )),
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
            sigil_kernel::AgentThreadClosedEntry {
                thread_id,
                reason: Some("archived".to_owned()),
            },
        ))),
    ]
    .join("\n");

    assert!(rendered.contains("[ctl] agent profile explore trust=trusted"));
    assert!(rendered.contains("[ctl] agent thread_1 started profile=explore mode=foreground"));
    assert!(rendered.contains("[ctl] agent thread_1 status=running"));
    assert!(rendered.contains("[ctl] agent message route_1 status=resolved"));
    assert!(rendered.contains("[ctl] agent result thread_1 status=completed"));
    assert!(rendered.contains("[ctl] agent name thread_1 kernel map"));
    assert!(rendered.contains("[ctl] agent approval route_1 call=call-1 status=requested"));
    assert!(
        rendered.contains("[ctl] agent elicitation route_1 server=filesystem status=registered")
    );
    assert!(
        rendered.contains("[ctl] agent attempt attempt_1 thread=thread_1 model=deepseek-v4-pro")
    );
    assert!(rendered.contains("[ctl] agent heartbeat attempt_1 thread=thread_1 at=44"));
    assert!(rendered.contains("[ctl] agent interrupted attempt_1 thread=thread_1"));
    assert!(rendered.contains("[ctl] agent route route_1 closed"));
    assert!(rendered.contains("[ctl] agent merge thread_1 parent=main"));
    assert!(rendered.contains("[ctl] agent thread_1 closed"));
    Ok(())
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
            .any(|line: &String| line.contains("[user] before"))
    );
    assert!(
        preview_lines
            .iter()
            .any(|line: &String| line.contains("[assistant] after"))
    );
}

#[test]
fn restored_timeline_entries_project_all_visible_session_entry_kinds() -> Result<()> {
    let app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("child prompt")),
        SessionLogEntry::Assistant(ModelMessage::assistant(Some(String::new()), Vec::new())),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("child answer".to_owned()),
            Vec::new(),
        )),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("checking provider shape".to_owned()),
            vec![sigil_kernel::ToolCall {
                id: "call-tool".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{}".to_owned(),
            }],
        )),
        SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "tool output")),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_delta".to_owned(),
            data: json!({ "delta": "think 1\n" }),
        }),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_delta".to_owned(),
            data: json!({ "delta": "" }),
        }),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_trace".to_owned(),
            data: json!({ "text": "think 2" }),
        }),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-2".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Failed,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: Some(ToolError {
                kind: ToolErrorKind::ExitStatus,
                message: "command failed".to_owned(),
                retryable: false,
                details: serde_json::Value::Null,
            }),
            model_content_hash: None,
        }))),
        SessionLogEntry::Control(ControlEntry::TerminalTask(
            sigil_kernel::TerminalTaskEntry {
                handle: sigil_kernel::TerminalTaskHandle {
                    task_id: sigil_kernel::TerminalTaskId::new("terminal-1")?,
                    command: "cargo test".to_owned(),
                    cwd: std::path::PathBuf::from("."),
                    shell: "sh".to_owned(),
                    log_path: std::path::PathBuf::from(".sigil/tasks/terminal-1/output.log"),
                    created_at_ms: 1,
                },
                status: sigil_kernel::TerminalTaskStatus::Running,
                output_preview: Some("running output".to_owned()),
                output_hash: Some("hash".to_owned()),
                output_truncated: false,
                updated_at_ms: 2,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: sigil_kernel::AgentThreadId::new("agent_restore_1")?,
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/agents/agent_restore_1.jsonl",
                )?,
                profile_id: sigil_kernel::AgentProfileId::new("explore")?,
                profile_snapshot_id: sigil_kernel::AgentProfileSnapshotId::new(
                    "snapshot_restore_1",
                )?,
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: sigil_kernel::AgentProfileSnapshotId::new(
                        "snapshot_restore_1",
                    )?,
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                    effective_tool_scope_hash: "tools".to_owned(),
                    effective_permission_policy_hash: "permissions".to_owned(),
                    effective_mcp_scope_hash: "mcp".to_owned(),
                    provider_capability_hash: "provider".to_owned(),
                    model_visible_agent_index_hash: Some("agent-index".to_owned()),
                    budget_policy_hash: "budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::JoinBeforeFinal,
                invocation_source: sigil_kernel::AgentInvocationSource::Mention,
                display_name: Some("kernel-explorer".to_owned()),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id: sigil_kernel::AgentThreadId::new("agent_restore_1")?,
                status: sigil_kernel::AgentThreadStatus::Running,
                reason: Some("waiting for result".to_owned()),
                updated_at_ms: Some(43),
            },
        )),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "other".to_owned(),
            data: json!({}),
        }),
    ];

    let restored = app.restored_timeline_entries_from_session_entries(&entries);
    let rendered = restored
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        restored
            .iter()
            .any(|entry| entry.role == TimelineRole::User)
    );
    assert!(rendered.contains("child prompt"));
    assert!(rendered.contains("child answer"));
    assert!(rendered.contains("checking provider shape"));
    assert!(rendered.contains("tool output"));
    assert!(rendered.contains("think 1"));
    assert!(rendered.contains("think 2"));
    assert!(rendered.contains("command failed"));
    assert!(rendered.contains("terminal_task"));
    assert!(rendered.contains("\"tool_name\":\"spawn_agent\""));
    assert!(rendered.contains("\"tool_name\":\"wait_agent\""));
    assert!(rendered.contains("\"thread_id\":\"agent_restore_1\""));
    assert!(!rendered.contains("other"));
    Ok(())
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
    assert!(app.restore_session_path_from_disk(
        invalid_path,
        "fallback-provider",
        "fallback-model",
        "restored",
    ));
    assert!(
        JsonlSessionStore::read_event_records(
            temp.path().join(".sigil/sessions/session-invalid.jsonl")
        )?
        .iter()
        .any(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_kind() == Some(DurableEventType::LogTailRecovered)
            )
        })
    );

    let blocked_parent = temp.path().join("not-a-directory");
    std::fs::write(&blocked_parent, "file parent")?;
    assert!(!app.restore_session_path_from_disk(
        blocked_parent.join("session-blocked.jsonl"),
        "fallback-provider",
        "fallback-model",
        "restored",
    ));
    Ok(())
}

#[test]
fn provider_projection_covers_compaction_preview_states() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    app.compaction_config = CompactionConfig {
        enabled: true,
        tail_messages: 1,
        context_window_tokens: Some(10_000),
        ..CompactionConfig::default()
    };
    app.sync_current_session_state(vec![
        SessionLogEntry::User(ModelMessage::user("first")),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("second".to_owned()),
            Vec::new(),
        )),
        SessionLogEntry::User(ModelMessage::user("third")),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("fourth".to_owned()),
            Vec::new(),
        )),
    ]);
    let preview = app.provider_projection_lines().join("\n");
    assert!(preview.contains("/compact preview: fold"));
    assert!(preview.contains("Before:"));
    assert!(preview.contains("After:"));

    app.sync_current_session_state(vec![SessionLogEntry::User(ModelMessage::user("only"))]);
    let nothing = app.provider_projection_lines().join("\n");
    assert!(nothing.contains("/compact preview: nothing to fold"));

    app.compaction_config.enabled = false;
    app.sync_current_session_state(vec![
        SessionLogEntry::User(ModelMessage::user("first")),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("second".to_owned()),
            Vec::new(),
        )),
    ]);
    let unavailable = app.provider_projection_lines().join("\n");
    assert!(unavailable.contains("/compact preview unavailable"));
}

#[test]
fn refresh_memory_summary_records_inspect_errors() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    app.workspace_root = temp.path().join("missing-workspace");
    app.memory_config = MemoryConfig { enabled: true };

    app.refresh_memory_summary();

    assert!(app.memory_enabled);
    assert_eq!(app.memory_document_count, 0);
    assert_ne!(app.memory_last_status, "ok");
    assert!(!app.memory_last_status.is_empty());
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
        tool_approval_action_label(sigil_kernel::ToolApprovalAuditAction::PolicyEvaluated),
        "policy"
    );
    assert_eq!(
        tool_approval_action_label(sigil_kernel::ToolApprovalAuditAction::Resolved),
        "resolved"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Started),
        "started"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Completed),
        "completed"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Failed),
        "failed"
    );
    assert_eq!(
        tool_execution_status_label(ToolExecutionStatus::Interrupted),
        "interrupted"
    );
    Ok(())
}

#[test]
fn render_session_control_entries_cover_remaining_labels() {
    let approval = render_session_log_entry(&SessionLogEntry::Control(ControlEntry::ToolApproval(
        ToolApprovalEntry {
            action: ToolApprovalAuditAction::Resolved,
            call_id: "call-approval".to_owned(),
            tool_name: "write_file".to_owned(),
            access: sigil_kernel::ToolAccess::Write,
            subjects: Vec::new(),
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            policy_decision: ApprovalMode::Deny,
            external_directory_required: false,
            user_decision: Some(ToolApprovalUserDecision::Denied),
            reason: Some("denied".to_owned()),
            preview_hash: None,
        },
    )));
    assert!(approval.contains("action=resolved"));

    let skill_index =
        render_session_log_entry(&SessionLogEntry::Control(ControlEntry::SkillIndexCaptured(
            SkillIndexSnapshot::new(vec![SkillDescriptor {
                id: "repo-review".to_owned(),
                name: "Repo Review".to_owned(),
                description: "Review repository changes".to_owned(),
                when_to_use: None,
                root: ".sigil/skills/repo-review".into(),
                entrypoint: ".sigil/skills/repo-review/SKILL.md".into(),
                source: SkillSource::Workspace,
                sha256: "hash".to_owned(),
                enabled: true,
                trust: SkillTrustState::Trusted,
                model_invocable: true,
                user_invocable: true,
                run_as: SkillRunMode::Inline,
                agent: None,
                argument_hint: None,
                allowed_tools: Default::default(),
                disallowed_tools: Default::default(),
                path_patterns: Vec::new(),
            }])
            .expect("valid skill index"),
        )));
    assert!(skill_index.contains("skills index count=1"));

    let skill_loaded = render_session_log_entry(&SessionLogEntry::Control(
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
    ));
    assert_eq!(
        skill_loaded,
        "[ctl] skill repo-review loaded bytes=128 lines=7"
    );

    let plugin_manifest = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::PluginManifestCaptured(PluginManifestSnapshot {
            plugin_id: "repo-review".to_owned(),
            name: "Repository Review".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
            manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
            manifest_hash: "sha256:manifest".to_owned(),
            capabilities: vec![PluginCapability::Skill {
                path: "skills/review/SKILL.md".into(),
            }],
            trust: PluginTrustDecision::NeedsReview,
        }),
    ));
    assert_eq!(
        plugin_manifest,
        "[ctl] plugin repo-review version=0.1.0 caps=1 trust=needs_review"
    );

    let plugin_trust = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::PluginTrustDecision(PluginTrustEntry {
            plugin_id: "repo-review".to_owned(),
            manifest_path: ".sigil/plugins/repo-review/plugin.toml".into(),
            manifest_hash: "sha256:manifest".to_owned(),
            decision: PluginTrustDecision::Trusted,
            reviewed_at_ms: 42,
        }),
    ));
    assert_eq!(
        plugin_trust,
        "[ctl] plugin repo-review trust=trusted hash=sha256:manifest"
    );

    let queue_id = sigil_kernel::ConversationInputQueueId::new("queue_1").expect("valid queue id");
    let queue_queued = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(sigil_kernel::ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: "sha256:queue".to_owned(),
            prompt: "queued prompt".to_owned(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        }),
    ));
    assert_eq!(
        queue_queued,
        "[ctl] queue queue_1 kind=Chat prompt=queued prompt"
    );
    let queue_control = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::ConversationInputQueueControl(
            sigil_kernel::ConversationInputQueueControlEntry {
                action: sigil_kernel::ConversationInputQueueControlAction::Pause,
                reason: None,
                updated_at_ms: Some(2),
            },
        ),
    ));
    assert_eq!(queue_control, "[ctl] queue control Pause");
    let queue_edited = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::ConversationInputEdited(sigil_kernel::ConversationInputEditedEntry {
            queue_id: queue_id.clone(),
            prompt_hash: "sha256:edited".to_owned(),
            prompt: "edited prompt".to_owned(),
            reasoning_effort: None,
            updated_at_ms: Some(3),
        }),
    ));
    assert_eq!(
        queue_edited,
        "[ctl] queue queue_1 edited prompt=edited prompt"
    );
    let queue_reordered = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::ConversationInputReordered(sigil_kernel::ConversationInputReorderedEntry {
            queue_id: queue_id.clone(),
            after_queue_id: None,
            updated_at_ms: Some(4),
        }),
    ));
    assert_eq!(queue_reordered, "[ctl] queue queue_1 moved after front");
    let queue_status = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(sigil_kernel::ConversationInputStatusEntry {
            queue_id,
            status: sigil_kernel::ConversationInputStatus::Delivered,
            reason: Some("sent now".to_owned()),
            updated_at_ms: Some(5),
        }),
    ));
    assert_eq!(queue_status, "[ctl] queue queue_1 status=Delivered");

    let continuation = render_session_log_entry(&SessionLogEntry::Control(
        ControlEntry::AgentResultContinuation(sigil_kernel::AgentResultContinuationEntry {
            thread_id: sigil_kernel::AgentThreadId::new("agent_chat_1").expect("valid thread id"),
            status: sigil_kernel::AgentResultContinuationStatus::Pending,
            reason: Some("waiting".to_owned()),
            updated_at_ms: Some(6),
        }),
    ));
    assert_eq!(
        continuation,
        "[ctl] agent continuation agent_chat_1 status=Pending"
    );

    for decision in [
        McpElicitationDecision::Accepted,
        McpElicitationDecision::Declined,
        McpElicitationDecision::Cancelled,
    ] {
        let line = render_session_log_entry(&SessionLogEntry::Control(
            ControlEntry::McpElicitation(Box::new(McpElicitationEntry::new(
                "server",
                "message",
                &json!({
                    "type": "object",
                    "properties": {"token": {"type": "string"}},
                    "required": ["token"]
                }),
                decision,
                Some(&json!({"token": "redacted"})),
            ))),
        ));
        assert!(line.contains("mcp elicitation server"));
    }

    let execution_with_error = ToolExecutionEntry {
        call_id: "call-error".to_owned(),
        tool_name: "bash".to_owned(),
        status: ToolExecutionStatus::Failed,
        duration_ms: Some(10),
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta::default(),
        error: Some(ToolError {
            kind: ToolErrorKind::Internal,
            message: "boom".to_owned(),
            retryable: false,
            details: serde_json::Value::Null,
        }),
        model_content_hash: None,
    };
    assert_eq!(
        restored_tool_execution_content(&execution_with_error),
        "boom"
    );
    assert!(should_render_restored_tool_execution(
        &ToolExecutionEntry {
            status: ToolExecutionStatus::Cancelled,
            ..execution_with_error.clone()
        },
        &std::collections::HashSet::new()
    ));
}

#[test]
fn restore_session_view_skips_empty_assistant_and_tool_result_content() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    let mut empty_tool = ModelMessage::new(sigil_kernel::MessageRole::Tool, None);
    empty_tool.tool_call_id = Some("call-empty".to_owned());

    app.restore_session_view(
        PathBuf::from("session-empty-content.jsonl"),
        "deepseek".to_owned(),
        "deepseek-v4-flash".to_owned(),
        vec![
            SessionLogEntry::Assistant(ModelMessage::assistant(None, Vec::new())),
            SessionLogEntry::Assistant(ModelMessage::assistant(Some(String::new()), Vec::new())),
            SessionLogEntry::ToolResult(empty_tool),
        ],
        "restored empty",
    );

    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Assistant)
    );
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Tool)
    );
}

#[test]
fn push_restored_reasoning_timeline_entry_ignores_whitespace_only_delta() {
    let mut timeline: Vec<crate::timeline::TimelineEntry> = Vec::new();
    push_restored_reasoning_timeline_entry(&mut timeline, "\n  ");
    assert!(timeline.is_empty());

    push_restored_reasoning_timeline_entry(&mut timeline, "");
    assert!(timeline.is_empty());

    push_restored_reasoning_timeline_entry(&mut timeline, "Real thinking");
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].role, TimelineRole::Thinking);
    assert_eq!(timeline[0].text, "Real thinking");
}
