use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    AgentProfileId, EventId, SessionId, SessionLogEntry, TaskId, TaskStepId, ToolEffect,
    session::ControlEntry,
};

pub type JobId = String;
pub type LeaseId = String;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepLeaseStatus {
    #[default]
    Acquired,
    Released,
    Interrupted,
    Abandoned,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResumeDisposition {
    Resumable,
    InterruptedNeedsUser,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct JobIntentEntry {
    pub job_id: JobId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile: Option<AgentProfileId>,
    pub user_goal_event_id: EventId,
    pub tool_policy_hash: String,
    pub expected_effect: ToolEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct StepLeaseEntry {
    pub lease_id: LeaseId,
    pub job_id: JobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<TaskStepId>,
    pub owner_process_id: String,
    pub deadline_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_event_id: Option<EventId>,
    #[serde(default)]
    pub status: StepLeaseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct StepLeaseHeartbeatEntry {
    pub lease_id: LeaseId,
    pub job_id: JobId,
    pub owner_process_id: String,
    pub observed_at_ms: u64,
    pub next_deadline_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_event_id: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeJobProjection {
    pub intent: JobIntentEntry,
    pub lease: Option<StepLeaseEntry>,
    pub disposition: ResumeDisposition,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumeJobStateProjection {
    pub jobs: BTreeMap<JobId, ResumeJobProjection>,
}

impl ResumeJobStateProjection {
    #[must_use]
    pub fn from_entries(entries: &[SessionLogEntry], now_ms: u64) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(control, now_ms);
            }
        }
        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry, now_ms: u64) {
        match control {
            ControlEntry::JobIntentRecorded(intent) => {
                self.jobs.insert(
                    intent.job_id.clone(),
                    ResumeJobProjection {
                        intent: intent.clone(),
                        lease: None,
                        disposition: ResumeDisposition::Resumable,
                    },
                );
            }
            ControlEntry::StepLeaseRecorded(lease) => {
                if let Some(job) = self.jobs.get_mut(&lease.job_id) {
                    job.lease = Some(lease.clone());
                    job.disposition = resume_disposition(job.lease.as_ref(), now_ms);
                }
            }
            ControlEntry::StepLeaseHeartbeatRecorded(heartbeat) => {
                if let Some(job) = self.jobs.get_mut(&heartbeat.job_id)
                    && let Some(lease) = &mut job.lease
                    && lease.lease_id == heartbeat.lease_id
                    && lease.owner_process_id == heartbeat.owner_process_id
                    && lease.status == StepLeaseStatus::Acquired
                {
                    lease.deadline_ms = heartbeat.next_deadline_ms;
                    lease.heartbeat_event_id = heartbeat.heartbeat_event_id.clone();
                    lease.updated_at_ms = Some(heartbeat.observed_at_ms);
                    job.disposition = resume_disposition(job.lease.as_ref(), now_ms);
                }
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn stale_jobs(&self) -> Vec<&ResumeJobProjection> {
        self.jobs
            .values()
            .filter(|job| job.disposition == ResumeDisposition::InterruptedNeedsUser)
            .collect()
    }
}

fn resume_disposition(lease: Option<&StepLeaseEntry>, now_ms: u64) -> ResumeDisposition {
    let Some(lease) = lease else {
        return ResumeDisposition::Resumable;
    };
    match lease.status {
        StepLeaseStatus::Released | StepLeaseStatus::Abandoned => ResumeDisposition::Abandoned,
        StepLeaseStatus::Interrupted => ResumeDisposition::InterruptedNeedsUser,
        StepLeaseStatus::Acquired if lease.deadline_ms <= now_ms => {
            ResumeDisposition::InterruptedNeedsUser
        }
        StepLeaseStatus::Acquired => ResumeDisposition::Resumable,
    }
}

#[cfg(test)]
#[path = "tests/resume_tests.rs"]
mod tests;
