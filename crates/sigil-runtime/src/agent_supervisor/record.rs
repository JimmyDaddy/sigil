use anyhow::Result;
use sigil_kernel::{
    AGENT_USER_INPUT_ROUTE_SCHEMA_VERSION, AgentApprovalRouteEntry, AgentMailboxMessageEntry,
    AgentMailboxStatus, AgentMergeSafePointEntry, AgentRouteId, AgentRouteStatus, AgentThreadId,
    AgentThreadResultRecordedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentUsageSummary, AgentUserInputRouteEntryV1, AgentUserInputRouteProjectionV1, ControlEntry,
    EventHandler, PublicUserInputRequestV1, Session, SessionRef, TaskChildSessionStatus, TaskId,
    TaskIsolationMode, UserInputSourceV1,
};

use super::{
    AgentChatChildThread, AgentResultMaterialization, AgentSupervisor, AgentTaskChildThread,
    agent_terminal_status_from_task_child, append_control, build_agent_thread_result, hash_text,
    short_digest,
};

impl AgentSupervisor {
    pub(crate) fn update_chat_child_user_input_route<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        route: &AgentUserInputRouteEntryV1,
        request: PublicUserInputRequestV1,
        status: AgentRouteStatus,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        let mut update = route.clone();
        update.request = request;
        update.status = status;
        update.updated_at_unix_ms = crate::current_unix_time_ms();
        append_control(session, handler, ControlEntry::AgentUserInputRoute(update))
    }

    pub(crate) fn close_registered_chat_child_user_input_routes<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread_id: &AgentThreadId,
        status: AgentRouteStatus,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        let routes = AgentUserInputRouteProjectionV1::from_session_entries(session.entries())?
            .routes_for_thread(thread_id)
            .filter(|route| route.status == AgentRouteStatus::Registered)
            .cloned()
            .collect::<Vec<_>>();
        for route in routes {
            self.update_chat_child_user_input_route(
                session,
                handler,
                &route,
                route.request.clone(),
                status,
            )?;
        }
        Ok(())
    }

    pub(crate) fn restore_chat_child_blocked_after_resume_failure<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread_id: &AgentThreadId,
        route_id: &AgentRouteId,
        reason: &str,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: AgentThreadStatus::Blocked,
                reason: Some(format!(
                    "blocked_needs_user_input:{}:{}",
                    route_id.as_str(),
                    sigil_kernel::safe_persistence_text(reason)
                )),
                updated_at_ms: Some(crate::current_unix_time_ms()),
            }),
        )?;
        self.release_thread(thread_id);
        Ok(())
    }

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
        H: EventHandler + Send + ?Sized,
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

    pub(crate) fn record_chat_child_blocked_for_approval<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        route: &AgentApprovalRouteEntry,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        let binding = route
            .binding
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("background approval route is missing its binding"))?;
        if route.source_thread_id != thread.thread_id
            || binding.attempt_id != thread.attempt_id
            || binding.batch_id != thread.batch_id
            || binding.isolation != thread.isolation
        {
            anyhow::bail!("background approval route binding does not match its child thread");
        }
        append_control(
            session,
            handler,
            ControlEntry::AgentApprovalRoute(route.clone()),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Blocked,
                reason: Some(format!(
                    "blocked_needs_approval:{}",
                    route.route_id.as_str()
                )),
                updated_at_ms: None,
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub(crate) fn record_chat_child_waiting_for_input<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentChatChildThread,
        request: PublicUserInputRequestV1,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        if request.identity.source_thread_id != thread.thread_id {
            anyhow::bail!("background user-input request belongs to a different child thread");
        }
        let route_id = AgentRouteId::new(format!(
            "agent_input_{}",
            short_digest(&hash_text(&format!(
                "{}\0{}\0{}",
                thread.thread_id.as_str(),
                request.identity.request_id.as_str(),
                request.request_hash,
            )))
        ))?;
        append_control(
            session,
            handler,
            ControlEntry::AgentUserInputRoute(AgentUserInputRouteEntryV1 {
                schema_version: AGENT_USER_INPUT_ROUTE_SCHEMA_VERSION,
                route_id: route_id.clone(),
                source_thread_id: thread.thread_id.clone(),
                source_attempt_id: thread.attempt_id.clone(),
                profile_id: thread.profile_id.clone(),
                parent_thread_id: thread.parent_thread_id.clone(),
                batch_id: thread.batch_id.clone(),
                budget_scope_id: thread.budget_scope_id.clone(),
                isolation: thread.isolation,
                child_session_ref: thread.child_session_ref.clone(),
                request,
                status: AgentRouteStatus::Requested,
                updated_at_unix_ms: crate::current_unix_time_ms(),
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Blocked,
                reason: Some(format!("blocked_needs_user_input:{}", route_id.as_str())),
                updated_at_ms: Some(crate::current_unix_time_ms()),
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }

    pub(crate) fn record_task_planner_waiting_for_input<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        thread: &AgentTaskChildThread,
        task_id: &TaskId,
        child_session_ref: &SessionRef,
        request: PublicUserInputRequestV1,
    ) -> Result<()>
    where
        H: EventHandler + Send + ?Sized,
    {
        if request.identity.source_thread_id != thread.thread_id {
            anyhow::bail!("planner user-input request belongs to a different child thread");
        }
        if !matches!(
            &request.source,
            UserInputSourceV1::Planner { task_id: source_task_id } if source_task_id == task_id
        ) {
            anyhow::bail!("planner user-input request is not bound to the active task");
        }
        let route_id = AgentRouteId::new(format!(
            "agent_input_{}",
            short_digest(&hash_text(&format!(
                "{}\0{}\0{}",
                thread.thread_id.as_str(),
                request.identity.request_id.as_str(),
                request.request_hash,
            )))
        ))?;
        append_control(
            session,
            handler,
            ControlEntry::AgentUserInputRoute(AgentUserInputRouteEntryV1 {
                schema_version: AGENT_USER_INPUT_ROUTE_SCHEMA_VERSION,
                route_id: route_id.clone(),
                source_thread_id: thread.thread_id.clone(),
                source_attempt_id: thread.attempt_id.clone(),
                profile_id: thread.profile_id.clone(),
                parent_thread_id: thread.parent_thread_id.clone(),
                batch_id: None,
                budget_scope_id: task_id.clone(),
                isolation: TaskIsolationMode::SharedReadOnly,
                child_session_ref: child_session_ref.clone(),
                request,
                status: AgentRouteStatus::Requested,
                updated_at_unix_ms: crate::current_unix_time_ms(),
            }),
        )?;
        append_control(
            session,
            handler,
            ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
                thread_id: thread.thread_id.clone(),
                status: AgentThreadStatus::Blocked,
                reason: Some(format!("blocked_needs_user_input:{}", route_id.as_str())),
                updated_at_ms: Some(crate::current_unix_time_ms()),
            }),
        )?;
        self.release_thread(&thread.thread_id);
        Ok(())
    }
}
