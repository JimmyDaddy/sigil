use std::{collections::BTreeMap, fs};

use super::*;
use crate::app::tests::common::test_config;
use sigil_kernel::ModelMessage;
use tempfile::tempdir;

fn sync_child(
    app: &mut AppState,
    status: sigil_kernel::TaskChildSessionStatus,
    child_ref: sigil_kernel::SessionRef,
) -> anyhow::Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "inspect".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: child_ref.clone(),
                role: sigil_kernel::AgentRole::SubagentRead,
                status,
                summary_hash: None,
            },
        )),
    ]);
    app.agent_panel.active_view = AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: child_ref,
    };
    Ok(())
}

#[test]
fn active_agent_terminal_status_and_transcript_error_paths_are_bounded() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join(".sigil/sessions");
    let child_dir = session_dir.join("children/child_dir.jsonl");
    fs::create_dir_all(&child_dir)?;

    let child_ref = sigil_kernel::SessionRef::new_relative("children/child_dir.jsonl")?;
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &test_config());
    app.session_log_path = session_dir.join("parent.jsonl");
    sync_child(
        &mut app,
        sigil_kernel::TaskChildSessionStatus::Completed,
        child_ref,
    )?;

    assert!(app.active_agent_view_is_terminal());
    app.reload_active_agent_child_transcript();
    let transcript = app
        .agent_panel
        .active_child_transcript
        .as_ref()
        .expect("load error should still create a transcript state");
    assert!(transcript.load_error.is_some());
    assert!(transcript.timeline_entries.is_empty());
    assert_eq!(transcript.total_timeline_entries, 0);

    app.rerender_active_agent_child_transcript();
    assert!(
        app.agent_panel
            .active_child_transcript
            .as_ref()
            .expect("transcript should remain available")
            .rendered_body_lines
            .is_empty()
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let blocked_dir = session_dir.join("blocked");
        fs::create_dir_all(&blocked_dir)?;
        fs::set_permissions(&blocked_dir, fs::Permissions::from_mode(0o000))?;
        let blocked_ref = sigil_kernel::SessionRef::new_relative("blocked/child.jsonl")?;
        sync_child(
            &mut app,
            sigil_kernel::TaskChildSessionStatus::Completed,
            blocked_ref,
        )?;
        app.agent_panel.active_child_transcript = None;
        app.reload_active_agent_child_transcript();
        fs::set_permissions(&blocked_dir, fs::Permissions::from_mode(0o700))?;

        let transcript = app
            .agent_panel
            .active_child_transcript
            .as_ref()
            .expect("metadata error should still create transcript state");
        assert!(transcript.load_error.is_some());
        assert!(transcript.timeline_entries.is_empty());
    }

    let missing_ref = sigil_kernel::SessionRef::new_relative("children/missing.jsonl")?;
    sync_child(
        &mut app,
        sigil_kernel::TaskChildSessionStatus::Started,
        missing_ref,
    )?;
    assert!(!app.active_agent_view_is_terminal());
    Ok(())
}

