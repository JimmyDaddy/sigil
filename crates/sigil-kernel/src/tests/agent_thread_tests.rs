use anyhow::Result;

use crate::{
    AgentApprovalRouteEntry, AgentArtifactRef, AgentElicitationRouteEntry, AgentInvocationMode,
    AgentInvocationSource, AgentMergeSafePointEntry, AgentProfile, AgentProfileCapturedEntry,
    AgentProfileId, AgentProfileKind, AgentProfileSnapshot, AgentProfileSnapshotId,
    AgentProfileSource, AgentRouteClosedEntry, AgentRouteId, AgentRouteStatus, AgentRunAttemptId,
    AgentRunAttemptStartedEntry, AgentRunContextSnapshot, AgentRunHeartbeatEntry,
    AgentRunInterruptedEntry, AgentThreadClosedEntry, AgentThreadDisplayNameEntry, AgentThreadId,
    AgentThreadMessageRoutedEntry, AgentThreadResult, AgentThreadResultRecordedEntry,
    AgentThreadStartedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentThreadTerminalStatus, AgentTrustState, AgentUsageSummary, ControlEntry, JsonlSessionStore,
    ModelMessage, Session, SessionLogEntry, SessionRef, TaskChildSessionDisplayNameEntry,
    TaskChildSessionEntry, TaskChildSessionStatus, TaskId, TaskStepId, WorkspaceRootSnapshot,
};

fn profile_id(value: &str) -> Result<AgentProfileId> {
    AgentProfileId::new(value)
}

fn snapshot_id(value: &str) -> Result<AgentProfileSnapshotId> {
    AgentProfileSnapshotId::new(value)
}

fn thread_id(value: &str) -> Result<AgentThreadId> {
    AgentThreadId::new(value)
}

fn attempt_id(value: &str) -> Result<AgentRunAttemptId> {
    AgentRunAttemptId::new(value)
}

fn route_id(value: &str) -> Result<AgentRouteId> {
    AgentRouteId::new(value)
}

fn session_ref(value: &str) -> Result<SessionRef> {
    SessionRef::new_relative(value)
}

fn task_id(value: &str) -> Result<TaskId> {
    TaskId::new(value)
}

fn step_id(value: &str) -> Result<TaskStepId> {
    TaskStepId::new(value)
}

fn sample_snapshot() -> Result<AgentProfileSnapshot> {
    Ok(AgentProfileSnapshot {
        snapshot_id: snapshot_id("snap_1")?,
        profile_id: profile_id("explore")?,
        source: AgentProfileSource::Workspace,
        source_hash: "sha256:source".to_owned(),
        profile_hash: "sha256:profile".to_owned(),
        resolved_tool_scope_hash: "sha256:tools".to_owned(),
        resolved_permission_policy_hash: "sha256:permissions".to_owned(),
        resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
        resolved_skill_hashes: vec!["sha256:skill".to_owned()],
        trust_state: AgentTrustState::Trusted,
    })
}

fn sample_run_context() -> Result<AgentRunContextSnapshot> {
    Ok(AgentRunContextSnapshot {
        profile_snapshot_id: snapshot_id("snap_1")?,
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning_effort: None,
        workspace_root: WorkspaceRootSnapshot::new("/workspace")?,
        effective_tool_scope_hash: "sha256:tools".to_owned(),
        effective_permission_policy_hash: "sha256:permissions".to_owned(),
        effective_mcp_scope_hash: "sha256:mcp".to_owned(),
        provider_capability_hash: "sha256:provider-capabilities".to_owned(),
        model_visible_agent_index_hash: Some("sha256:index".to_owned()),
        budget_policy_hash: "sha256:budget".to_owned(),
        provider_background_handle_ref: Some("opaque-handle".to_owned()),
    })
}

fn sample_started_entry() -> Result<AgentThreadStartedEntry> {
    Ok(AgentThreadStartedEntry {
        thread_id: thread_id("thread_1")?,
        parent_thread_id: Some(thread_id("main")?),
        parent_session_ref: session_ref("parent.jsonl")?,
        thread_session_ref: session_ref("children/thread_1.jsonl")?,
        profile_id: profile_id("explore")?,
        profile_snapshot_id: snapshot_id("snap_1")?,
        run_context: sample_run_context()?,
        objective: "inspect kernel".to_owned(),
        prompt_hash: "sha256:prompt".to_owned(),
        invocation_mode: AgentInvocationMode::Foreground,
        invocation_source: AgentInvocationSource::Chat,
        display_name: Some("kernel map".to_owned()),
        created_at_ms: Some(42),
    })
}

