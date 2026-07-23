use anyhow::Result;

use crate::{
    ControlEntry, ConversationTurnRef, SessionLogEntry, TaskAdmissionReason, TaskAdmissionTrigger,
    TaskHandoffDecision, TaskHandoffId, TaskHandoffProjection, TaskHandoffRequestedEntry,
    TaskHandoffResolvedEntry, TaskId,
};

fn source_turn(message_id: &str) -> Result<ConversationTurnRef> {
    ConversationTurnRef::new("session-1", message_id, "foreground-run-1")
}

fn request(
    handoff_id: &str,
    source_turn: ConversationTurnRef,
) -> Result<TaskHandoffRequestedEntry> {
    Ok(TaskHandoffRequestedEntry {
        handoff_id: TaskHandoffId::new(handoff_id)?,
        source_turn,
        trigger: TaskAdmissionTrigger::ModelRequested,
        reason_codes: vec![TaskAdmissionReason::CrossLayer],
        recovery_objective: None,
        policy_snapshot_hash: "sha256:policy".to_owned(),
        requested_at_ms: 42,
    })
}

fn resolution(handoff_id: &str, task_id: &str) -> Result<TaskHandoffResolvedEntry> {
    Ok(TaskHandoffResolvedEntry {
        handoff_id: TaskHandoffId::new(handoff_id)?,
        decision: TaskHandoffDecision::Accepted,
        task_id: Some(TaskId::new(task_id)?),
        decided_at_ms: 43,
    })
}

#[test]
fn handoff_identifiers_and_source_turns_validate_shape() {
    assert!(TaskHandoffId::new("").is_err());
    assert!(TaskHandoffId::new("../handoff").is_err());
    assert!(ConversationTurnRef::new("", "message", "run").is_err());
    assert!(ConversationTurnRef::new("session", "message\n", "run").is_err());
    assert_eq!(
        TaskAdmissionReason::ParallelResearch.as_str(),
        "parallel_research"
    );
}

#[test]
fn handoff_projection_keeps_admission_separate_from_task_runs() -> Result<()> {
    let source = source_turn("message-1")?;
    let request = request("handoff-1", source.clone())?;
    let resolution = resolution("handoff-1", "task-handoff-1")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(request.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(resolution.clone())),
    ];

    let projection = TaskHandoffProjection::from_entries(&entries);
    let state = projection
        .handoff_for_source(&source)
        .expect("source turn should resolve to its handoff");
    assert_eq!(state.request.as_ref(), Some(&request));
    assert_eq!(state.resolution.as_ref(), Some(&resolution));
    assert_eq!(
        projection
            .accepted_tasks
            .get(resolution.task_id.as_ref().expect("accepted task id")),
        Some(&request.handoff_id)
    );
    assert!(!projection.has_conflicts());
    Ok(())
}

#[test]
fn duplicate_handoff_facts_are_idempotent_but_conflicts_fail_closed() -> Result<()> {
    let source = source_turn("message-1")?;
    let first_request = request("handoff-1", source.clone())?;
    let first_resolution = resolution("handoff-1", "task-handoff-1")?;
    let duplicate_entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(first_request.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(first_request.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(first_resolution.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(first_resolution)),
    ];
    let duplicate_projection = TaskHandoffProjection::from_entries(&duplicate_entries);
    let duplicate_state = duplicate_projection
        .handoffs
        .get(&first_request.handoff_id)
        .expect("handoff state");
    assert_eq!(duplicate_state.duplicate_requests, 1);
    assert_eq!(duplicate_state.duplicate_resolutions, 1);
    assert!(!duplicate_projection.has_conflicts());

    let conflicting_entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(first_request)),
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(request(
            "handoff-2",
            ConversationTurnRef::new(
                source.session_scope_id,
                source.message_id,
                "foreground-run-replayed",
            )?,
        )?)),
    ];
    let conflicting_projection = TaskHandoffProjection::from_entries(&conflicting_entries);
    assert!(conflicting_projection.has_conflicts());
    Ok(())
}

#[test]
fn accepted_and_rejected_resolution_shapes_are_strict() -> Result<()> {
    let accepted_without_task = TaskHandoffResolvedEntry {
        handoff_id: TaskHandoffId::new("handoff-1")?,
        decision: TaskHandoffDecision::Accepted,
        task_id: None,
        decided_at_ms: 43,
    };
    let rejected_with_task = TaskHandoffResolvedEntry {
        handoff_id: TaskHandoffId::new("handoff-2")?,
        decision: TaskHandoffDecision::Rejected,
        task_id: Some(TaskId::new("task-2")?),
        decided_at_ms: 44,
    };
    let projection = TaskHandoffProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(accepted_without_task)),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(rejected_with_task)),
    ]);
    assert_eq!(projection.conflicts.len(), 2);
    Ok(())
}
