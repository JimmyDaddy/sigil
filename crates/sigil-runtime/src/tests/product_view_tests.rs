use anyhow::Result;
use sigil_kernel::{
    AgentInvocationMode, AgentInvocationSource, AgentProfileCapturedEntry, AgentProfileId,
    AgentProfileSnapshot, AgentProfileSnapshotId, AgentProfileSource, AgentResultContinuationEntry,
    AgentResultContinuationStatus, AgentRunContextSnapshot, AgentThreadId, AgentThreadStartedEntry,
    AgentThreadStatus, AgentThreadStatusChangedEntry, AgentTrustState, ControlEntry,
    JsonlSessionStore, ModelMessage, SessionLogEntry, SessionRef, WorkspaceRootSnapshot,
};

use super::{
    agent_graph_product_summary_from_entries, agent_graph_product_summary_from_session_log,
};

#[test]
fn agent_graph_product_view_replays_durable_session_log() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_log_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    for entry in agent_entries(temp.path(), AgentThreadStatus::Running)? {
        store.append(&entry)?;
    }

    let summary = agent_graph_product_summary_from_session_log(&session_log_path)?
        .expect("durable agent graph should produce summary");

    assert_eq!(summary.total_agents, 1);
    assert_eq!(summary.active_agents, 1);
    assert_eq!(summary.terminal_agents, 0);
    assert!(!summary.projection_degraded);
    assert_eq!(summary.display_line(), "graph: 1 agents · 1 active");
    Ok(())
}

#[test]
fn agent_graph_product_view_treats_unresolved_continuation_as_active() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut entries = agent_entries(temp.path(), AgentThreadStatus::Failed)?;
    entries.push(SessionLogEntry::Control(
        ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
            thread_id: AgentThreadId::new("thread_1")?,
            status: AgentResultContinuationStatus::Started,
            reason: Some("parent is continuing replacement".to_owned()),
            updated_at_ms: None,
        }),
    ));

    let summary = agent_graph_product_summary_from_entries(&entries)
        .expect("entry projection should produce summary");

    assert_eq!(summary.active_agents, 1);
    assert_eq!(summary.terminal_agents, 0);
    Ok(())
}

fn agent_entries(
    workspace_root: &std::path::Path,
    status: AgentThreadStatus,
) -> Result<Vec<SessionLogEntry>> {
    let profile_id = AgentProfileId::new("explore")?;
    let snapshot_id = AgentProfileSnapshotId::new("snapshot_explore_1")?;
    let thread_id = AgentThreadId::new("thread_1")?;
    Ok(vec![
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            AgentProfileCapturedEntry {
                snapshot: AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
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
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
            thread_id: thread_id.clone(),
            parent_thread_id: Some(AgentThreadId::new("main")?),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            thread_session_ref: SessionRef::new_relative("children/thread_1.jsonl")?,
            profile_id,
            profile_snapshot_id: snapshot_id.clone(),
            run_context: AgentRunContextSnapshot {
                profile_snapshot_id: snapshot_id,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                reasoning_effort: None,
                workspace_root: WorkspaceRootSnapshot::new(workspace_root.display().to_string())?,
                effective_tool_scope_hash: "sha256:tools".to_owned(),
                effective_permission_policy_hash: "sha256:permissions".to_owned(),
                effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                provider_capability_hash: "sha256:provider".to_owned(),
                model_visible_agent_index_hash: Some("sha256:index".to_owned()),
                budget_policy_hash: "sha256:budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "inspect kernel".to_owned(),
            prompt_hash: "sha256:prompt".to_owned(),
            invocation_mode: AgentInvocationMode::Background,
            invocation_source: AgentInvocationSource::Chat,
            display_name: Some("kernel map".to_owned()),
            created_at_ms: Some(42),
        })),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            AgentThreadStatusChangedEntry {
                thread_id,
                status,
                reason: None,
                updated_at_ms: None,
            },
        )),
        SessionLogEntry::Assistant(ModelMessage::assistant(Some("done".to_owned()), Vec::new())),
    ])
}