#[test]
fn agent_identifiers_reject_path_unsafe_values() {
    assert!(AgentProfileId::new("").is_err());
    assert!(AgentProfileId::new("../agent").is_err());
    assert!(AgentThreadId::new("thread/slash").is_err());
    assert!(AgentProfileSnapshotId::new("snap space").is_err());
    assert!(AgentRunAttemptId::new("attempt:1").is_err());
    assert!(WorkspaceRootSnapshot::new("").is_err());
    assert!(WorkspaceRootSnapshot::new("workspace\nroot").is_err());
    assert!(AgentRouteId::new("route_1").is_ok());
    assert_eq!(
        AgentProfileSnapshotId::new("snap_1")
            .expect("snapshot id should parse")
            .as_str(),
        "snap_1"
    );
    assert_eq!(
        AgentRouteId::new("route_1")
            .expect("route id should parse")
            .as_str(),
        "route_1"
    );
    assert_eq!(
        WorkspaceRootSnapshot::new("/workspace")
            .expect("workspace root should parse")
            .as_str(),
        "/workspace"
    );
    assert_eq!(
        AgentThreadId::new("thread_1")
            .expect("thread id should parse")
            .as_str(),
        "thread_1"
    );
}

#[test]
fn agent_profile_defaults_keep_model_invocation_disabled() -> Result<()> {
    let profile = AgentProfile {
        id: profile_id("explore")?,
        kind: AgentProfileKind::Subagent,
        description: "Read-only exploration".to_owned(),
        instructions: "Inspect only.".to_owned(),
        model: None,
        provider: None,
        reasoning_effort: None,
        tool_scope: Default::default(),
        permission_policy: Default::default(),
        user_invocable: true,
        model_invocable: false,
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        nickname_candidates: vec!["Atlas".to_owned()],
    };

    let encoded = serde_json::to_string(&profile)?;
    let decoded: AgentProfile = serde_json::from_str(&encoded)?;

    assert!(decoded.user_invocable);
    assert!(!decoded.model_invocable);
    assert_eq!(decoded.nickname_candidates, vec!["Atlas"]);
    Ok(())
}

