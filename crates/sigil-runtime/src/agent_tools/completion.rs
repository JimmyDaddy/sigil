use std::collections::BTreeSet;

use super::*;
use crate::agent_completion::{AgentCompletionHub, AgentCompletionRegistration};

struct JoinedAgentCompletionContext {
    call_id: String,
    thread: BackgroundChatAgentThreadRecord,
    _release_guard: ChatChildThreadGuard,
}

impl AgentToolRuntime {
    pub(super) async fn settle_current_join_dependencies(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
    ) -> Result<Option<FinalAnswerContext>> {
        let dependencies = std::mem::take(&mut self.join_dependencies);
        if dependencies.is_empty() {
            return Ok(None);
        }

        let registrations = dependencies
            .into_iter()
            .map(|dependency| {
                let JoinedChatAgentHandle {
                    sequence,
                    call_id,
                    thread,
                    future,
                    release_guard,
                } = dependency;
                let key = (thread.thread_id.clone(), thread.attempt_id.clone());
                AgentCompletionRegistration::new(
                    key,
                    sequence,
                    JoinedAgentCompletionContext {
                        call_id,
                        thread,
                        _release_guard: release_guard,
                    },
                    future,
                )
            })
            .collect::<Vec<_>>();
        let completion_hub = match AgentCompletionHub::from_batch(registrations) {
            Ok(hub) => hub,
            Err(rejection) => {
                let (error, registrations) = rejection.into_parts();
                let reason = format!("host join completion batch rejected: {error}");
                let mut recorded = BTreeSet::new();
                let mut first_cleanup_error = None;
                for registration in registrations {
                    let (key, _sequence, context, future) = registration.into_parts();
                    drop(future);
                    let thread = context.thread.to_runtime_thread();
                    drop(context._release_guard);
                    if !recorded.insert(key) {
                        continue;
                    }
                    let failure_result = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &thread,
                        reason.clone(),
                    );
                    let continuation_result = append_agent_result_continuation(
                        session,
                        handler,
                        thread.thread_id.clone(),
                        AgentResultContinuationStatus::Failed,
                        Some(reason.clone()),
                    );
                    if first_cleanup_error.is_none() {
                        first_cleanup_error =
                            failure_result.err().or_else(|| continuation_result.err());
                    }
                }
                if let Some(cleanup_error) = first_cleanup_error {
                    return Err(anyhow!(reason).context(format!(
                        "completion batch cleanup also failed: {cleanup_error:#}"
                    )));
                }
                return Err(error.into());
            }
        };
        let completions = completion_hub.collect().await;

