use super::*;

#[test]
fn eval_projection_preserves_failure_cancel_pause_and_interrupt() {
    for (terminal, expected) in [
        (
            ApplicationRunTerminalStatus::Succeeded,
            RunStatus::Completed,
        ),
        (ApplicationRunTerminalStatus::Failed, RunStatus::Failed),
        (ApplicationRunTerminalStatus::Blocked, RunStatus::Blocked),
        (
            ApplicationRunTerminalStatus::Cancelled,
            RunStatus::Cancelled,
        ),
        (
            ApplicationRunTerminalStatus::Interrupted,
            RunStatus::Interrupted,
        ),
        (ApplicationRunTerminalStatus::Paused, RunStatus::Paused),
        (
            ApplicationRunTerminalStatus::AwaitingUserInput,
            RunStatus::Paused,
        ),
    ] {
        assert_eq!(terminal_run_status(terminal), expected);
    }
}

#[test]
fn agent_batch_assertion_requires_completed_durable_join_delivery() -> anyhow::Result<()> {
    let completed = join_batch_session(sigil_kernel::AgentResultContinuationStatus::Completed)?;
    assert!(agent_batch_succeeded(&completed, 2, "explore"));
    assert!(!agent_batch_succeeded(&completed, 3, "explore"));
    assert!(!agent_batch_succeeded(&completed, 2, "planner"));

    let pending = join_batch_session(sigil_kernel::AgentResultContinuationStatus::Pending)?;
    assert!(!agent_batch_succeeded(&pending, 2, "explore"));
    Ok(())
}

fn join_batch_session(
    continuation_status: sigil_kernel::AgentResultContinuationStatus,
) -> anyhow::Result<Session> {
    use sigil_kernel::{
        AgentBatchId, AgentProfileCapturedEntry, AgentProfileId, AgentProfileSnapshot,
        AgentProfileSnapshotId, AgentProfileSource, AgentResultContinuationEntry, AgentRouteId,
        AgentRunContextSnapshot, AgentThreadId, AgentThreadResult, AgentThreadResultRecordedEntry,
        AgentThreadStartedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
        AgentThreadTerminalStatus, AgentTrustState, ControlEntry, SessionLogEntry, SessionRef,
        WorkspaceRootSnapshot,
    };

    let profile_id = AgentProfileId::new("explore")?;
    let profile_snapshot_id = AgentProfileSnapshotId::new("snapshot_explore")?;
    let batch_id = AgentBatchId::new("batch_eval")?;
    let mut entries = vec![SessionLogEntry::Control(
        ControlEntry::AgentProfileCaptured(AgentProfileCapturedEntry {
            snapshot: AgentProfileSnapshot {
                snapshot_id: profile_snapshot_id.clone(),
                profile_id: profile_id.clone(),
                source: AgentProfileSource::System,
                source_hash: "sha256:source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                resolved_tool_scope_hash: "sha256:tools".to_owned(),
                resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                resolved_skill_hashes: Vec::new(),
                trust_state: AgentTrustState::Trusted,
            },
        }),
    )];

    for member in ["parser", "formatter"] {
        let thread_id = AgentThreadId::new(format!("agent_{member}"))?;
        let member_key = AgentRouteId::new(format!("{member}_scope"))?;
        entries.push(SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(AgentThreadId::new("main")?),
                batch_id: Some(batch_id.clone()),
                batch_member_key: Some(member_key),
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: SessionRef::new_relative(format!("children/{member}.jsonl"))?,
                profile_id: profile_id.clone(),
                profile_snapshot_id: profile_snapshot_id.clone(),
                run_context: AgentRunContextSnapshot {
                    profile_snapshot_id: profile_snapshot_id.clone(),
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-flash".to_owned(),
                    model_ref: None,
                    reasoning_effort: None,
                    workspace_root: WorkspaceRootSnapshot::new(
                        std::env::temp_dir().display().to_string(),
                    )?,
                    effective_tool_scope_hash: "sha256:tools".to_owned(),
                    effective_permission_policy_hash: "sha256:permissions".to_owned(),
                    effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                    provider_capability_hash: "sha256:provider".to_owned(),
                    model_visible_agent_index_hash: None,
                    budget_policy_hash: "sha256:budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: format!("inspect {member}"),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::JoinBeforeFinal,
                invocation_source: sigil_kernel::AgentInvocationSource::Chat,
                display_name: None,
                created_at_ms: Some(1),
            },
        )));
        entries.push(SessionLogEntry::Control(
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: AgentThreadStatus::Completed,
                reason: None,
                updated_at_ms: None,
            }),
        ));
        entries.push(SessionLogEntry::Control(
            ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry {
                result: AgentThreadResult {
                    thread_id: thread_id.clone(),
                    session_ref: SessionRef::new_relative(format!("children/{member}.jsonl"))?,
                    status: AgentThreadTerminalStatus::Completed,
                    summary: format!("{member} inspected"),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: format!("sha256:{}", "a".repeat(64)),
                    final_answer_ref: None,
                },
            }),
        ));
        entries.push(SessionLogEntry::Control(
            ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                thread_id,
                status: continuation_status,
                reason: Some("join context delivery".to_owned()),
                updated_at_ms: Some(2),
            }),
        ));
    }

    Ok(Session::from_entries(
        "deepseek",
        "deepseek-v4-flash",
        entries,
    ))
}