#[test]
fn agent_control_entries_roundtrip() -> Result<()> {
    let result = AgentThreadResult {
        thread_id: thread_id("thread_1")?,
        session_ref: session_ref("children/thread_1.jsonl")?,
        status: AgentThreadTerminalStatus::Completed,
        summary: "Kernel structure mapped.".to_owned(),
        summary_truncated: false,
        original_summary_chars: None,
        artifacts: vec![AgentArtifactRef {
            kind: "report".to_owned(),
            path: ".repo-local-dev/kernel.md".to_owned(),
            hash: Some("sha256:artifact".to_owned()),
        }],
        changed_paths: Vec::new(),
        risks: Vec::new(),
        followups: vec!["Review runtime next.".to_owned()],
        usage: Some(AgentUsageSummary {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            cached_tokens: Some(4),
        }),
        output_hash: "sha256:result".to_owned(),
    };
    let entries = vec![
        ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
            snapshot: sample_snapshot()?,
        }),
        ControlEntry::AgentThreadStarted(sample_started_entry()?),
        ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id: thread_id("thread_1")?,
            status: AgentThreadStatus::Running,
            reason: None,
            updated_at_ms: Some(43),
        }),
        ControlEntry::AgentThreadMessageRouted(AgentThreadMessageRoutedEntry {
            route_id: route_id("route_1")?,
            source_thread_id: thread_id("main")?,
            target_thread_id: thread_id("thread_1")?,
            prompt_hash: "sha256:steer".to_owned(),
            status: AgentRouteStatus::Resolved,
        }),
        ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry { result }),
        ControlEntry::AgentThreadDisplayName(AgentThreadDisplayNameEntry {
            thread_id: thread_id("thread_1")?,
            display_name: "kernel map".to_owned(),
        }),
        ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
            route_id: route_id("route_2")?,
            source_thread_id: thread_id("thread_1")?,
            target_thread_id: Some(thread_id("main")?),
            call_id: "call-1".to_owned(),
            tool_name: "read_file".to_owned(),
            status: AgentRouteStatus::Requested,
        }),
        ControlEntry::AgentElicitationRoute(AgentElicitationRouteEntry {
            route_id: route_id("route_3")?,
            source_thread_id: thread_id("thread_1")?,
            target_thread_id: Some(thread_id("main")?),
            server_name: "filesystem".to_owned(),
            status: AgentRouteStatus::Registered,
        }),
        ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
            thread_id: thread_id("thread_1")?,
            attempt_id: attempt_id("attempt_1")?,
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            background: true,
            provider_background_handle_ref: Some("handle".to_owned()),
        }),
        ControlEntry::AgentRunHeartbeat(AgentRunHeartbeatEntry {
            thread_id: thread_id("thread_1")?,
            attempt_id: attempt_id("attempt_1")?,
            updated_at_ms: 44,
        }),
        ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
            thread_id: thread_id("thread_1")?,
            attempt_id: attempt_id("attempt_1")?,
            reason: "restore".to_owned(),
        }),
        ControlEntry::AgentRouteClosed(AgentRouteClosedEntry {
            route_id: route_id("route_2")?,
            reason: "restore".to_owned(),
        }),
        ControlEntry::AgentMergeSafePoint(AgentMergeSafePointEntry {
            thread_id: thread_id("thread_1")?,
            parent_thread_id: thread_id("main")?,
            result_hash: "sha256:result".to_owned(),
        }),
        ControlEntry::AgentThreadClosed(AgentThreadClosedEntry {
            thread_id: thread_id("thread_1")?,
            reason: Some("archived".to_owned()),
        }),
    ];

    for entry in entries {
        let session_entry = SessionLogEntry::Control(entry);
        let encoded = serde_json::to_string(&session_entry)?;
        let decoded: SessionLogEntry = serde_json::from_str(&encoded)?;
        assert!(matches!(decoded, SessionLogEntry::Control(_)));
    }
    Ok(())
}