        let mut members = Vec::new();
        let mut delivered_threads = Vec::new();
        let mut first_commit_error = None;
        let cancellation_requested = self
            .run_cancellation
            .as_ref()
            .is_some_and(sigil_kernel::RunCancellationHandle::is_cancel_requested);
        for completion in completions {
            let (completion_thread_id, completion_attempt_id) = completion.key;
            let sequence = completion.sequence;
            let JoinedAgentCompletionContext {
                call_id,
                thread: thread_record,
                _release_guard,
            } = completion.context;
            let joined = completion.result;
            let thread = thread_record.to_runtime_thread();
            if thread.thread_id != completion_thread_id
                || thread.attempt_id != completion_attempt_id
            {
                let reason = "completion hub identity did not match joined child context";
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &thread,
                    reason.to_owned(),
                );
                let _ = append_agent_result_continuation(
                    session,
                    handler,
                    thread.thread_id.clone(),
                    AgentResultContinuationStatus::Failed,
                    Some(reason.to_owned()),
                );
                if first_commit_error.is_none() {
                    first_commit_error = Some(anyhow!(
                        "{reason}: expected thread {} attempt {:?}, received thread {} attempt {:?}",
                        thread.thread_id.as_str(),
                        thread.attempt_id,
                        completion_thread_id.as_str(),
                        completion_attempt_id,
                    ));
                }
                continue;
            }
            let thread_id = completion_thread_id;
            let commit_result = if cancellation_requested {
                append_joined_child_interrupted(
                    session,
                    handler,
                    &thread,
                    "root run cancelled while the host join barrier was active",
                )
            } else {
                match joined {
                    Ok(output) => {
                        let budget_warning = self
                            .supervisor
                            .validate_usage_budget(&thread.budget_scope_id, &output.usage)
                            .err()
                            .map(|error| format!("{error:#}"));
                        let result = self
                            .supervisor
                            .record_chat_child_result(
                                session,
                                handler,
                                &thread,
                                output.status,
                                &output.materialized,
                                &output.outcome,
                                Some(output.usage),
                            )
                            .and_then(|()| {
                                self.supervisor.record_chat_mailbox_consumed(
                                    session,
                                    handler,
                                    &thread,
                                    &output.consumed_mailbox_route_ids,
                                )
                            });
                        if result.is_ok()
                            && let Some(warning) = budget_warning
                        {
                            let _ = handler.handle(RunEvent::Notice(format!(
                                "agent budget warning after joined child completion: {warning}"
                            )));
                        }
                        result
                    }
                    Err(error) => {
                        let reason = format!("{error:#}");
                        let result = self.supervisor.record_chat_child_failure(
                            session,
                            handler,
                            &thread,
                            reason.clone(),
                        );
                        let _ = handler.handle(RunEvent::Notice(format!(
                            "joined agent {} failed: {reason}",
                            thread_id.as_str()
                        )));
                        result
                    }
                }
            };
            if let Err(error) = commit_result {
                if first_commit_error.is_none() {
                    first_commit_error = Some(error.context(format!(
                        "failed to commit joined agent {}",
                        thread_id.as_str()
                    )));
                }
                let _ = append_agent_result_continuation(
                    session,
                    handler,
                    thread_id,
                    AgentResultContinuationStatus::Failed,
                    Some("host join terminal commit failed".to_owned()),
                );
                continue;
            }
            let continuation_status = if cancellation_requested {
                AgentResultContinuationStatus::Cancelled
            } else {
                AgentResultContinuationStatus::Started
            };
            if let Err(error) = append_agent_result_continuation(
                session,
                handler,
                thread_id.clone(),
                continuation_status,
                Some("host join barrier committed child terminal state".to_owned()),
            ) {
                if first_commit_error.is_none() {
                    first_commit_error = Some(error.context(format!(
                        "failed to commit joined agent continuation {}",
                        thread_id.as_str()
                    )));
                }
                continue;
            }
            if cancellation_requested {
                continue;
            }

