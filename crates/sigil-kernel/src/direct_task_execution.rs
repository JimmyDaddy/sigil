use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{PlanId, TaskId, TaskParticipantAttemptStatus, safe_persistence_text, sha256_hex};

/// Current durable schema for direct Task execution admission.
pub const TASK_DIRECT_EXECUTION_ADMISSION_SCHEMA_VERSION: u16 = 1;

/// The host-owned authority that selected direct execution instead of a scheduled TaskPlan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum TaskDirectExecutionSourceV1 {
    /// The user approved one exact durable Plan artifact.
    ApprovedPlan { plan_id: PlanId, plan_hash: String },
    /// An optional model planner failed to produce a valid TaskPlan, so the host preserved basic
    /// execution instead of failing the Task.
    PlannerFallback { planner_attempt_id: String },
}

impl TaskDirectExecutionSourceV1 {
    fn stable_material(&self) -> String {
        match self {
            Self::ApprovedPlan { plan_id, plan_hash } => {
                format!("approved_plan\n{}\n{plan_hash}", plan_id.as_str())
            }
            Self::PlannerFallback { planner_attempt_id } => {
                format!("planner_fallback\n{planner_attempt_id}")
            }
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::ApprovedPlan { plan_hash, .. } => validate_sha256("plan hash", plan_hash),
            Self::PlannerFallback { planner_attempt_id } => {
                validate_stable_token("planner attempt id", planner_attempt_id)
            }
        }
    }
}

/// First-class admission for executing the complete durable Task objective without a TaskPlan.
///
/// This record is execution authority, not a hidden one-step plan and not a user-facing
/// checklist. It carries no model-authored steps, dependencies, roles, or capability claims.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskDirectExecutionAdmittedV1 {
    pub schema_version: u16,
    pub admission_id: String,
    pub task_id: TaskId,
    pub objective_hash: String,
    pub source: TaskDirectExecutionSourceV1,
    pub admitted_at_ms: u64,
}

impl TaskDirectExecutionAdmittedV1 {
    /// Creates direct execution authority for one user-approved Plan.
    #[must_use]
    pub fn approved_plan(
        task_id: TaskId,
        objective: &str,
        plan_id: PlanId,
        plan_hash: String,
        admitted_at_ms: u64,
    ) -> Self {
        Self::new(
            task_id,
            objective,
            TaskDirectExecutionSourceV1::ApprovedPlan { plan_id, plan_hash },
            admitted_at_ms,
        )
    }

    /// Creates direct execution authority after an optional planner failed.
    #[must_use]
    pub fn planner_fallback(
        task_id: TaskId,
        objective: &str,
        planner_attempt_id: impl Into<String>,
        admitted_at_ms: u64,
    ) -> Self {
        Self::new(
            task_id,
            objective,
            TaskDirectExecutionSourceV1::PlannerFallback {
                planner_attempt_id: planner_attempt_id.into(),
            },
            admitted_at_ms,
        )
    }

    fn new(
        task_id: TaskId,
        objective: &str,
        source: TaskDirectExecutionSourceV1,
        admitted_at_ms: u64,
    ) -> Self {
        let objective_hash = task_direct_execution_objective_hash(objective);
        let admission_id = crate::stable_event_uuid(
            "sigil-task-direct-execution-admission-v1",
            &format!(
                "{}\n{}\n{}",
                task_id.as_str(),
                objective_hash,
                source.stable_material()
            ),
        );
        Self {
            schema_version: TASK_DIRECT_EXECUTION_ADMISSION_SCHEMA_VERSION,
            admission_id,
            task_id,
            objective_hash,
            source,
            admitted_at_ms,
        }
    }