#[test]
fn agent_thread_projection_replays_lifecycle_and_result() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-pro");
    session.append_control(ControlEntry::AgentProfileCaptured(
        AgentProfileCapturedEntry {
            snapshot: sample_snapshot()?,
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStarted(sample_started_entry()?))?;
    session.append_control(ControlEntry::AgentRunAttemptStarted(
        AgentRunAttemptStartedEntry {
            thread_id: thread_id("thread_1")?,
            attempt_id: attempt_id("attempt_1")?,
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            background: false,
            provider_background_handle_ref: None,
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStatusChanged(
        AgentThreadStatusChangedEntry {
            thread_id: thread_id("thread_1")?,
            status: AgentThreadStatus::Running,
            reason: None,
            updated_at_ms: None,
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadResultRecorded(
        AgentThreadResultRecordedEntry {
            result: AgentThreadResult {
                thread_id: thread_id("thread_1")?,
                session_ref: session_ref("children/thread_1.jsonl")?,
                status: AgentThreadTerminalStatus::Completed,
                summary: "done".to_owned(),
                summary_truncated: false,
                original_summary_chars: None,
                artifacts: Vec::new(),
                changed_paths: Vec::new(),
                risks: Vec::new(),
                followups: Vec::new(),
                usage: None,
                output_hash: "sha256:done".to_owned(),
            },
        },
    ))?;

    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("latest thread");

    assert_eq!(projection.profiles.len(), 1);
    assert_eq!(thread.thread_id.as_str(), "thread_1");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(thread.display_name.as_deref(), Some("kernel map"));
    assert_eq!(
        thread.result.as_ref().map(|result| result.summary.as_str()),
        Some("done")
    );
    assert_eq!(
        thread
            .run_context
            .as_ref()
            .map(|context| context.model.as_str()),
        Some("deepseek-v4-pro")
    );
    Ok(())
}

#[test]
fn agent_thread_projection_covers_attempt_display_merge_and_close_edges() -> Result<()> {
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("ignored by projection")),
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            AgentProfileCapturedEntry {
                snapshot: sample_snapshot()?,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(sample_started_entry()?)),
        SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(
            AgentThreadDisplayNameEntry {
                thread_id: thread_id("thread_1")?,
                display_name: "renamed".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentRunHeartbeat(AgentRunHeartbeatEntry {
            thread_id: thread_id("thread_1")?,
            attempt_id: attempt_id("attempt_heartbeat")?,
            updated_at_ms: 46,
        })),
        SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(
            AgentRunInterruptedEntry {
                thread_id: thread_id("thread_1")?,
                attempt_id: attempt_id("attempt_interrupted")?,
                reason: "network dropped".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            AgentThreadStatusChangedEntry {
                thread_id: thread_id("thread_1")?,
                status: AgentThreadStatus::Failed,
                reason: Some("duplicate terminal".to_owned()),
                updated_at_ms: Some(47),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(
            AgentThreadResultRecordedEntry {
                result: AgentThreadResult {
                    thread_id: thread_id("thread_1")?,
                    session_ref: session_ref("children/thread_1.jsonl")?,
                    status: AgentThreadTerminalStatus::Unknown,
                    summary: "unknown future status".to_owned(),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: "sha256:unknown".to_owned(),
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentMergeSafePoint(
            AgentMergeSafePointEntry {
                thread_id: thread_id("thread_1")?,
                parent_thread_id: thread_id("main")?,
                result_hash: "sha256:unknown".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadClosed(AgentThreadClosedEntry {
            thread_id: thread_id("thread_1")?,
            reason: Some("archived".to_owned()),
        })),
    ];

    let projection = crate::AgentThreadStateProjection::from_entries(&entries);
    let thread = projection.latest_thread().expect("latest thread");

    assert_eq!(thread.display_name.as_deref(), Some("renamed"));
    assert_eq!(thread.status, AgentThreadStatus::Closed);
    assert!(thread.closed);
    assert_eq!(thread.reason.as_deref(), Some("archived"));
    assert_eq!(thread.duplicate_terminal_entries, 1);
    assert_eq!(thread.merge_safe_points.len(), 1);
    assert_eq!(
        thread
            .attempts
            .get(&attempt_id("attempt_heartbeat")?)
            .and_then(|attempt| attempt.last_heartbeat_ms),
        Some(46)
    );
    assert_eq!(
        thread
            .attempts
            .get(&attempt_id("attempt_interrupted")?)
            .and_then(|attempt| attempt.interrupted.as_deref()),
        Some("network dropped")
    );
    assert_eq!(
        thread.result.as_ref().map(|result| result.status),
        Some(AgentThreadTerminalStatus::Unknown)
    );
    Ok(())
}

#[test]
fn agent_thread_result_statuses_project_to_terminal_thread_statuses() -> Result<()> {
    let cases = [
        (
            AgentThreadTerminalStatus::Failed,
            AgentThreadStatus::Failed,
            "thread_failed",
        ),
        (
            AgentThreadTerminalStatus::Cancelled,
            AgentThreadStatus::Cancelled,
            "thread_cancelled",
        ),
        (
            AgentThreadTerminalStatus::Interrupted,
            AgentThreadStatus::Interrupted,
            "thread_interrupted",
        ),
    ];

    for (terminal_status, expected_status, thread_name) in cases {
        let mut started = sample_started_entry()?;
        started.thread_id = thread_id(thread_name)?;
        let session_path = format!("children/{thread_name}.jsonl");
        started.thread_session_ref = session_ref(&session_path)?;
        let result = AgentThreadResult {
            thread_id: thread_id(thread_name)?,
            session_ref: session_ref(&session_path)?,
            status: terminal_status,
            summary: "done".to_owned(),
            summary_truncated: false,
            original_summary_chars: None,
            artifacts: Vec::new(),
            changed_paths: Vec::new(),
            risks: Vec::new(),
            followups: Vec::new(),
            usage: None,
            output_hash: "sha256:done".to_owned(),
        };
        let entries = vec![
            SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
                AgentProfileCapturedEntry {
                    snapshot: sample_snapshot()?,
                },
            )),
            SessionLogEntry::Control(ControlEntry::AgentThreadStarted(started)),
            SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(
                AgentThreadResultRecordedEntry { result },
            )),
        ];

        let projection = crate::AgentThreadStateProjection::from_entries(&entries);
        let thread = projection.latest_thread().expect("latest thread");

        assert_eq!(thread.status, expected_status);
    }
    Ok(())
}

#[test]
fn agent_result_without_started_entry_stays_unresolved() -> Result<()> {
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry {
            result: AgentThreadResult {
                thread_id: thread_id("thread_1")?,
                session_ref: session_ref("children/thread_1.jsonl")?,
                status: AgentThreadTerminalStatus::Completed,
                summary: "done".to_owned(),
                summary_truncated: false,
                original_summary_chars: None,
                artifacts: Vec::new(),
                changed_paths: Vec::new(),
                risks: Vec::new(),
                followups: Vec::new(),
                usage: None,
                output_hash: "sha256:done".to_owned(),
            },
        }),
    )];

    let projection = crate::AgentThreadStateProjection::from_entries(&entries);
    let thread = projection.latest_thread().expect("unresolved thread");

    assert!(thread.unresolved);
    assert_eq!(thread.status, AgentThreadStatus::Unavailable);
    assert_eq!(
        thread.reason.as_deref(),
        Some("agent thread start entry missing")
    );
    assert!(thread.result.is_some());
    Ok(())
}

#[test]
fn legacy_task_child_session_projects_as_agent_thread() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-pro");
    let child = TaskChildSessionEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        step_id: step_id("step_1")?,
        child_task_id: task_id("child_1")?,
        child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
        role: crate::AgentRole::SubagentRead,
        status: TaskChildSessionStatus::Completed,
        summary_hash: Some("sha256:summary".to_owned()),
    };
    session.append_control(ControlEntry::TaskChildSession(child))?;
    session.append_control(ControlEntry::TaskChildSessionDisplayName(
        TaskChildSessionDisplayNameEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            child_task_id: task_id("child_1")?,
            display_name: "kernel map".to_owned(),
        },
    ))?;

    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("legacy thread");

    assert!(thread.legacy_task);
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(
        thread.profile_id.as_ref().map(AgentProfileId::as_str),
        Some("legacy_subagent_read")
    );
    assert_eq!(thread.display_name.as_deref(), Some("kernel map"));
    assert_eq!(projection.legacy_task_thread_ids.len(), 1);
    Ok(())
}

#[test]
fn legacy_task_child_session_projects_all_statuses() -> Result<()> {
    let cases = [
        (TaskChildSessionStatus::Started, AgentThreadStatus::Started),
        (TaskChildSessionStatus::Failed, AgentThreadStatus::Failed),
        (
            TaskChildSessionStatus::Cancelled,
            AgentThreadStatus::Cancelled,
        ),
        (
            TaskChildSessionStatus::Interrupted,
            AgentThreadStatus::Interrupted,
        ),
        (
            TaskChildSessionStatus::Unavailable,
            AgentThreadStatus::Unavailable,
        ),
    ];

    for (index, (legacy_status, expected_status)) in cases.into_iter().enumerate() {
        let task_name = format!("task_{index}");
        let step_name = format!("step_{index}");
        let child_name = format!("child_{index}");
        let child_session_path = format!("children/task_{index}.jsonl");
        let task = task_id(&task_name)?;
        let step = step_id(&step_name)?;
        let child = task_id(&child_name)?;
        let entries = vec![SessionLogEntry::Control(ControlEntry::TaskChildSession(
            TaskChildSessionEntry {
                task_id: task,
                plan_version: 1,
                step_id: step,
                child_task_id: child,
                child_session_ref: session_ref(&child_session_path)?,
                role: crate::AgentRole::SubagentWrite,
                status: legacy_status,
                summary_hash: None,
            },
        ))];

        let projection = crate::AgentThreadStateProjection::from_entries(&entries);
        let thread = projection.latest_thread().expect("legacy thread");

        assert_eq!(thread.status, expected_status);
        assert_eq!(
            thread.profile_id.as_ref().map(AgentProfileId::as_str),
            Some("legacy_subagent_write")
        );
    }
    Ok(())
}

#[test]
fn legacy_task_display_name_before_child_entry_still_projects_legacy_thread() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(
            TaskChildSessionDisplayNameEntry {
                task_id: task_id("task_1")?,
                plan_version: 1,
                step_id: step_id("step_1")?,
                child_task_id: task_id("child_1")?,
                display_name: "kernel map".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            child_task_id: task_id("child_1")?,
            child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
            role: crate::AgentRole::SubagentRead,
            status: TaskChildSessionStatus::Completed,
            summary_hash: None,
        })),
    ];

    let projection = crate::AgentThreadStateProjection::from_entries(&entries);
    let thread = projection.latest_thread().expect("legacy thread");

    assert!(thread.legacy_task);
    assert_eq!(thread.invocation_source, Some(AgentInvocationSource::Task));
    assert_eq!(thread.display_name.as_deref(), Some("kernel map"));
    assert_eq!(
        thread.profile_id.as_ref().map(AgentProfileId::as_str),
        Some("legacy_subagent_read")
    );
    Ok(())
}