            let projection = session.agent_thread_state_projection();
            let Some(projected) = projection.threads.get(&thread_id) else {
                if first_commit_error.is_none() {
                    first_commit_error = Some(anyhow!(
                        "joined agent {} is missing from projection",
                        thread_id.as_str()
                    ));
                }
                continue;
            };
            let result = projected.result.as_ref();
            members.push((
                sequence,
                json!({
                    "call_id": call_id,
                    "thread_id": thread_id.as_str(),
                    "display_name": projected.display_name.as_deref(),
                    "status": thread_status_label(projected.status),
                    "objective": &projected.objective,
                    "summary": result.map(|result| bounded_summary(&result.summary, DEFAULT_RESULT_SUMMARY_LIMIT)),
                    "summary_truncated": result.is_some_and(|result| result.summary_truncated),
                    "result_ref": result.map(|result| json!({
                        "thread_id": result.thread_id.as_str(),
                        "session_ref": result.session_ref.as_path().display().to_string(),
                        "output_hash": result.output_hash,
                        "final_answer_ref": result.final_answer_ref.as_ref().map(|reference| json!({
                            "session_ref": reference.session_ref.as_path().display().to_string(),
                            "message_id": reference.message_id,
                            "content_hash": reference.content_hash,
                            "char_count": reference.char_count,
                        })),
                    })),
                    "changed_paths": result.map(|result| &result.changed_paths),
                    "risks": result.map(|result| &result.risks),
                    "followups": result.map(|result| &result.followups),
                }),
            ));
            delivered_threads.push((sequence, thread_id));
        }

        if cancellation_requested {
            return Err(anyhow!("root run cancelled while joining child agents"));
        }
        if let Some(error) = first_commit_error {
            return Err(error);
        }

        members.sort_by_key(|(sequence, _)| *sequence);
        delivered_threads.sort_by_key(|(sequence, _)| *sequence);
        let members = members
            .into_iter()
            .map(|(_, member)| member)
            .collect::<Vec<_>>();
        let thread_ids = delivered_threads
            .into_iter()
            .map(|(_, thread_id)| thread_id)
            .collect::<Vec<_>>();
        let prompt = json!({
            "type": "agent_join_results",
            "message": "All host-joined child agents from this tool batch are terminal. Use these bounded results now; do not call wait_agent merely to collect them. Use read_agent_result only when the bounded summary is insufficient for a concrete decision.",
            "members": members,
        })
        .to_string();
        let key = format!("agent-join:{}", hash_text(&prompt));
        self.pending_join_contexts.insert(key.clone(), thread_ids);
        Ok(Some(FinalAnswerContext { key, prompt }))
    }

    pub(super) fn abort_current_join_dependencies(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        reason: &str,
    ) -> Result<()> {
        let dependencies = std::mem::take(&mut self.join_dependencies);
        let mut first_error = None;
        for dependency in dependencies {
            let thread = dependency.thread.to_runtime_thread();
            // Dropping the ordinary child future guarantees no detached execution survives this
            // parent error. The per-dependency release guard closes the supervisor reservation.
            drop(dependency.future);
            let record_result = self.supervisor.record_chat_child_failure(
                session,
                handler,
                &thread,
                reason.to_owned(),
            );
            let continuation_result = append_agent_result_continuation(
                session,
                handler,
                thread.thread_id.clone(),
                AgentResultContinuationStatus::Failed,
                Some(reason.to_owned()),
            );
            drop(dependency.release_guard);
            if first_error.is_none() {
                first_error = record_result.err().or_else(|| continuation_result.err());
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn confirm_current_join_context(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        context_key: &str,
    ) -> Result<()> {
        let Some(thread_ids) = self.pending_join_contexts.get(context_key).cloned() else {
            return Ok(());
        };
        for thread_id in thread_ids {
            if session
                .agent_result_continuation_projection()
                .statuses
                .get(&thread_id)
                == Some(&AgentResultContinuationStatus::Completed)
            {
                continue;
            }
            append_agent_result_continuation(
                session,
                handler,
                thread_id,
                AgentResultContinuationStatus::Completed,
                Some(format!("joined result context delivered as {context_key}")),
            )?;
        }
        self.pending_join_contexts.remove(context_key);
        Ok(())
    }

    pub(super) fn cancel_current_join_contexts(
        &mut self,
        session: &mut Session,
        handler: &mut (dyn EventHandler + Send),
        context_keys: &[String],
        reason: &str,
    ) -> Result<()> {
        let mut first_error = None;
        for context_key in context_keys {
            let Some(thread_ids) = self.pending_join_contexts.remove(context_key) else {
                continue;
            };
            for thread_id in thread_ids {
                if let Err(error) = append_agent_result_continuation(
                    session,
                    handler,
                    thread_id,
                    AgentResultContinuationStatus::Cancelled,
                    Some(reason.to_owned()),
                ) && first_error.is_none()
                {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }
}

fn append_joined_child_interrupted(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    thread: &crate::AgentChatChildThread,
    reason: &str,
) -> Result<()> {
    for control in [
        ControlEntry::AgentRunInterrupted(AgentRunInterruptedEntry {
            thread_id: thread.thread_id.clone(),
            attempt_id: thread.attempt_id.clone(),
            reason: reason.to_owned(),
        }),
        ControlEntry::AgentThreadStatusChanged(AgentThreadStatusChangedEntry {
            thread_id: thread.thread_id.clone(),
            status: AgentThreadStatus::Interrupted,
            reason: Some(reason.to_owned()),
            updated_at_ms: Some(unix_time_ms()),
        }),
    ] {
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    Ok(())
}

pub(super) fn append_agent_result_continuation(
    session: &mut Session,
    handler: &mut (dyn EventHandler + Send),
    thread_id: AgentThreadId,
    status: AgentResultContinuationStatus,
    reason: Option<String>,
) -> Result<()> {
    let control = ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
        thread_id,
        status,
        reason,
        updated_at_ms: Some(unix_time_ms()),
    });
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))?;
    Ok(())
}