    /// Validates bounded identity and source facts before append or replay.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TASK_DIRECT_EXECUTION_ADMISSION_SCHEMA_VERSION {
            bail!("unsupported direct Task execution admission schema version");
        }
        validate_sha256("direct execution objective hash", &self.objective_hash)?;
        self.source.validate()?;
        let expected = crate::stable_event_uuid(
            "sigil-task-direct-execution-admission-v1",
            &format!(
                "{}\n{}\n{}",
                self.task_id.as_str(),
                self.objective_hash,
                self.source.stable_material()
            ),
        );
        if self.admission_id != expected {
            bail!("direct Task execution admission identity does not match its authority facts");
        }
        Ok(())
    }

    /// Confirms that the admission is bound to the current durable objective.
    #[must_use]
    pub fn matches_objective(&self, objective: &str) -> bool {
        self.objective_hash == task_direct_execution_objective_hash(objective)
    }
}

/// One durable physical direct-execution attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskDirectExecutionAttemptV1 {
    pub attempt_id: String,
    pub task_id: TaskId,
    pub admission_id: String,
    pub ordinal: u32,
    pub status: TaskParticipantAttemptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Exact durable Assistant message produced by a completed direct execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message_id: Option<String>,
    /// Hash of the safely persisted final answer bound to `final_message_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
}

impl TaskDirectExecutionAttemptV1 {
    /// Creates a started attempt bound to one exact direct-execution admission.
    #[must_use]
    pub fn started(admission: &TaskDirectExecutionAdmittedV1, ordinal: u32) -> Self {
        Self {
            attempt_id: task_direct_execution_attempt_id(
                &admission.task_id,
                &admission.admission_id,
                ordinal,
            ),
            task_id: admission.task_id.clone(),
            admission_id: admission.admission_id.clone(),
            ordinal,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
            final_message_id: None,
            output_hash: None,
        }
    }

    /// Validates the retry-stable attempt identity and bounded reason.
    pub fn validate(&self) -> Result<()> {
        if self.ordinal == 0 {
            bail!("direct Task execution attempt ordinal must start at one");
        }
        validate_stable_token("direct execution admission id", &self.admission_id)?;
        let expected =
            task_direct_execution_attempt_id(&self.task_id, &self.admission_id, self.ordinal);
        if self.attempt_id != expected {
            bail!("direct Task execution attempt identity does not match its durable facts");
        }
        if let Some(reason) = self.reason.as_deref()
            && safe_persistence_text(reason) != reason
        {
            bail!("direct Task execution attempt reason is not safely persistable");
        }
        match (&self.final_message_id, &self.output_hash) {
            (Some(message_id), Some(output_hash)) => {
                validate_stable_token("direct execution final message id", message_id)?;
                validate_sha256("direct execution output hash", output_hash)?;
                if self.status != TaskParticipantAttemptStatus::Completed {
                    bail!("only a completed direct execution attempt may bind a final answer");
                }
            }
            (None, None) if self.status != TaskParticipantAttemptStatus::Completed => {}
            (None, None) => {
                bail!("completed direct execution attempt must bind its final answer");
            }
            _ => bail!("direct execution final answer binding is incomplete"),
        }
        Ok(())
    }
}

/// Returns the stable logical-run identity for one direct execution attempt.
#[must_use]
pub fn task_direct_execution_logical_run_id(attempt_id: &str) -> String {
    format!("task-direct-execution-{attempt_id}")
}

/// Returns the stable attempt id for one admission generation and ordinal.
#[must_use]
pub fn task_direct_execution_attempt_id(
    task_id: &TaskId,
    admission_id: &str,
    ordinal: u32,
) -> String {
    crate::stable_event_uuid(
        "sigil-task-direct-execution-attempt-v1",
        &format!("{}\n{admission_id}\n{ordinal}", task_id.as_str()),
    )
}

fn task_direct_execution_objective_hash(objective: &str) -> String {
    format!(
        "sha256:{}",
        sha256_hex(safe_persistence_text(objective).as_bytes())
    )
}

fn validate_sha256(label: &str, value: &str) -> Result<()> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn validate_stable_token(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_whitespace)
        || safe_persistence_text(value) != value
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/direct_task_execution_tests.rs"]
mod tests;
