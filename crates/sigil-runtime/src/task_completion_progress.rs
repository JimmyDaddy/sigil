use std::sync::{Arc, Mutex};

use sigil_kernel::{TaskId, TaskStepId};

/// Terminal outcome observed before the parent single writer commits the child result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCompletionOutcome {
    Succeeded,
    Failed,
}

impl TaskCompletionOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "ok",
            Self::Failed => "failed",
        }
    }
}

/// Process-local dual-order progress for one parallel task participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletionProgressMember {
    pub step_id: String,
    pub title: String,
    /// One-based stable request order used by the durable parent single writer.
    pub request_order: usize,
    /// One-based terminal arrival order, when the participant has settled.
    pub arrival_order: Option<usize>,
    pub outcome: Option<TaskCompletionOutcome>,
}

/// Process-local progress for the latest parallel task batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCompletionProgress {
    pub generation: u64,
    pub task_id: String,
    pub plan_version: u32,
    pub arrived: usize,
    pub total: usize,
    pub members: Vec<TaskCompletionProgressMember>,
}

/// Deduplicatable process-local task completion snapshot.
///
/// Arrival order is observational only. It must not be persisted or used as restart authority;
/// durable parent commits continue to follow `request_order`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskCompletionProgressSnapshot {
    pub batch: Option<TaskCompletionProgress>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskCompletionProgressRegistry {
    state: Arc<Mutex<TaskCompletionProgressState>>,
}

impl Default for TaskCompletionProgressRegistry {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(TaskCompletionProgressState::default())),
        }
    }
}

#[derive(Debug, Default)]
struct TaskCompletionProgressState {
    next_generation: u64,
    batch: Option<TaskCompletionProgress>,
}

pub(crate) struct TaskCompletionProgressRegistration {
    pub(crate) step_id: TaskStepId,
    pub(crate) title: String,
}

impl TaskCompletionProgressRegistry {
    pub(crate) fn begin(
        &self,
        task_id: &TaskId,
        plan_version: u32,
        registrations: Vec<TaskCompletionProgressRegistration>,
    ) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        state.next_generation = state.next_generation.saturating_add(1).max(1);
        let generation = state.next_generation;
        let members = registrations
            .into_iter()
            .enumerate()
            .map(
                |(request_index, registration)| TaskCompletionProgressMember {
                    step_id: registration.step_id.as_str().to_owned(),
                    title: registration.title,
                    request_order: request_index.saturating_add(1),
                    arrival_order: None,
                    outcome: None,
                },
            )
            .collect::<Vec<_>>();
        state.batch = Some(TaskCompletionProgress {
            generation,
            task_id: task_id.as_str().to_owned(),
            plan_version,
            arrived: 0,
            total: members.len(),
            members,
        });
        generation
    }

    pub(crate) fn record_arrival(
        &self,
        generation: u64,
        request_index: usize,
        completion_index: usize,
        outcome: TaskCompletionOutcome,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(batch) = state
            .batch
            .as_mut()
            .filter(|batch| batch.generation == generation)
        else {
            return;
        };
        let Some(member) = batch.members.get_mut(request_index) else {
            return;
        };
        if member.arrival_order.is_some() {
            return;
        }
        member.arrival_order = Some(completion_index.saturating_add(1));
        member.outcome = Some(outcome);
        batch.arrived = batch.arrived.saturating_add(1).min(batch.total);
    }

    pub(crate) fn snapshot(&self) -> TaskCompletionProgressSnapshot {
        let batch = self.state.lock().ok().and_then(|state| state.batch.clone());
        TaskCompletionProgressSnapshot { batch }
    }
}

#[cfg(test)]
#[path = "tests/task_completion_progress_tests.rs"]
mod tests;
