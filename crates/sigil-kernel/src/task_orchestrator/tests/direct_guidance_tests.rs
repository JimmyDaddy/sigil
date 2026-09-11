use super::*;

#[test]
fn direct_guidance_acceptance_is_source_idempotent_across_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    let source =
        crate::ConversationTurnRef::new(session.session_scope_id(), "guidance-user", "root-run")?;
    let task = TaskId::new("task")?;
    accept_task_continuation_guidance(
        &mut session,
        &task,
        TaskRunStatus::Paused,
        "Use the approved recovery instructions",
        &source,
        &mut crate::NoopEventHandler,
    )?;
    let entries = session.entries().to_vec();
    let mut reloaded = Session::load_from_store("test", "model", store)?;
    accept_task_continuation_guidance(
        &mut reloaded,
        &task,
        TaskRunStatus::Paused,
        "Use the approved recovery instructions",
        &source,
        &mut crate::NoopEventHandler,
    )?;
    assert_eq!(
        serde_json::to_value(reloaded.entries())?,
        serde_json::to_value(&entries)?
    );
    assert_eq!(
        reloaded
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1
    );
    assert!(
        accept_task_continuation_guidance(
            &mut reloaded,
            &task,
            TaskRunStatus::Paused,
            "Conflicting text",
            &source,
            &mut crate::NoopEventHandler,
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(reloaded.entries())?,
        serde_json::to_value(&entries)?
    );
    Ok(())
}

#[test]
fn direct_guidance_rejects_foreign_source_without_writing() -> Result<()> {
    let mut session = Session::new("test", "model");
    let source = crate::ConversationTurnRef::new("other-session", "message", "run")?;
    let before = session.entries().to_vec();
    assert!(
        accept_task_continuation_guidance(
            &mut session,
            &TaskId::new("task")?,
            TaskRunStatus::Paused,
            "New guidance",
            &source,
            &mut crate::NoopEventHandler,
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(session.entries())?,
        serde_json::to_value(&before)?
    );
    Ok(())
}