#[test]
fn agent_snapshot_restore_uses_captured_run_context() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-pro");
    session.append_control(ControlEntry::AgentProfileCaptured(
        AgentProfileCapturedEntry {
            snapshot: sample_snapshot()?,
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStarted(sample_started_entry()?))?;

    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("latest thread");
    let context = thread.run_context.as_ref().expect("run context snapshot");

    assert_eq!(context.provider, "deepseek");
    assert_eq!(context.model, "deepseek-v4-pro");
    assert_eq!(
        context.provider_background_handle_ref.as_deref(),
        Some("opaque-handle")
    );
    assert_eq!(
        context.model_visible_agent_index_hash.as_deref(),
        Some("sha256:index")
    );
    Ok(())
}

#[test]
fn agent_thread_started_without_profile_snapshot_projects_unavailable() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-pro");
    session.append_control(ControlEntry::AgentThreadStarted(sample_started_entry()?))?;

    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("latest thread");

    assert_eq!(thread.status, AgentThreadStatus::Unavailable);
    assert!(thread.profile_snapshot_missing);
    assert_eq!(
        thread.reason.as_deref(),
        Some("agent profile snapshot missing")
    );
    Ok(())
}

#[test]
fn agent_thread_started_with_mismatched_run_context_snapshot_projects_unavailable() -> Result<()> {
    let mut started = sample_started_entry()?;
    started.run_context.profile_snapshot_id = snapshot_id("snap_2")?;
    let mut session = Session::new("deepseek", "deepseek-v4-pro");
    session.append_control(ControlEntry::AgentProfileCaptured(
        AgentProfileCapturedEntry {
            snapshot: sample_snapshot()?,
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStarted(started))?;

    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("latest thread");

    assert_eq!(thread.status, AgentThreadStatus::Unavailable);
    assert!(thread.profile_snapshot_mismatch);
    assert!(!thread.profile_snapshot_missing);
    assert_eq!(
        thread.reason.as_deref(),
        Some("agent profile snapshot mismatch")
    );
    Ok(())
}

#[test]
fn agent_status_without_started_projects_unresolved_thread() -> Result<()> {
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id: thread_id("thread_1")?,
            status: AgentThreadStatus::Running,
            reason: Some("late status".to_owned()),
            updated_at_ms: Some(45),
        }),
    )];

    let projection = crate::AgentThreadStateProjection::from_entries(&entries);
    let thread = projection.latest_thread().expect("unresolved thread");

    assert!(thread.unresolved);
    assert_eq!(thread.status, AgentThreadStatus::Unavailable);
    assert_eq!(thread.reason.as_deref(), Some("late status"));
    assert!(thread.profile_id.is_none());
    assert!(thread.thread_session_ref.is_none());
    Ok(())
}

