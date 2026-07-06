use anyhow::Result;
use sigil_kernel::{
    AgentMailboxMessageEntry, AgentMailboxStatus, AgentMergeSafePointEntry, AgentRouteId,
    AgentThreadResultRecordedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentUsageSummary, ControlEntry, EventHandler, Session, SessionRef, TaskChildSessionStatus,
};

use super::{
    AgentChatChildThread, AgentResultMaterialization, AgentSupervisor, AgentTaskChildThread,
    agent_terminal_status_from_task_child, append_control, build_agent_thread_result, hash_text,
};

impl AgentSupervisor {
    pub(crate) fn record_task_child_result<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentTaskChildThread,
        child_session_ref: SessionRef,
        status: TaskChildSessionStatus,
        materialized: &AgentResultMaterialization,
        outcome: &sigil_kernel::AgentRunOutcome,
        usage: Option<AgentUsageSummary>,
    ) -> Result<()>
    where
        H: EventHandler + Send,
    {
        let terminal_status = agent_terminal_status_from_task_child(status);
        let result = build_agent_thread_result(
            thread.thread_id.clone(),
            child_session_ref.clone(),
            terminal_status,
            materialized,
            outcome,
            usage,
        );
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry { result }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentMergeSafePoint(AgentMergeSafePointEntry {
                thread_id: thread.thread_id.clone(),
                parent_thread_id: thread.parent_thread_id.clone(),
                result_hash: hash_text(&materialized.final_text),
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub(crate) fn record_chat_child_result<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        status: TaskChildSessionStatus,
        materialized: &AgentResultMaterialization,
        outcome: &sigil_kernel::AgentRunOutcome,
        usage: Option<AgentUsageSummary>,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        let terminal_status = agent_terminal_status_from_task_child(status);
        let result = build_agent_thread_result(
            thread.thread_id.clone(),
            thread.child_session_ref.clone(),
            terminal_status,
            materialized,
            outcome,
            usage,
        );
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadResultRecorded(AgentThreadResultRecordedEntry { result }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentMergeSafePoint(AgentMergeSafePointEntry {
                thread_id: thread.thread_id.clone(),
                parent_thread_id: thread.parent_thread_id.clone(),
                result_hash: hash_text(&materialized.final_text),
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub fn record_chat_mailbox_consumed<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        route_ids: &[AgentRouteId],
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        for route_id in route_ids {
            append_control(
                session,
                handler,
                ControlEntry::AgentMailboxMessage(AgentMailboxMessageEntry {
                    route_id: route_id.clone(),
                    source_thread_id: thread.parent_thread_id.clone(),
                    target_thread_id: thread.thread_id.clone(),
                    prompt_hash: String::new(),
                    prompt: None,
                    status: AgentMailboxStatus::Consumed,
                    reason: None,
                    updated_at_ms: None,
                }),
            )?;
        }
        Ok(())
    }

    pub fn record_task_child_failure<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentTaskChildThread,
        reason: String,
    ) -> Result<()>
    where
        H: EventHandler + Send,
    {
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Failed,
                reason: Some(reason),
                updated_at_ms: None,
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub fn record_chat_child_failure<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        reason: String,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Failed,
                reason: Some(reason),
                updated_at_ms: None,
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }
}
