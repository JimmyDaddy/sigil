use crate::{
    ControlEntry, DurableEventType, EventId, JobIntentEntry, JsonlSessionStore, ResumeDisposition,
    Session, SessionLogEntry, StepLeaseEntry, StepLeaseHeartbeatEntry, StepLeaseStatus, ToolEffect,
};

fn sample_job_intent(job_id: &str) -> JobIntentEntry {
    JobIntentEntry {
        job_id: job_id.to_owned(),
        session_id: "session-resume".to_owned(),
        task_id: None,
        agent_profile: None,
        user_goal_event_id: EventId::from("event-user-goal"),
        tool_policy_hash: "policy-hash".to_owned(),
        expected_effect: ToolEffect::WorkspaceWrite,
        created_at_ms: Some(1_000),
    }
}

fn sample_step_lease(job_id: &str, deadline_ms: u64) -> StepLeaseEntry {
    StepLeaseEntry {
        lease_id: "lease-1".to_owned(),
        job_id: job_id.to_owned(),
        step_id: None,
        owner_process_id: "pid:123".to_owned(),
        deadline_ms,
        heartbeat_event_id: None,
        status: StepLeaseStatus::Acquired,
        updated_at_ms: Some(1_500),
        reason: None,
    }
}

fn sample_heartbeat(job_id: &str, next_deadline_ms: u64) -> StepLeaseHeartbeatEntry {
    StepLeaseHeartbeatEntry {
        lease_id: "lease-1".to_owned(),
        job_id: job_id.to_owned(),
        owner_process_id: "pid:123".to_owned(),
        observed_at_ms: 2_500,
        next_deadline_ms,
        heartbeat_event_id: Some("heartbeat-event-1".to_owned()),
    }
}

#[test]
fn resume_job_intent_projection_marks_expired_lease_interrupted() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::JobIntentRecorded(sample_job_intent("job-1"))),
        SessionLogEntry::Control(ControlEntry::StepLeaseRecorded(sample_step_lease(
            "job-1", 2_000,
        ))),
    ];

    let projection = crate::ResumeJobStateProjection::from_entries(&entries, 3_000);

    let job = projection.jobs.get("job-1").expect("job should project");
    assert_eq!(job.disposition, ResumeDisposition::InterruptedNeedsUser);
    assert_eq!(projection.stale_jobs().len(), 1);
}

#[test]
fn resume_lease_heartbeat_extends_matching_acquired_lease() {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::JobIntentRecorded(sample_job_intent("job-3"))),
        SessionLogEntry::Control(ControlEntry::StepLeaseRecorded(sample_step_lease(
            "job-3", 2_000,
        ))),
        SessionLogEntry::Control(ControlEntry::StepLeaseHeartbeatRecorded(sample_heartbeat(
            "job-3", 5_000,
        ))),
    ];

    let resumable = crate::ResumeJobStateProjection::from_entries(&entries, 3_000);
    let job = resumable.jobs.get("job-3").expect("job should project");
    assert_eq!(job.disposition, ResumeDisposition::Resumable);
    assert_eq!(
        job.lease.as_ref().and_then(|lease| lease.updated_at_ms),
        Some(2_500)
    );

    let expired = crate::ResumeJobStateProjection::from_entries(&entries, 6_000);
    let job = expired.jobs.get("job-3").expect("job should project");
    assert_eq!(job.disposition, ResumeDisposition::InterruptedNeedsUser);
}

#[test]
fn resume_lease_heartbeat_does_not_update_mismatched_owner() {
    let mut heartbeat = sample_heartbeat("job-4", 5_000);
    heartbeat.owner_process_id = "pid:other".to_owned();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::JobIntentRecorded(sample_job_intent("job-4"))),
        SessionLogEntry::Control(ControlEntry::StepLeaseRecorded(sample_step_lease(
            "job-4", 2_000,
        ))),
        SessionLogEntry::Control(ControlEntry::StepLeaseHeartbeatRecorded(heartbeat)),
    ];

    let projection = crate::ResumeJobStateProjection::from_entries(&entries, 3_000);

    let job = projection.jobs.get("job-4").expect("job should project");
    assert_eq!(job.disposition, ResumeDisposition::InterruptedNeedsUser);
    assert_eq!(
        job.lease.as_ref().map(|lease| lease.deadline_ms),
        Some(2_000)
    );
}

#[test]
fn resume_job_intent_roundtrips_as_durable_events() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session-resume.jsonl"))?;
    let mut session = Session::new("provider", "model").with_store(store.clone());
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "provider".to_owned(),
        model_name: "model".to_owned(),
    })?;
    session.append_control(ControlEntry::JobIntentRecorded(sample_job_intent("job-2")))?;
    session.append_control(ControlEntry::StepLeaseRecorded(sample_step_lease(
        "job-2", 2_000,
    )))?;
    session.append_control(ControlEntry::StepLeaseHeartbeatRecorded(sample_heartbeat(
        "job-2", 5_000,
    )))?;

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let event_types = records
        .iter()
        .filter_map(|record| {
            record
                .domain_event_record()
                .expect("record should decode")
                .map(|event| event.event.event_type().as_str().to_owned())
        })
        .collect::<Vec<_>>();
    assert!(event_types.contains(&DurableEventType::JobIntentRecorded.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::StepLeaseRecorded.as_str().to_owned()));
    assert!(
        event_types.contains(
            &DurableEventType::StepLeaseHeartbeatRecorded
                .as_str()
                .to_owned()
        )
    );

    let restored = Session::load_from_store("fallback", "fallback", store)?;
    let projection = restored
        .try_resume_job_state_projection_from_durable(3_000)?
        .expect("durable projection should be available");

    let job = projection.jobs.get("job-2").expect("job should project");
    assert_eq!(job.intent.job_id, "job-2");
    assert_eq!(job.disposition, ResumeDisposition::Resumable);
    Ok(())
}