#[test]
fn agent_thread_sidebar_detail_handles_fully_delivered_result_projection() -> anyhow::Result<()> {
    let thread_id = sigil_kernel::AgentThreadId::new("agent_done")?;
    let session_ref = sigil_kernel::SessionRef::new_relative("children/agent_done.jsonl")?;
    let thread = sigil_kernel::AgentThreadProjection {
        thread_id: thread_id.clone(),
        parent_thread_id: None,
        parent_session_ref: None,
        thread_session_ref: Some(session_ref.clone()),
        profile_id: Some(sigil_kernel::AgentProfileId::new("explore")?),
        profile_snapshot_id: None,
        run_context: None,
        objective: "inspect result".to_owned(),
        prompt_hash: "sha256:prompt".to_owned(),
        invocation_mode: Some(sigil_kernel::AgentInvocationMode::JoinBeforeFinal),
        invocation_source: Some(sigil_kernel::AgentInvocationSource::Chat),
        display_name: None,
        status: sigil_kernel::AgentThreadStatus::Completed,
        reason: None,
        result: Some(sigil_kernel::AgentThreadResult {
            thread_id,
            session_ref,
            status: sigil_kernel::AgentThreadTerminalStatus::Completed,
            summary: "done".to_owned(),
            summary_truncated: false,
            original_summary_chars: None,
            artifacts: Vec::new(),
            changed_paths: Vec::new(),
            risks: Vec::new(),
            followups: Vec::new(),
            usage: None,
            output_hash: "sha256:done".to_owned(),
            final_answer_ref: None,
        }),
        result_delivered: true,
        result_fully_delivered: true,
        result_delivered_chars: 40_000,
        result_delivery_call_ids: vec!["call-read-result".to_owned()],
        attempts: BTreeMap::new(),
        merge_safe_points: Vec::new(),
        duplicate_terminal_entries: 0,
        closed: false,
        unresolved: false,
        profile_snapshot_missing: false,
        profile_snapshot_mismatch: false,
    };

    let detail = agent_thread_sidebar_detail(&thread, None, false);

    assert!(detail.contains("completed"));
    assert!(detail.contains("join-before-final chat"));
    assert!(detail.contains("result ready"));
    Ok(())
}

#[test]
fn child_transcript_readers_cover_invalid_paths_blank_lines_and_tail_truncation()
-> anyhow::Result<()> {
    let temp = tempdir()?;
    let invalid_path = std::path::PathBuf::from("bad\0path");
    assert!(
        child_transcript_file_signature(&invalid_path)
            .expect_err("nul byte path should fail")
            .to_string()
            .contains("failed to stat child session")
    );

    let path = temp.path().join("child.jsonl");
    fs::write(&path, "\n")?;
    JsonlSessionStore::new(&path)?.append(&SessionLogEntry::User(ModelMessage::user("hello")))?;
    let signature = child_transcript_file_signature(&path)?;
    let recent = read_recent_session_entries(&path, 8, signature)?;
    assert_eq!(recent.entries.len(), 1);
    assert!(!recent.truncated);

    let long_path = temp.path().join("long.jsonl");
    let long_store = JsonlSessionStore::new(&long_path)?;
    for index in 0..96 {
        long_store.append(&SessionLogEntry::User(ModelMessage::user(format!(
            "line {index}"
        ))))?;
    }
    let signature = child_transcript_file_signature(&long_path)?;
    let recent = read_recent_session_entries(&long_path, 2, signature)?;
    assert_eq!(recent.entries.len(), 2);
    assert!(recent.truncated);
    Ok(())
}

#[test]
fn bounded_composer_agent_rows_uses_contiguous_window_around_selection() {
    let rows = (0..6)
        .map(|index| SidebarAgentRow {
            label: if index == 0 {
                "main".to_owned()
            } else {
                format!("child {index}")
            },
            detail: if index == 0 {
                "idle in current session".to_owned()
            } else {
                "completed · explore · mention".to_owned()
            },
            selected: index == 3,
            active: index == 2,
            muted: false,
        })
        .collect::<Vec<_>>();

    let bounded = bounded_composer_agent_rows(rows);

    assert_eq!(
        bounded
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["main", "child 1", "child 2", "child 3"]
    );
}

#[test]
fn bounded_composer_agent_rows_scrolls_to_late_selection() {
    let rows = (0..7)
        .map(|index| SidebarAgentRow {
            label: if index == 0 {
                "main".to_owned()
            } else {
                format!("child {index}")
            },
            detail: "agent".to_owned(),
            selected: index == 6,
            active: index == 2,
            muted: false,
        })
        .collect::<Vec<_>>();

    let bounded = bounded_composer_agent_rows(rows);

    assert_eq!(
        bounded
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        vec!["child 3", "child 4", "child 5", "child 6"]
    );
}