#[test]
fn agent_unknown_enum_values_deserialize_without_failing() -> Result<()> {
    let status: SessionLogEntry = serde_json::from_value(serde_json::json!({
        "control": {
            "agent_thread_status_changed": {
                "thread_id": "thread_1",
                "status": "paused_by_future_runtime"
            }
        }
    }))?;
    assert!(matches!(
        status,
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(entry))
            if entry.status == AgentThreadStatus::Unknown
    ));

    let started: SessionLogEntry = serde_json::from_value(serde_json::json!({
        "control": {
            "agent_thread_started": {
                "thread_id": "thread_1",
                "parent_session_ref": { "path": "parent.jsonl" },
                "thread_session_ref": { "path": "children/thread_1.jsonl" },
                "profile_id": "explore",
                "profile_snapshot_id": "snap_1",
                "run_context": {
                    "profile_snapshot_id": "snap_1",
                    "provider": "deepseek",
                    "model": "deepseek-v4-pro",
                    "workspace_root": "/workspace"
                },
                "objective": "inspect kernel",
                "invocation_mode": "future_mode",
                "invocation_source": "future_source"
            }
        }
    }))?;
    assert!(matches!(
        started,
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(entry))
            if entry.invocation_mode == AgentInvocationMode::Unknown
                && entry.invocation_source == AgentInvocationSource::Unknown
    ));

    let profile: AgentProfileSnapshot = serde_json::from_value(serde_json::json!({
        "snapshot_id": "snap_1",
        "profile_id": "explore",
        "source": { "kind": "future_source" },
        "trust_state": "future_trust"
    }))?;
    assert_eq!(profile.source, AgentProfileSource::Unknown);
    assert_eq!(profile.trust_state, AgentTrustState::Unknown);
    Ok(())
}

