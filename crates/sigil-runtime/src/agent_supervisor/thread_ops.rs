use std::sync::mpsc;

use anyhow::Result;
use sigil_kernel::{
    AgentInvocationMode, AgentProfileId, AgentRole, AgentRunAttemptId, AgentRunInterruptedEntry,
    AgentThreadId, AgentThreadStatus, AgentThreadStatusChangedEntry, ControlEntry, EventHandler,
    Session, TaskId,
};

use super::{
    AgentInterruptedThread, AgentMailboxMessage, AgentSupervisor, ForegroundCancelImpact,
    append_control, thread_state::ActiveAgentThread,
};

impl AgentSupervisor {
    pub fn send_agent_message(
        &self,
        target_thread_id: &AgentThreadId,
        message: AgentMailboxMessage,
    ) -> std::result::Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "agent supervisor state lock poisoned".to_owned())?;
        let Some(thread) = state.active_threads.get(target_thread_id) else {
            return Err("agent thread is not active".to_owned());
        };
        let Some(sender) = thread.mailbox_tx.as_ref() else {
            return Err("agent thread has no active mailbox".to_owned());
        };
        sender
            .send(message)
            .map_err(|_| "agent mailbox is closed".to_owned())
    }

    #[must_use]
    pub fn cancel_foreground_run(&self) -> ForegroundCancelImpact {
        let foreground_children_interrupted = self
            .state
            .lock()
            .map(|mut state| {
                let thread_ids = state
                    .active_threads
                    .iter()
                    .filter(|(_, thread)| !thread.background)
                    .map(|(thread_id, _)| thread_id.clone())
                    .collect::<Vec<_>>();
                thread_ids
                    .into_iter()
                    .filter_map(|thread_id| {
                        state.active_threads.remove(&thread_id).map(|thread| {
                            AgentInterruptedThread {
                                thread_id,
                                attempt_id: thread.attempt_id,
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ForegroundCancelImpact {
            foreground_children_interrupted,
            background_children_cancelled: 0,
        }
    }

    pub fn request_foreground_background(&self) -> std::result::Result<AgentThreadId, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "agent supervisor state lock poisoned".to_owned())?;
        let Some(thread_id) = state
            .active_threads
            .iter()
            .find(|(_, thread)| !thread.background)
            .map(|(thread_id, _)| thread_id.clone())
        else {
            return Err("no foreground child agent is currently running".to_owned());
        };
        if let Some(thread) = state.active_threads.get_mut(&thread_id) {
            thread.background = true;
        }
        Ok(thread_id)
    }

    #[must_use]
    pub fn active_profile_ids(&self) -> Vec<AgentProfileId> {
        self.state
            .lock()
            .map(|state| {
                state
                    .active_threads
                    .values()
                    .map(|thread| thread.profile_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn reserve_thread(
        &self,
        thread_id: &AgentThreadId,
        attempt_id: &AgentRunAttemptId,
        profile_id: &AgentProfileId,
        _task_id: &TaskId,
        _role: AgentRole,
        invocation_mode: AgentInvocationMode,
        parent_depth: usize,
        mailbox_tx: Option<mpsc::Sender<AgentMailboxMessage>>,
    ) -> std::result::Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "agent supervisor state lock poisoned".to_owned())?;
        if let Some(active) = state.active_threads.get_mut(thread_id) {
            if active.batch_reserved
                && active.profile_id == *profile_id
                && active.attempt_id == *attempt_id
            {
                active.batch_reserved = false;
                active.background = matches!(invocation_mode, AgentInvocationMode::Background);
                active.mailbox_tx = mailbox_tx;
                return Ok(());
            }
            return Err(format!(
                "agent thread {} is already active",
                thread_id.as_str()
            ));
        }
        if state.active_threads.len() >= self.budget.max_subagents {
            return Err(format!(
                "agent thread budget exceeded: [task].max_subagents={}",
                self.budget.max_subagents
            ));
        }
        if parent_depth >= self.budget.max_depth {
            return Err(format!(
                "agent depth budget exceeded: max_depth={}",
                self.budget.max_depth
            ));
        }
        state.active_threads.insert(
            thread_id.clone(),
            ActiveAgentThread {
                profile_id: profile_id.clone(),
                attempt_id: attempt_id.clone(),
                background: matches!(invocation_mode, AgentInvocationMode::Background),
                mailbox_tx,
                batch_reserved: false,
            },
        );
        Ok(())
    }

    pub(super) fn release_thread(&self, thread_id: &AgentThreadId) {
        if let Ok(mut state) = self.state.lock() {
            state.active_threads.remove(thread_id);
        }
    }

    pub(crate) fn release_runtime_thread(&self, thread_id: &AgentThreadId) {
        self.release_thread(thread_id);
    }

    pub fn append_foreground_cancel_audit<H>(
        session: &mut Session,
        handler: &mut H,
        impact: ForegroundCancelImpact,
        reason: &str,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        for interrupted in impact.foreground_children_interrupted {
            append_control(
                session,
                handler,
                ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
                    thread_id: interrupted.thread_id.clone(),
                    attempt_id: interrupted.attempt_id,
                    reason: reason.to_owned(),
                }),
            )?;
            append_control(
                session,
                handler,
                ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                    thread_id: interrupted.thread_id,
                    status: AgentThreadStatus::Interrupted,
                    reason: Some(reason.to_owned()),
                    updated_at_ms: None,
                }),
            )?;
        }
        Ok(())
    }
}
