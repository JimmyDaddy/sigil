use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use sigil_kernel::{AgentInvocationMode, AgentThreadId};

use super::{
    AgentChatChildStart, AgentSupervisor, AgentTaskChildStart,
    begin::begin_attempt_id,
    chat_agent_thread_id_for_call,
    ids::{agent_thread_id_for_task_child, profile_id_for_role},
    thread_state::ActiveAgentThread,
};

/// Atomic runtime-slot reservation for one joined child-agent batch.
pub(crate) struct AgentChildBatchReservation {
    supervisor: AgentSupervisor,
    thread_ids: BTreeSet<AgentThreadId>,
    committed: bool,
}

impl AgentChildBatchReservation {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for AgentChildBatchReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Ok(mut state) = self.supervisor.state.lock() {
            for thread_id in &self.thread_ids {
                state.active_threads.remove(thread_id);
            }
        }
    }
}

impl AgentSupervisor {
    /// Reserves every runtime slot for one join-before-final batch or reserves none.
    pub(crate) fn reserve_chat_child_batch(
        &self,
        starts: &[AgentChatChildStart],
    ) -> Result<AgentChildBatchReservation> {
        if starts.is_empty() {
            bail!("agent child batch cannot be empty");
        }
        let mut candidates = Vec::with_capacity(starts.len());
        let mut thread_ids = BTreeSet::new();
        for start in starts {
            if start.invocation_mode != AgentInvocationMode::JoinBeforeFinal {
                bail!("agent child batch only supports join-before-final participants");
            }
            if start.parent_depth >= self.budget.max_depth {
                bail!(
                    "agent depth budget exceeded: max_depth={}",
                    self.budget.max_depth
                );
            }
            let thread_id = chat_agent_thread_id_for_call(&start.call_id, &start.profile_id)?;
            if !thread_ids.insert(thread_id.clone()) {
                bail!(
                    "agent child batch contains duplicate thread {}",
                    thread_id.as_str()
                );
            }
            candidates.push((
                thread_id.clone(),
                ActiveAgentThread {
                    profile_id: start.profile_id.clone(),
                    attempt_id: begin_attempt_id(&thread_id)?,
                    background: false,
                    mailbox_tx: None,
                    batch_reserved: true,
                },
            ));
        }

        self.reserve_child_batch_candidates(candidates, thread_ids)
    }

    /// Reserves every runtime slot for one task-owned discovery batch or reserves none.
    pub(crate) fn reserve_task_child_batch(
        &self,
        starts: &[AgentTaskChildStart],
    ) -> Result<AgentChildBatchReservation> {
        if starts.is_empty() {
            bail!("agent task child batch cannot be empty");
        }
        let mut candidates = Vec::with_capacity(starts.len());
        let mut thread_ids = BTreeSet::new();
        for start in starts {
            if start.invocation_mode != AgentInvocationMode::JoinBeforeFinal {
                bail!("agent task child batch only supports join-before-final participants");
            }
            if start.parent_depth >= self.budget.max_depth {
                bail!(
                    "agent depth budget exceeded: max_depth={}",
                    self.budget.max_depth
                );
            }
            let thread_id = agent_thread_id_for_task_child(
                &start.task_id,
                start.plan_version,
                &start.step,
                &start.child_task_id,
            )?;
            if !thread_ids.insert(thread_id.clone()) {
                bail!(
                    "agent task child batch contains duplicate thread {}",
                    thread_id.as_str()
                );
            }
            candidates.push((
                thread_id.clone(),
                ActiveAgentThread {
                    profile_id: profile_id_for_role(start.role)?,
                    attempt_id: begin_attempt_id(&thread_id)?,
                    background: false,
                    mailbox_tx: None,
                    batch_reserved: true,
                },
            ));
        }

        self.reserve_child_batch_candidates(candidates, thread_ids)
    }

    fn reserve_child_batch_candidates(
        &self,
        candidates: Vec<(AgentThreadId, ActiveAgentThread)>,
        thread_ids: BTreeSet<AgentThreadId>,
    ) -> Result<AgentChildBatchReservation> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("agent supervisor state lock poisoned"))?;
        let requested_total = state
            .active_threads
            .len()
            .checked_add(candidates.len())
            .ok_or_else(|| anyhow!("agent thread budget calculation overflowed"))?;
        if requested_total > self.budget.max_subagents {
            bail!(
                "agent thread budget exceeded: active={} requested={} [task].max_subagents={}",
                state.active_threads.len(),
                candidates.len(),
                self.budget.max_subagents
            );
        }
        if let Some((thread_id, _)) = candidates
            .iter()
            .find(|(thread_id, _)| state.active_threads.contains_key(thread_id))
        {
            bail!("agent thread {} is already active", thread_id.as_str());
        }
        for (thread_id, active) in candidates {
            state.active_threads.insert(thread_id, active);
        }
        drop(state);

        Ok(AgentChildBatchReservation {
            supervisor: self.clone(),
            thread_ids,
            committed: false,
        })
    }
}