#[test]
fn load_from_store_marks_orphan_agent_attempt_as_interrupted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
        sample_started_entry()?,
    )))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
            thread_id: thread_id("thread_1")?,
            attempt_id: attempt_id("attempt_1")?,
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            background: true,
            provider_background_handle_ref: Some("opaque-handle".to_owned()),
        }),
    ))?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-pro", store.clone())?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(interrupted))
                if interrupted.thread_id.as_str() == "thread_1"
                    && interrupted.attempt_id.as_str() == "attempt_1"
        )
    }));
    let reloaded = JsonlSessionStore::read_entries(store.path())?;
    assert!(reloaded.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(interrupted))
                if interrupted.thread_id.as_str() == "thread_1"
        )
    }));
    Ok(())
}

#[test]
fn interrupted_agent_attempts_skip_terminal_attempts_and_threads() -> Result<()> {
    let started_attempt = |thread: &str, attempt: &str| -> Result<SessionLogEntry> {
        Ok(SessionLogEntry::Control(
            ControlEntry::AgentRunAttemptStarted(AgentRunAttemptStartedEntry {
                thread_id: thread_id(thread)?,
                attempt_id: attempt_id(attempt)?,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                background: false,
                provider_background_handle_ref: None,
            }),
        ))
    };
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("ignored")),
        started_attempt("thread_interrupted", "attempt_interrupted")?,
        SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(
            AgentRunInterruptedEntry {
                thread_id: thread_id("thread_interrupted")?,
                attempt_id: attempt_id("attempt_interrupted")?,
                reason: "already interrupted".to_owned(),
            },
        )),
        started_attempt("thread_result", "attempt_result")?,
        SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(
            AgentThreadResultRecordedEntry {
                result: AgentThreadResult {
                    thread_id: thread_id("thread_result")?,
                    session_ref: session_ref("children/thread_result.jsonl")?,
                    status: AgentThreadTerminalStatus::Completed,
                    summary: "done".to_owned(),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: "sha256:done".to_owned(),
                },
            },
        )),
        started_attempt("thread_status", "attempt_status")?,
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            AgentThreadStatusChangedEntry {
                thread_id: thread_id("thread_status")?,
                status: AgentThreadStatus::Completed,
                reason: None,
                updated_at_ms: None,
            },
        )),
        started_attempt("thread_closed", "attempt_closed")?,
        SessionLogEntry::Control(ControlEntry::AgentThreadClosed(AgentThreadClosedEntry {
            thread_id: thread_id("thread_closed")?,
            reason: None,
        })),
        started_attempt("thread_open", "attempt_open")?,
    ];

    let interrupted = crate::interrupted_agent_attempts(&entries);

    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].thread_id.as_str(), "thread_open");
    assert_eq!(interrupted[0].attempt_id.as_str(), "attempt_open");
    Ok(())
}

