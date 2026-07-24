use anyhow::Result;

use crate::{
    AgentRole, ControlEntry, IntegrationBaseRepresentation, IntegrationLaneCandidate,
    IntegrationLaneChanged, IntegrationLaneId, IntegrationLaneSpec, IntegrationLaneStatus,
    IntegrationPlan, IntegrationPlanId, IntegrationPlanRecorded, PublicRunEventKind,
    PublicTaskEventProjector, PublicTaskPhase, SessionRef, TaskId, TaskIsolationMode,
    TaskParticipantAttemptEntry, TaskParticipantAttemptStatus, TaskParticipantPurpose,
    TaskPlanEntry, TaskPlanStatus, TaskStepId, TaskStepMode, TaskStepSpec,
    task_participant_attempt_id,
};

#[test]
fn projector_emits_typed_plan_batch_and_step_events_without_raw_detail() -> Result<()> {
    let task_id = TaskId::new("task_public")?;
    let step_id = TaskStepId::new("step_public")?;
    let mut projector = PublicTaskEventProjector::default();
    let plan_events = projector.project_control(&ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "Inspect public protocol".to_owned(),
            display_name: Some("Protocol Scout".to_owned()),
            detail: Some(
                "raw prompt and /private/worktree/refs/sigil/integration/secret".to_owned(),
            ),
            role: AgentRole::SubagentRead,
            depends_on: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: Some("raw planner transcript".to_owned()),
    }));

    assert!(matches!(
        &plan_events[..],
        [PublicRunEventKind::TaskPlanUpdated {
            task_id,
            plan_version: 1,
            status,
            steps,
        }] if task_id == "task_public"
            && status == "accepted"
            && steps[0].step_id == "step_public"
    ));
    let serialized = serde_json::to_string(&plan_events)?;
    assert!(!serialized.contains("raw prompt"));
    assert!(!serialized.contains("/private/worktree"));
    assert!(!serialized.contains("raw planner transcript"));

    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    let attempt_events = projector.project_control(&ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id,
            purpose: TaskParticipantPurpose::Step,
            ordinal: 1,
            plan_version: Some(1),
            step_id: Some(step_id),
            role: AgentRole::SubagentRead,
            child_session_ref: SessionRef::new_relative("child.jsonl")?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ));

    assert!(attempt_events.iter().any(|event| matches!(
        event,
        PublicRunEventKind::TaskPhaseChanged {
            phase: PublicTaskPhase::Execution,
            status,
            ..
        } if status == "started"
    )));
    assert!(attempt_events.iter().any(|event| matches!(
        event,
        PublicRunEventKind::TaskBatchChanged {
            active: 1,
            completed: 0,
            failed: 0,
            ..
        }
    )));
    assert!(attempt_events.iter().any(|event| matches!(
        event,
        PublicRunEventKind::TaskStepChanged {
            attempt_id: Some(public_attempt_id),
            ..
        } if public_attempt_id == attempt_id.as_str()
    )));
    Ok(())
}

#[test]
fn projector_joins_integration_lane_to_task_without_exposing_private_target() -> Result<()> {
    let task_id = TaskId::new("task_integration_public")?;
    let plan_id = IntegrationPlanId::new("plan_public")?;
    let lane_id = IntegrationLaneId::new("lane_public")?;
    let mut projector = PublicTaskEventProjector::default();
    let plan = IntegrationPlan {
        plan_id: plan_id.clone(),
        task_id: task_id.clone(),
        plan_version: 3,
        base_snapshot_id: "snapshot_public".to_owned(),
        base_representation: IntegrationBaseRepresentation::Unknown,
        proposals: Vec::new(),
        conflicts: Vec::new(),
        lanes: vec![IntegrationLaneSpec {
            lane_id: lane_id.clone(),
            proposals: Vec::new(),
            verification_scope_hashes: Vec::new(),
        }],
    };
    let phase_events = projector.project_control(&ControlEntry::IntegrationPlanRecorded(
        IntegrationPlanRecorded { plan },
    ));
    assert!(matches!(
        &phase_events[..],
        [PublicRunEventKind::TaskPhaseChanged {
            task_id: Some(task_id),
            phase: PublicTaskPhase::Integration,
            status,
        }] if task_id == "task_integration_public" && status == "planned"
    ));

    let private_ref = "refs/sigil/integration/private/lane";
    let lane_events = projector.project_control(&ControlEntry::IntegrationLaneChanged(
        IntegrationLaneChanged {
            plan_id,
            lane_id,
            status: IntegrationLaneStatus::Ready,
            candidate: Some(IntegrationLaneCandidate::ManagedRef {
                private_ref: private_ref.to_owned(),
                base_commit: "b".repeat(40),
                candidate_commit: "a".repeat(40),
                workspace_snapshot_id: "snapshot_private".to_owned(),
            }),
            verification_check_ids: vec!["check_public".to_owned()],
            reason: None,
        },
    ));

    assert!(matches!(
        &lane_events[..],
        [PublicRunEventKind::IntegrationLaneChanged {
            task_id,
            plan_version: 3,
            plan_id,
            lane_id,
            status,
            conflicts,
        }] if task_id == "task_integration_public"
            && plan_id == "plan_public"
            && lane_id == "lane_public"
            && status == "ready"
            && conflicts.is_empty()
    ));
    assert!(!serde_json::to_string(&lane_events)?.contains(private_ref));
    Ok(())
}
