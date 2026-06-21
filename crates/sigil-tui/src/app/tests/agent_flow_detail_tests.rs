use std::fs;

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
    app.active_agent_view = AgentView::Child {
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
        .active_agent_child_transcript
        .as_ref()
        .expect("load error should still create a transcript state");
    assert!(transcript.load_error.is_some());
    assert!(transcript.timeline_entries.is_empty());
    assert_eq!(transcript.total_timeline_entries, 0);

    app.rerender_active_agent_child_transcript();
    assert!(
        app.active_agent_child_transcript
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
        app.active_agent_child_transcript = None;
        app.reload_active_agent_child_transcript();
        fs::set_permissions(&blocked_dir, fs::Permissions::from_mode(0o700))?;

        let transcript = app
            .active_agent_child_transcript
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
    let valid = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("hello")))?;
    fs::write(&path, format!("\n{valid}\n"))?;
    let signature = child_transcript_file_signature(&path)?;
    let recent = read_recent_session_entries(&path, 8, signature)?;
    assert_eq!(recent.entries.len(), 1);
    assert!(!recent.truncated);

    let long_path = temp.path().join("long.jsonl");
    let mut long_body = String::new();
    for index in 0..1300 {
        let line = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user(format!(
            "line {index}"
        ))))?;
        long_body.push_str(&line);
        long_body.push('\n');
    }
    fs::write(&long_path, long_body)?;
    let signature = child_transcript_file_signature(&long_path)?;
    let recent = read_recent_session_entries(&long_path, 2, signature)?;
    assert_eq!(recent.entries.len(), 2);
    assert!(recent.truncated);
    Ok(())
}

#[test]
fn bounded_composer_agent_rows_keeps_main_active_selected_and_recent() {
    let rows = (0..6)
        .map(|index| SidebarAgentRow {
            label: if index == 0 {
                "main".to_owned()
            } else {
                format!("agent {index}")
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
        vec!["main", "agent 2", "agent 3", "agent 5"]
    );
}