#[test]
fn load_from_store_closes_orphan_agent_routes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(
        AgentApprovalRouteEntry {
            route_id: route_id("route_1")?,
            source_thread_id: thread_id("thread_1")?,
            target_thread_id: None,
            call_id: "call-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: AgentRouteStatus::Requested,
        },
    )))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::AgentElicitationRoute(AgentElicitationRouteEntry {
            route_id: route_id("route_2")?,
            source_thread_id: thread_id("thread_1")?,
            target_thread_id: None,
            server_name: "filesystem".to_owned(),
            status: AgentRouteStatus::Registered,
        }),
    ))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::AgentThreadMessageRouted(AgentThreadMessageRoutedEntry {
            route_id: route_id("route_3")?,
            source_thread_id: thread_id("main")?,
            target_thread_id: thread_id("thread_1")?,
            prompt_hash: "sha256:message".to_owned(),
            status: AgentRouteStatus::Requested,
        }),
    ))?;

    let session = Session::load_from_store("deepseek", "deepseek-v4-pro", store)?;
    let projection = session.agent_thread_state_projection();

    assert!(projection.closed_routes.contains_key(&route_id("route_1")?));
    assert!(projection.closed_routes.contains_key(&route_id("route_2")?));
    assert!(projection.closed_routes.contains_key(&route_id("route_3")?));
    assert_eq!(
        projection
            .approval_routes
            .get(&route_id("route_1")?)
            .map(|route| route.status),
        Some(AgentRouteStatus::Closed)
    );
    assert_eq!(
        projection
            .elicitation_routes
            .get(&route_id("route_2")?)
            .map(|route| route.status),
        Some(AgentRouteStatus::Closed)
    );
    assert_eq!(
        projection
            .message_routes
            .get(&route_id("route_3")?)
            .map(|route| route.status),
        Some(AgentRouteStatus::Closed)
    );
    Ok(())
}

#[test]
fn closed_agent_routes_skip_terminal_and_already_closed_routes() -> Result<()> {
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("ignored")),
        SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
            route_id: route_id("route_terminal")?,
            source_thread_id: thread_id("thread_1")?,
            target_thread_id: None,
            call_id: "call-terminal".to_owned(),
            tool_name: "read_file".to_owned(),
            status: AgentRouteStatus::Resolved,
        })),
        SessionLogEntry::Control(ControlEntry::AgentElicitationRoute(
            AgentElicitationRouteEntry {
                route_id: route_id("route_already_closed")?,
                source_thread_id: thread_id("thread_1")?,
                target_thread_id: None,
                server_name: "filesystem".to_owned(),
                status: AgentRouteStatus::Requested,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentRouteClosed(AgentRouteClosedEntry {
            route_id: route_id("route_already_closed")?,
            reason: "already closed".to_owned(),
        })),
        SessionLogEntry::Control(ControlEntry::AgentThreadMessageRouted(
            AgentThreadMessageRoutedEntry {
                route_id: route_id("route_open")?,
                source_thread_id: thread_id("main")?,
                target_thread_id: thread_id("thread_1")?,
                prompt_hash: "sha256:message".to_owned(),
                status: AgentRouteStatus::Registered,
            },
        )),
    ];

    let closed = crate::closed_agent_routes(&entries);

    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].route_id.as_str(), "route_open");
    Ok(())
}

#[test]
fn profile_hash_change_can_be_captured_with_needs_review_trust() -> Result<()> {
    let trusted = sample_snapshot()?;
    let changed = AgentProfileSnapshot {
        snapshot_id: snapshot_id("snap_2")?,
        profile_hash: "sha256:changed-profile".to_owned(),
        trust_state: AgentTrustState::NeedsReview,
        ..trusted.clone()
    };
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            AgentProfileCapturedEntry { snapshot: trusted },
        )),
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            AgentProfileCapturedEntry { snapshot: changed },
        )),
    ];

    let projection = crate::AgentThreadStateProjection::from_entries(&entries);

    assert_eq!(projection.profiles.len(), 2);
    assert!(projection.profiles.values().any(|snapshot| {
        snapshot.profile_hash == "sha256:changed-profile"
            && snapshot.trust_state == AgentTrustState::NeedsReview
    }));
    Ok(())
}
