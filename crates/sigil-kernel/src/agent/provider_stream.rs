use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result};
use futures::StreamExt;

use crate::{
    FrozenProviderRequestMaterial, MAX_PROVIDER_TURN_TOOL_ARGS_BYTES, MAX_PROVIDER_TURN_TOOL_CALLS,
    MAX_STREAMED_TOOL_ARGS_BYTES, ProviderPhysicalAttemptOutcome, ProviderTurnRecoveryEvidenceV1,
    ProviderTurnRecoveryPolicyV1, ProviderTurnRecoveryScheduledEntry,
    ProviderTurnRecoveryTerminalDispositionV1, ProviderTurnRecoveryTerminalError,
    ProviderWireStateV1, RecoveryDispositionV1, ToolCallPersistenceProjection,
    event::{EventHandler, RunEvent},
    provider::{
        CompletionRequest, Provider, ProviderChunk, ProviderContinuationState, ResponseHandle,
        ToolCall,
    },
    session::{ControlEntry, ProviderPhysicalAttemptAudit, ProviderTurnRecoveryAudit, Session},
};

/// Ephemeral accounting for streamed data that has not crossed the durable assistant-message
/// boundary. It carries counts only: raw partial text never enters recovery authority or logs.
#[derive(Debug, Default)]
struct ProviderTurnPartialOutput {
    text_bytes: usize,
    reasoning_bytes: usize,
    streamed_tool_call_count: usize,
}

impl ProviderTurnPartialOutput {
    fn observes_tool_call(&mut self) {
        self.streamed_tool_call_count = self.streamed_tool_call_count.saturating_add(1);
    }
}

#[derive(Debug, thiserror::Error)]
#[error("run cancellation requested before provider dispatch")]
struct ProviderConnectCancelledBeforeDispatch;

pub(super) struct ProviderTurnOutput {
    pub(super) assistant_text: String,
    pub(super) reasoning_trace: String,
    pub(super) completed_calls: Vec<ToolCallPersistenceProjection>,
    pub(super) pending_states: Vec<ProviderContinuationState>,
    pub(super) hosted_finalized: Option<crate::FinalizedHostedTurn>,
}

pub(super) struct ProviderTurnDispatchContext<'a> {
    pub(super) hosted_processor: Option<&'a std::sync::Arc<dyn crate::HostedEvidenceProcessor>>,
    pub(super) hosted_dispatch_lifecycle:
        Option<&'a std::sync::Arc<dyn crate::AgentHostedTurnDispatchLifecycle>>,
    pub(super) initial_physical_attempt_id: Option<&'a str>,
    /// A recovery-only turn must claim existing durable authority before it can dispatch.
    pub(super) require_durable_recovery_claim: bool,
}

struct RecoveredProviderTurnAttempt {
    schedule: ProviderTurnRecoveryScheduledEntry,
    physical_attempt_id: String,
}

pub(super) async fn collect_provider_turn<H>(
    provider: &dyn Provider,
    recovery_policy: ProviderTurnRecoveryPolicyV1,
    session: &mut Session,
    request: CompletionRequest,
    logical_run_id: &str,
    previous_response_handle: &mut Option<ResponseHandle>,
    _total_tool_calls: usize,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
    dispatch: ProviderTurnDispatchContext<'_>,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let frozen_request =
        match FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request) {
            Ok(request) => request,
            Err(error) => {
                finish_undispatched_hosted_turn(
                    dispatch.hosted_dispatch_lifecycle,
                    crate::HostedToolTerminalStatus::RequestFailed,
                )?;
                return Err(error);
            }
        };
    collect_frozen_provider_turn(
        provider,
        recovery_policy,
        session,
        frozen_request,
        logical_run_id,
        previous_response_handle,
        _total_tool_calls,
        handler,
        cancellation,
        dispatch,
    )
    .await
}

/// Streams one provider turn from material frozen by a durable pre-send admission boundary.
///
/// The supplied request is never rebuilt or re-frozen. Its session binding is checked before the
/// same physical-attempt Started barrier used by ordinary turns is appended.
#[allow(clippy::too_many_arguments)]
pub(super) async fn collect_frozen_provider_turn<H>(
    provider: &dyn Provider,
    recovery_policy: ProviderTurnRecoveryPolicyV1,
    session: &mut Session,
    frozen_request: FrozenProviderRequestMaterial,
    logical_run_id: &str,
    previous_response_handle: &mut Option<ResponseHandle>,
    _total_tool_calls: usize,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
    dispatch: ProviderTurnDispatchContext<'_>,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let ProviderTurnDispatchContext {
        hosted_processor,
        hosted_dispatch_lifecycle,
        initial_physical_attempt_id,
        require_durable_recovery_claim,
    } = dispatch;
    let request_template = frozen_request.request().clone();
    let hosted_enabled = !request_template.hosted_tools.is_empty();
    let pre_wire_validation = (|| -> Result<()> {
        if frozen_request.session_scope_id() != session.session_scope_id() {
            anyhow::bail!("frozen provider request belongs to a different session scope");
        }
        crate::validate_request_image_attachments(&request_template)?;
        crate::validate_image_input_capability(
            provider.image_input_capability(&request_template.model_name),
            &request_template,
        )?;
        if hosted_enabled && hosted_processor.is_none() {
            return Err(crate::HostedTurnError::MissingProcessor.into());
        }
        if hosted_enabled
            && !provider
                .hosted_web_search_capability(&request_template.model_name)
                .is_supported()
        {
            anyhow::bail!("provider model does not support hosted web search");
        }
        for hosted_tool in &request_template.hosted_tools {
            hosted_tool.validate()?;
        }
        Ok(())
    })();
    if let Err(error) = pre_wire_validation {
        finish_undispatched_hosted_turn(
            hosted_dispatch_lifecycle,
            crate::HostedToolTerminalStatus::RequestFailed,
        )?;
        return Err(error);
    }
    recovery_policy.validate()?;
    let mut recovered_attempt = resume_scheduled_provider_turn_recovery(
        provider,
        recovery_policy,
        session,
        &frozen_request,
        logical_run_id,
        handler,
        cancellation,
    )
    .await?;
    if require_durable_recovery_claim && recovered_attempt.is_none() {
        return Err(ProviderTurnRecoveryTerminalError {
            disposition: ProviderTurnRecoveryTerminalDispositionV1::Blocked,
            reason_code: "provider_recovery_schedule_missing",
        }
        .into());
    }
    let mut next_physical_attempt_id = initial_physical_attempt_id.map(str::to_owned);
    loop {
        let request = request_template.clone();
        let request_for_transport_fallback = request.clone();
        let hosted_context = crate::HostedFinalizationContext {
            session_scope_id: session.session_scope_id().to_owned(),
            provider_name: request.provider_name.clone(),
            model_name: request.model_name.clone(),
        };
        let mut physical_attempt = match match recovered_attempt.take() {
            Some(recovery) => {
                ProviderPhysicalAttemptAudit::start_recovery(
                    session,
                    logical_run_id,
                    &frozen_request,
                    &recovery.schedule,
                    &recovery.physical_attempt_id,
                )
                .await
            }
            None => match next_physical_attempt_id.take() {
                Some(physical_attempt_id) => {
                    ProviderPhysicalAttemptAudit::start_with_id(
                        session,
                        logical_run_id,
                        &frozen_request,
                        &physical_attempt_id,
                    )
                    .await
                }
                None => {
                    ProviderPhysicalAttemptAudit::start(session, logical_run_id, &frozen_request)
                        .await
                }
            },
        } {
            Ok(attempt) => attempt,
            Err(error) => {
                finish_undispatched_hosted_turn(
                    hosted_dispatch_lifecycle,
                    crate::HostedToolTerminalStatus::RequestFailed,
                )?;
                return Err(error);
            }
        };
        let mut generation_observed = false;
        let mut partial_output = ProviderTurnPartialOutput::default();
        let result = collect_provider_turn_after_send_barrier(
            provider,
            session,
            request,
            previous_response_handle,
            _total_tool_calls,
            handler,
            cancellation,
            hosted_processor,
            hosted_dispatch_lifecycle,
            hosted_enabled,
            hosted_context,
            &mut physical_attempt,
            &mut generation_observed,
            &mut partial_output,
        )
        .await;
        let rejection = (!generation_observed
            && !physical_attempt.has_durable_output_or_side_effect())
        .then(|| {
            result
                .as_ref()
                .err()
                .and_then(|error| provider.classify_pre_generation_rejection(error))
        })
        .flatten();
        let outcome = match &result {
            Ok(_) => ProviderPhysicalAttemptOutcome::Completed,
            Err(error)
                if error
                    .downcast_ref::<ProviderConnectCancelledBeforeDispatch>()
                    .is_some() =>
            {
                ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
            }
            Err(_) if rejection.is_some() => {
                ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
            }
            Err(error)
                if error
                    .downcast_ref::<crate::ProviderProtocolViolation>()
                    .is_some() =>
            {
                // A typed adapter protocol violation proves that a provider response was parsed,
                // even when the violating frame was rejected before it could become a durable
                // TextDelta or tool-call event.
                ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput
            }
            Err(_) if physical_attempt.has_durable_output_or_side_effect() => {
                ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect
            }
            Err(_) if generation_observed => {
                ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput
            }
            Err(_) => ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
        };
        let physical_attempt_id = physical_attempt.physical_attempt_id().map(str::to_owned);
        let terminal_result = physical_attempt.finish(session, outcome, rejection).await;
        match (result, terminal_result) {
            (Ok(output), Ok(())) => return Ok(output),
            (Ok(_), Err(error)) => {
                return Err(error.context("provider physical-attempt terminal append failed"));
            }
            (Err(error), Ok(())) => {
                let Some(physical_attempt_id) = physical_attempt_id else {
                    finish_failed_hosted_attempt(hosted_dispatch_lifecycle, &error, rejection)?;
                    return Err(error);
                };
                if partial_output.text_bytes > 0
                    || partial_output.reasoning_bytes > 0
                    || partial_output.streamed_tool_call_count > 0
                {
                    let discarded = ProviderTurnRecoveryAudit::discard_partial_output(
                        session,
                        logical_run_id,
                        &physical_attempt_id,
                        partial_output.text_bytes,
                        partial_output.reasoning_bytes,
                        partial_output.streamed_tool_call_count,
                    )
                    .await?;
                    let public = discarded.as_ref().map_or_else(
                        || crate::PublicProviderTurnPartialOutputDiscardedViewV1 {
                            text_discarded: partial_output.text_bytes > 0,
                            reasoning_discarded: partial_output.reasoning_bytes > 0,
                            tool_request_discarded: partial_output.streamed_tool_call_count > 0,
                        },
                        crate::PublicProviderTurnPartialOutputDiscardedViewV1::from,
                    );
                    handler.handle(RunEvent::ProviderTurnPartialOutputDiscarded(public))?;
                }
                let wire_state = if generation_observed {
                    ProviderWireStateV1::ResponseStarted
                } else if rejection
                    == Some(crate::ProviderRequestRejection::ConnectFailedBeforeDispatch)
                {
                    ProviderWireStateV1::NoBytesSent
                } else {
                    ProviderWireStateV1::RequestBytesMayHaveBeenSent
                };
                let failure = provider.observe_failure(&error, wire_state);
                let attempts = session.provider_physical_attempt_projection()?;
                let attempt = attempts
                    .attempt(&physical_attempt_id)
                    .context("finished provider attempt is missing from durable projection")?;
                let mut evidence = ProviderTurnRecoveryEvidenceV1::from_terminal_attempt(
                    attempt,
                    failure,
                    &frozen_request,
                )?;
                evidence.partial_output_has_tool_calls =
                    partial_output.streamed_tool_call_count > 0;
                let budget = session
                    .provider_turn_recovery_projection()?
                    .budget_for_logical_run_id(logical_run_id);
                let transport_fallback = provider.transport_fallback_candidate(
                    &request_for_transport_fallback,
                    &evidence.failure,
                );
                if let Some(candidate) = &transport_fallback {
                    candidate.validate()?;
                }
                match recovery_policy.decide(
                    &evidence,
                    budget,
                    cancellation.is_some_and(crate::RunCancellationHandle::is_cancel_requested),
                ) {
                    RecoveryDispositionV1::RetryProviderTurn { retry_after_ms } => {
                        let schedule = ProviderTurnRecoveryAudit::schedule(
                            session,
                            &evidence,
                            budget,
                            retry_after_ms,
                            recovery_policy,
                        )
                        .await?;
                        if let Some(candidate) = transport_fallback {
                            let selection = ProviderTurnRecoveryAudit::select_transport_fallback(
                                session, &schedule, candidate,
                            )
                            .await?;
                            if let Err(activation_error) =
                                provider.activate_transport_fallback(&selection.candidate)
                            {
                                let exhausted = ProviderTurnRecoveryAudit::exhaust_scheduled(
                                    session,
                                    &schedule,
                                    false,
                                    ProviderTurnRecoveryTerminalDispositionV1::Blocked,
                                    "transport_fallback_unavailable",
                                )
                                .await?;
                                handler.handle(RunEvent::ProviderTurnRecovery(
                                    crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
                                ))?;
                                return Err(activation_error.context(
                                    ProviderTurnRecoveryTerminalError {
                                        disposition: exhausted.terminal_disposition,
                                        reason_code: "transport_fallback_unavailable",
                                    },
                                ));
                            }
                        }
                        let recovery_view =
                            crate::PublicProviderTurnRecoveryViewV1::waiting(&schedule);
                        handler.handle(RunEvent::ProviderTurnRecovery(recovery_view.clone()))?;
                        handler.handle(RunEvent::Notice(format!(
                            "Reconnecting... {}/{}",
                            recovery_view.active_retry_count, recovery_view.active_max_retries
                        )))?;
                        if let Err(wait_error) = wait_for_pre_dispatch_connect_retry(
                            Duration::from_millis(retry_after_ms),
                            cancellation,
                        )
                        .await
                        {
                            finish_undispatched_hosted_turn(
                                hosted_dispatch_lifecycle,
                                crate::HostedToolTerminalStatus::Cancelled,
                            )?;
                            // The schedule is durable authority.  If cancellation wins the
                            // backoff race, close that authority explicitly so restart repair
                            // never mistakes it for a retry that still needs dispatching.
                            let disposition = ProviderTurnRecoveryTerminalDispositionV1::Cancelled;
                            let exhausted = ProviderTurnRecoveryAudit::exhaust(
                                session,
                                &evidence,
                                schedule.budget_snapshot,
                                disposition,
                                "provider_recovery_cancelled",
                            )
                            .await?;
                            handler.handle(RunEvent::ProviderTurnRecovery(
                                crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
                            ))?;
                            return Err(wait_error.context(ProviderTurnRecoveryTerminalError {
                                disposition,
                                reason_code: "provider_recovery_cancelled",
                            }));
                        }
                        let started = ProviderTurnRecoveryAudit::start(session, &schedule).await?;
                        handler.handle(RunEvent::ProviderTurnRecovery(
                            crate::PublicProviderTurnRecoveryViewV1::recovering(&schedule),
                        ))?;
                        recovered_attempt = Some(RecoveredProviderTurnAttempt {
                            schedule,
                            physical_attempt_id: started.physical_attempt_id,
                        });
                    }
                    RecoveryDispositionV1::Block { reason_code } => {
                        finish_failed_hosted_attempt(hosted_dispatch_lifecycle, &error, rejection)?;
                        let disposition = ProviderTurnRecoveryTerminalDispositionV1::Blocked;
                        let exhausted = ProviderTurnRecoveryAudit::exhaust(
                            session,
                            &evidence,
                            budget,
                            disposition,
                            reason_code,
                        )
                        .await?;
                        handler.handle(RunEvent::ProviderTurnRecovery(
                            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
                        ))?;
                        return Err(error.context(ProviderTurnRecoveryTerminalError {
                            disposition,
                            reason_code,
                        }));
                    }
                    RecoveryDispositionV1::Pause { reason_code } => {
                        finish_failed_hosted_attempt(hosted_dispatch_lifecycle, &error, rejection)?;
                        let disposition = ProviderTurnRecoveryTerminalDispositionV1::Paused;
                        let exhausted = ProviderTurnRecoveryAudit::exhaust(
                            session,
                            &evidence,
                            budget,
                            disposition,
                            reason_code,
                        )
                        .await?;
                        handler.handle(RunEvent::ProviderTurnRecovery(
                            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
                        ))?;
                        return Err(error.context(ProviderTurnRecoveryTerminalError {
                            disposition,
                            reason_code,
                        }));
                    }
                    RecoveryDispositionV1::Cancelled => {
                        finish_undispatched_hosted_turn(
                            hosted_dispatch_lifecycle,
                            crate::HostedToolTerminalStatus::Cancelled,
                        )?;
                        let disposition = ProviderTurnRecoveryTerminalDispositionV1::Cancelled;
                        let exhausted = ProviderTurnRecoveryAudit::exhaust(
                            session,
                            &evidence,
                            budget,
                            disposition,
                            "provider_recovery_cancelled",
                        )
                        .await?;
                        handler.handle(RunEvent::ProviderTurnRecovery(
                            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
                        ))?;
                        return Err(error.context(ProviderTurnRecoveryTerminalError {
                            disposition,
                            reason_code: "provider_recovery_cancelled",
                        }));
                    }
                    RecoveryDispositionV1::Irrecoverable { reason_code } => {
                        finish_failed_hosted_attempt(hosted_dispatch_lifecycle, &error, rejection)?;
                        let exhausted = ProviderTurnRecoveryAudit::exhaust(
                            session,
                            &evidence,
                            budget,
                            ProviderTurnRecoveryTerminalDispositionV1::Irrecoverable,
                            reason_code,
                        )
                        .await?;
                        handler.handle(RunEvent::ProviderTurnRecovery(
                            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
                        ))?;
                        return Err(error);
                    }
                }
            }
            (Err(error), Err(terminal_error)) => {
                finish_failed_hosted_attempt(hosted_dispatch_lifecycle, &error, rejection)?;
                return Err(error.context(format!(
                    "provider turn failed and physical-attempt terminal append also failed: {terminal_error:#}"
                )));
            }
        }
    }
}

async fn wait_for_pre_dispatch_connect_retry(
    delay: Duration,
    cancellation: Option<&crate::RunCancellationHandle>,
) -> Result<()> {
    match cancellation {
        Some(cancellation) => {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    anyhow::bail!("run cancellation requested before provider connect retry");
                }
                _ = tokio::time::sleep(delay) => {}
            }
        }
        None => tokio::time::sleep(delay).await,
    }
    Ok(())
}

/// Claims a recovery that survived a process loss before opening any new provider send barrier.
///
/// A recovery `Started` fact is intentionally *not* retried here: it may have crossed provider
/// I/O before the process died. Unstarted schedules instead prove the reconstructed request at
/// their original durable frontier, wait against the absolute deadline, then use the schedule's
/// CAS claim as the only authority for the new physical attempt.
async fn resume_scheduled_provider_turn_recovery<H>(
    provider: &dyn Provider,
    recovery_policy: ProviderTurnRecoveryPolicyV1,
    session: &mut Session,
    frozen_request: &FrozenProviderRequestMaterial,
    logical_run_id: &str,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
) -> Result<Option<RecoveredProviderTurnAttempt>>
where
    H: EventHandler + Send,
{
    if session.store_path().is_none() {
        return Ok(None);
    }
    let projection = session.provider_turn_recovery_projection()?;
    if let Some(terminal) = projection.terminal_for_logical_run_id(logical_run_id) {
        handler.handle(RunEvent::ProviderTurnRecovery(
            crate::PublicProviderTurnRecoveryViewV1::terminal(terminal),
        ))?;
        return Err(ProviderTurnRecoveryTerminalError {
            disposition: terminal.terminal_disposition,
            reason_code: "provider_recovery_already_terminal",
        }
        .into());
    }
    let Some(state) = projection
        .recoveries_for_logical_run_id(logical_run_id)
        .into_iter()
        .filter(|state| state.exhausted.is_none())
        .max_by_key(|state| {
            (
                state.schedule.budget_snapshot.retry_count,
                state.schedule.not_before_unix_ms,
            )
        })
    else {
        return Ok(None);
    };
    let schedule = state.schedule.clone();
    let transport_fallback = state.transport_fallback.clone();

    if schedule.recovery_policy_fingerprint != recovery_policy.fingerprint() {
        let exhausted = ProviderTurnRecoveryAudit::exhaust_scheduled(
            session,
            &schedule,
            false,
            ProviderTurnRecoveryTerminalDispositionV1::Blocked,
            "provider_recovery_policy_changed_re_admit_required",
        )
        .await?;
        handler.handle(RunEvent::ProviderTurnRecovery(
            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
        ))?;
        return Err(ProviderTurnRecoveryTerminalError {
            disposition: exhausted.terminal_disposition,
            reason_code: "provider_recovery_policy_changed_re_admit_required",
        }
        .into());
    }

    if state.started.is_some() {
        // A start may have crossed the send barrier. First make any physical attempt without a
        // terminal explicit, then close the logical turn as an actionable blocker. Neither path
        // is allowed to send provider bytes a second time.
        let store = session
            .durable_store()
            .context("provider-turn recovery restart repair requires a durable store")?;
        let now = provider_turn_recovery_unix_ms();
        tokio::task::spawn_blocking(move || {
            store.recover_unfinished_provider_physical_attempts(now)
        })
        .await
        .context("provider-turn recovery unfinished-attempt repair task failed")??;
        let exhausted = ProviderTurnRecoveryAudit::exhaust_scheduled(
            session,
            &schedule,
            true,
            ProviderTurnRecoveryTerminalDispositionV1::Blocked,
            "recovery_started_without_safe_completion",
        )
        .await?;
        handler.handle(RunEvent::ProviderTurnRecovery(
            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
        ))?;
        return Err(ProviderTurnRecoveryTerminalError {
            disposition: exhausted.terminal_disposition,
            reason_code: "recovery_started_without_safe_completion",
        }
        .into());
    }

    let reconstruction = (|| -> Result<()> {
        let attempts = session.provider_physical_attempt_projection()?;
        let predecessor = attempts
            .attempt(&schedule.failed_physical_attempt_id)
            .context("provider-turn recovery schedule predecessor is missing")?;
        let envelope = predecessor
            .entry
            .request_envelope
            .as_ref()
            .context("provider-turn recovery schedule predecessor lacks an envelope")?;
        if schedule.request_envelope_digest != envelope.canonical_request_hash
            || schedule.source_frontier != envelope.source_frontier
        {
            anyhow::bail!(
                "provider-turn recovery schedule does not match its predecessor envelope"
            );
        }
        if envelope.process_local_material_fingerprint == frozen_request.fingerprint() {
            envelope.verify_exact_process_local_request(frozen_request)?;
        } else {
            let store_path = session
                .store_path()
                .context("provider-turn recovery reconstruction requires a durable session path")?;
            envelope
                .verify_reconstructed_request_at_frontier(store_path, frozen_request.request())?;
        }
        Ok(())
    })();
    if reconstruction.is_err() {
        let exhausted = ProviderTurnRecoveryAudit::exhaust_scheduled(
            session,
            &schedule,
            false,
            ProviderTurnRecoveryTerminalDispositionV1::Blocked,
            "recovery_material_unavailable",
        )
        .await?;
        handler.handle(RunEvent::ProviderTurnRecovery(
            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
        ))?;
        return Err(ProviderTurnRecoveryTerminalError {
            disposition: exhausted.terminal_disposition,
            reason_code: "recovery_material_unavailable",
        }
        .into());
    }

    if let Some(selection) = transport_fallback
        && let Err(activation_error) = provider.activate_transport_fallback(&selection.candidate)
    {
        let exhausted = ProviderTurnRecoveryAudit::exhaust_scheduled(
            session,
            &schedule,
            false,
            ProviderTurnRecoveryTerminalDispositionV1::Blocked,
            "transport_fallback_unavailable",
        )
        .await?;
        handler.handle(RunEvent::ProviderTurnRecovery(
            crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
        ))?;
        return Err(activation_error.context(ProviderTurnRecoveryTerminalError {
            disposition: exhausted.terminal_disposition,
            reason_code: "transport_fallback_unavailable",
        }));
    }

    let now = provider_turn_recovery_unix_ms();
    if schedule.not_before_unix_ms > now {
        handler.handle(RunEvent::ProviderTurnRecovery(
            crate::PublicProviderTurnRecoveryViewV1::waiting(&schedule),
        ))?;
        let delay = schedule.not_before_unix_ms.saturating_sub(now);
        if let Err(wait_error) =
            wait_for_pre_dispatch_connect_retry(Duration::from_millis(delay), cancellation).await
        {
            let exhausted = ProviderTurnRecoveryAudit::exhaust_scheduled(
                session,
                &schedule,
                false,
                ProviderTurnRecoveryTerminalDispositionV1::Cancelled,
                "provider_recovery_cancelled",
            )
            .await?;
            handler.handle(RunEvent::ProviderTurnRecovery(
                crate::PublicProviderTurnRecoveryViewV1::terminal(&exhausted),
            ))?;
            return Err(wait_error.context(ProviderTurnRecoveryTerminalError {
                disposition: exhausted.terminal_disposition,
                reason_code: "provider_recovery_cancelled",
            }));
        }
    }
    let started = match ProviderTurnRecoveryAudit::start(session, &schedule).await {
        Ok(started) => started,
        Err(error) => {
            // A second process may have won the schedule CAS after this owner finished its
            // frontier proof. Re-read durable state instead of falling through the generic
            // provider-error terminal, which would incorrectly mark the participant failed.
            let claimed_elsewhere = session
                .provider_turn_recovery_projection()?
                .recovery(&schedule.recovery_id)
                .is_some_and(|state| state.started.is_some());
            if claimed_elsewhere {
                return Err(error.context(ProviderTurnRecoveryTerminalError {
                    disposition: ProviderTurnRecoveryTerminalDispositionV1::Paused,
                    reason_code: "provider_recovery_claimed_elsewhere",
                }));
            }
            return Err(error);
        }
    };
    handler.handle(RunEvent::ProviderTurnRecovery(
        crate::PublicProviderTurnRecoveryViewV1::recovering(&schedule),
    ))?;
    Ok(Some(RecoveredProviderTurnAttempt {
        schedule,
        physical_attempt_id: started.physical_attempt_id,
    }))
}

fn provider_turn_recovery_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn finish_undispatched_hosted_turn(
    lifecycle: Option<&std::sync::Arc<dyn crate::AgentHostedTurnDispatchLifecycle>>,
    status: crate::HostedToolTerminalStatus,
) -> Result<()> {
    if let Some(lifecycle) = lifecycle {
        lifecycle.finish(status).map_err(anyhow::Error::from)?;
    }
    Ok(())
}

fn finish_failed_hosted_attempt(
    lifecycle: Option<&std::sync::Arc<dyn crate::AgentHostedTurnDispatchLifecycle>>,
    error: &anyhow::Error,
    rejection: Option<crate::ProviderRequestRejection>,
) -> Result<()> {
    let Some(lifecycle) = lifecycle else {
        return Ok(());
    };
    if error
        .downcast_ref::<ProviderConnectCancelledBeforeDispatch>()
        .is_some()
    {
        lifecycle
            .finish(crate::HostedToolTerminalStatus::Cancelled)
            .map_err(anyhow::Error::from)?;
        return Ok(());
    }
    if rejection != Some(crate::ProviderRequestRejection::ConnectFailedBeforeDispatch) {
        // A stream was established, the provider returned a typed response, or the transport
        // outcome is uncertain. In every case the request may have reached the provider and the
        // hosted-request count must become non-refundable.
        lifecycle.mark_dispatched().map_err(anyhow::Error::from)?;
    }
    lifecycle
        .finish(crate::HostedToolTerminalStatus::RequestFailed)
        .map_err(anyhow::Error::from)
}

#[allow(clippy::too_many_arguments)]
async fn collect_provider_turn_after_send_barrier<H>(
    provider: &dyn Provider,
    session: &mut Session,
    request: CompletionRequest,
    previous_response_handle: &mut Option<ResponseHandle>,
    _total_tool_calls: usize,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
    hosted_processor: Option<&std::sync::Arc<dyn crate::HostedEvidenceProcessor>>,
    hosted_dispatch_lifecycle: Option<&std::sync::Arc<dyn crate::AgentHostedTurnDispatchLifecycle>>,
    hosted_enabled: bool,
    hosted_context: crate::HostedFinalizationContext,
    physical_attempt: &mut ProviderPhysicalAttemptAudit,
    generation_observed: &mut bool,
    partial_output: &mut ProviderTurnPartialOutput,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let pricing_snapshot = provider.usage_pricing_snapshot(&request.model_name);
    let stream_result = match cancellation {
        Some(cancellation) => tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ProviderConnectCancelledBeforeDispatch.into()),
            result = provider.stream(request) => result,
        },
        None => provider.stream(request).await,
    };
    let mut stream = match stream_result {
        Ok(stream) => {
            if let Some(lifecycle) = hosted_dispatch_lifecycle {
                lifecycle.mark_dispatched().map_err(anyhow::Error::from)?;
            }
            stream
        }
        Err(error) => return Err(error),
    };
    if hosted_enabled {
        return collect_hosted_provider_turn(
            &mut stream,
            session,
            previous_response_handle,
            handler,
            cancellation,
            hosted_processor.ok_or(crate::HostedTurnError::MissingProcessor)?,
            hosted_context,
            physical_attempt,
            generation_observed,
            partial_output,
            pricing_snapshot.as_ref(),
        )
        .await;
    }
    let mut assistant_text = String::new();
    let mut reasoning_trace_buffer = String::new();
    let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut completed_calls: Vec<ToolCallPersistenceProjection> = Vec::new();
    let mut pending_states: Vec<ProviderContinuationState> = Vec::new();
    let mut total_tool_args_bytes = 0usize;
    let mut completed_call_ids = std::collections::BTreeSet::new();
    let mut terminal_frame_observed = false;

    loop {
        let next = match cancellation {
            Some(cancellation) => tokio::select! {
                biased;
                _ = cancellation.cancelled() => anyhow::bail!("run cancellation requested during provider stream"),
                chunk = stream.next() => chunk,
            },
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.context("provider stream failed")?;
        *generation_observed |= provider_chunk_observes_generation(&chunk);
        match chunk {
            ProviderChunk::TextDelta(delta) => {
                partial_output.text_bytes = partial_output.text_bytes.saturating_add(delta.len());
                assistant_text.push_str(&delta);
                handler.handle(RunEvent::TextDelta(delta))?;
            }
            ProviderChunk::ReasoningDelta(delta) => {
                partial_output.reasoning_bytes =
                    partial_output.reasoning_bytes.saturating_add(delta.len());
                reasoning_trace_buffer.push_str(&delta);
                handler.handle(RunEvent::ReasoningDelta(delta))?;
            }
            ProviderChunk::ReasoningSummaryDelta(delta) => {
                partial_output.reasoning_bytes =
                    partial_output.reasoning_bytes.saturating_add(delta.len());
                reasoning_trace_buffer.push_str(&delta);
                handler.handle(RunEvent::ReasoningDelta(delta))?;
            }
            ProviderChunk::ToolCallStart { id, name } => {
                partial_output.observes_tool_call();
                validate_streamed_tool_identity(&id, &name)?;
                if tool_parts.len() >= MAX_PROVIDER_TURN_TOOL_CALLS && !tool_parts.contains_key(&id)
                {
                    anyhow::bail!(
                        "tool_call_stream_invalid: provider turn exceeded {MAX_PROVIDER_TURN_TOOL_CALLS} tool calls"
                    );
                }
                if tool_parts.contains_key(&id) || completed_call_ids.contains(&id) {
                    anyhow::bail!("tool_call_stream_invalid: provider reused a tool-call id");
                }
                tool_parts.insert(id.clone(), (name.clone(), String::new()));
                handler.handle(RunEvent::ToolCallStarted(ToolCall {
                    id,
                    name: crate::safe_persistence_text(&name),
                    args_json: String::new(),
                }))?;
            }
            ProviderChunk::ToolCallArgsDelta { id, delta } => {
                crate::persistence::validate_tool_call_id(&id)?;
                let Some((_, current_args)) = tool_parts.get(&id) else {
                    anyhow::bail!(
                        "tool_call_stream_invalid: arguments arrived before a matching tool-call start"
                    );
                };
                let next_call_bytes = current_args.len().saturating_add(delta.len());
                let next_total_bytes = total_tool_args_bytes.saturating_add(delta.len());
                if next_call_bytes > MAX_STREAMED_TOOL_ARGS_BYTES {
                    tool_parts.values_mut().for_each(|(_, args)| args.clear());
                    tool_parts.clear();
                    anyhow::bail!(
                        "tool_args_too_large: observed at least {next_call_bytes} bytes, limit {MAX_STREAMED_TOOL_ARGS_BYTES}"
                    );
                }
                if next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
                    tool_parts.values_mut().for_each(|(_, args)| args.clear());
                    tool_parts.clear();
                    anyhow::bail!(
                        "tool_args_too_large: provider turn observed at least {next_total_bytes} bytes, limit {MAX_PROVIDER_TURN_TOOL_ARGS_BYTES}"
                    );
                }
                let Some((_, args_json)) = tool_parts.get_mut(&id) else {
                    anyhow::bail!(
                        "tool_call_stream_invalid: tool-call state disappeared before append"
                    );
                };
                args_json.push_str(&delta);
                total_tool_args_bytes = next_total_bytes;
                handler.handle(RunEvent::ToolCallArgsDelta {
                    id,
                    delta: format!("[{} argument bytes buffered]", args_json.len()),
                })?;
            }
            ProviderChunk::ToolCallComplete(call) => {
                partial_output.observes_tool_call();
                if completed_calls.len() >= MAX_PROVIDER_TURN_TOOL_CALLS {
                    anyhow::bail!(
                        "tool_call_stream_invalid: provider turn exceeded {MAX_PROVIDER_TURN_TOOL_CALLS} completed tool calls"
                    );
                }
                validate_streamed_tool_identity(&call.id, &call.name)?;
                if !completed_call_ids.insert(call.id.clone()) {
                    anyhow::bail!(
                        "tool_call_stream_invalid: provider reused a completed tool-call id"
                    );
                }
                if let Some((streamed_name, streamed_args)) = tool_parts.remove(&call.id) {
                    if streamed_name != call.name || streamed_args != call.args_json {
                        anyhow::bail!(
                            "tool_call_stream_invalid: completed tool call conflicts with streamed identity or arguments"
                        );
                    }
                } else {
                    let next_total_bytes =
                        total_tool_args_bytes.saturating_add(call.args_json.len());
                    if next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
                        anyhow::bail!(
                            "tool_args_too_large: provider turn observed at least {next_total_bytes} bytes, limit {MAX_PROVIDER_TURN_TOOL_ARGS_BYTES}"
                        );
                    }
                    total_tool_args_bytes = next_total_bytes;
                }
                let projection = crate::project_tool_call_for_persistence(call)?;
                handler.handle(RunEvent::ToolCallCompleted(projection.durable_call.clone()))?;
                completed_calls.push(projection);
            }
            ProviderChunk::Usage(mut usage) => {
                let mutation = physical_attempt.cache_layout_mutation();
                usage
                    .cache_usage
                    .get_or_insert(crate::CacheUsageV1 {
                        schema_version: crate::CacheUsageV1::SCHEMA_VERSION,
                        read: None,
                        write: None,
                        uncached: None,
                        local_layout_mutation: None,
                        provider_miss_without_local_mutation: false,
                    })
                    .observe_local_layout(mutation);
                if let Some(cache_usage) = &usage.cache_usage {
                    cache_usage.validate_for_prompt_tokens(usage.prompt_tokens)?;
                }
                if let Some(snapshot) = &pricing_snapshot {
                    usage = snapshot.apply_to_usage(usage)?;
                }
                session.stats_mut().apply_usage(&usage);
                physical_attempt
                    .append_output_control(session, ControlEntry::UsageSnapshot(usage.clone()))
                    .await?;
                handler.handle(RunEvent::Usage(usage))?;
            }
            ProviderChunk::ResponseHandle(handle) => {
                *previous_response_handle = Some(handle.clone());
                let control = ControlEntry::ResponseHandleTracked(handle);
                physical_attempt
                    .append_output_control(session, control.clone())
                    .await?;
                handler.handle(RunEvent::Control(control))?;
            }
            ProviderChunk::BackgroundTaskAccepted(handle) => {
                let control = ControlEntry::BackgroundTaskTracked(handle);
                physical_attempt
                    .append_output_control(session, control.clone())
                    .await?;
                handler.handle(RunEvent::Control(control))?;
            }
            ProviderChunk::BackgroundTaskStatus(status) => {
                handler.handle(RunEvent::Notice(format!(
                    "background task {} status {}",
                    status.task_id, status.status
                )))?;
            }
            ProviderChunk::ReasoningArtifact(_) => {}
            ProviderChunk::ContinuationState(state) => {
                pending_states.push(state.clone());
                handler.handle(RunEvent::ContinuationState(state))?;
            }
            ProviderChunk::ToolCallStreamError(error) => return Err(error.into()),
            ProviderChunk::HostedToolStarted { .. }
            | ProviderChunk::HostedEvidence { .. }
            | ProviderChunk::HostedToolFailed { .. }
            | ProviderChunk::HostedRequestUsage { .. } => {
                anyhow::bail!("provider emitted hosted evidence for a non-hosted request")
            }
            ProviderChunk::Done => {
                terminal_frame_observed = true;
                break;
            }
        }
    }

    if !terminal_frame_observed {
        return Err(crate::ProviderStreamEndedUnexpectedly.into());
    }

    if !tool_parts.is_empty() {
        tool_parts.values_mut().for_each(|(_, args)| args.clear());
        anyhow::bail!("tool_call_stream_invalid: provider ended with incomplete tool calls");
    }

    Ok(ProviderTurnOutput {
        assistant_text,
        reasoning_trace: reasoning_trace_buffer,
        completed_calls,
        pending_states,
        hosted_finalized: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn collect_hosted_provider_turn<H>(
    stream: &mut std::pin::Pin<
        Box<dyn futures::Stream<Item = anyhow::Result<ProviderChunk>> + Send>,
    >,
    session: &mut Session,
    previous_response_handle: &mut Option<ResponseHandle>,
    handler: &mut H,
    cancellation: Option<&crate::RunCancellationHandle>,
    processor: &std::sync::Arc<dyn crate::HostedEvidenceProcessor>,
    context: crate::HostedFinalizationContext,
    physical_attempt: &mut ProviderPhysicalAttemptAudit,
    generation_observed: &mut bool,
    partial_output: &mut ProviderTurnPartialOutput,
    pricing_snapshot: Option<&crate::ModelPricingSnapshotV1>,
) -> Result<ProviderTurnOutput>
where
    H: EventHandler + Send,
{
    let mut buffer = crate::HostedTurnBuffer::new(crate::HostedTurnBufferLimits::default());
    let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut completed_calls = Vec::new();
    let mut completed_call_ids = std::collections::BTreeSet::new();
    let mut total_tool_args_bytes = 0usize;
    loop {
        let next = match cancellation {
            Some(cancellation) => tokio::select! {
                biased;
                _ = cancellation.cancelled() => anyhow::bail!("hosted provider turn cancelled before safe finalization"),
                chunk = stream.next() => chunk,
            },
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.context("hosted provider stream failed before safe finalization")?;
        if matches!(chunk, ProviderChunk::Done) {
            break;
        }
        if !matches!(chunk, ProviderChunk::ToolCallStreamError(_)) {
            *generation_observed = true;
        }
        match chunk {
            ProviderChunk::ToolCallStart { id, name } => {
                partial_output.observes_tool_call();
                validate_streamed_tool_identity(&id, &name)?;
                if tool_parts.len() >= MAX_PROVIDER_TURN_TOOL_CALLS
                    || tool_parts.contains_key(&id)
                    || completed_call_ids.contains(&id)
                {
                    anyhow::bail!("tool_call_stream_invalid: invalid hosted mixed-tool identity");
                }
                tool_parts.insert(id, (name, String::new()));
            }
            ProviderChunk::ToolCallArgsDelta { id, delta } => {
                crate::persistence::validate_tool_call_id(&id)?;
                let Some((_, args)) = tool_parts.get_mut(&id) else {
                    anyhow::bail!(
                        "tool_call_stream_invalid: hosted mixed-tool args arrived before start"
                    );
                };
                let next_call_bytes = args.len().saturating_add(delta.len());
                let next_total_bytes = total_tool_args_bytes.saturating_add(delta.len());
                if next_call_bytes > MAX_STREAMED_TOOL_ARGS_BYTES
                    || next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES
                {
                    anyhow::bail!(
                        "tool_args_too_large: hosted mixed-tool arguments exceeded limit"
                    );
                }
                args.push_str(&delta);
                total_tool_args_bytes = next_total_bytes;
            }
            ProviderChunk::ToolCallComplete(call) => {
                partial_output.observes_tool_call();
                validate_streamed_tool_identity(&call.id, &call.name)?;
                if completed_calls.len() >= MAX_PROVIDER_TURN_TOOL_CALLS
                    || !completed_call_ids.insert(call.id.clone())
                {
                    anyhow::bail!("tool_call_stream_invalid: invalid hosted mixed-tool completion");
                }
                if let Some((streamed_name, streamed_args)) = tool_parts.remove(&call.id) {
                    if streamed_name != call.name || streamed_args != call.args_json {
                        anyhow::bail!(
                            "tool_call_stream_invalid: hosted mixed-tool completion drifted"
                        );
                    }
                } else {
                    let next_total_bytes =
                        total_tool_args_bytes.saturating_add(call.args_json.len());
                    if next_total_bytes > MAX_PROVIDER_TURN_TOOL_ARGS_BYTES {
                        anyhow::bail!(
                            "tool_args_too_large: hosted mixed-tool arguments exceeded limit"
                        );
                    }
                    total_tool_args_bytes = next_total_bytes;
                }
                completed_calls.push(crate::project_tool_call_for_persistence(call)?);
            }
            ProviderChunk::ToolCallStreamError(error) => return Err(error.into()),
            chunk => buffer.push(chunk)?,
        }
    }
    if !tool_parts.is_empty() {
        anyhow::bail!("tool_call_stream_invalid: hosted turn ended with incomplete client tools");
    }
    if buffer.provider_failed() {
        return Err(crate::HostedTurnError::ProviderFailed.into());
    }
    if cancellation.is_some_and(crate::RunCancellationHandle::is_cancel_requested) {
        anyhow::bail!("hosted provider turn cancelled before safe finalization");
    }
    let finalized = processor
        .finalize(context, &buffer)
        .await
        .map_err(|error| crate::HostedTurnError::FinalizationFailed(format!("{error:#}")))?;

    for usage in buffer.usages() {
        let mut usage = usage.clone();
        let mutation = physical_attempt.cache_layout_mutation();
        usage
            .cache_usage
            .get_or_insert(crate::CacheUsageV1 {
                schema_version: crate::CacheUsageV1::SCHEMA_VERSION,
                read: None,
                write: None,
                uncached: None,
                local_layout_mutation: None,
                provider_miss_without_local_mutation: false,
            })
            .observe_local_layout(mutation);
        if let Some(cache_usage) = &usage.cache_usage {
            cache_usage.validate_for_prompt_tokens(usage.prompt_tokens)?;
        }
        if let Some(snapshot) = pricing_snapshot {
            usage = snapshot.apply_to_usage(usage)?;
        }
        session.stats_mut().apply_usage(&usage);
        physical_attempt
            .append_output_control(session, ControlEntry::UsageSnapshot(usage.clone()))
            .await?;
        handler.handle(RunEvent::Usage(usage))?;
    }
    for handle in buffer.response_handles() {
        *previous_response_handle = Some(handle.clone());
        let control = ControlEntry::ResponseHandleTracked(handle.clone());
        physical_attempt
            .append_output_control(session, control.clone())
            .await?;
        handler.handle(RunEvent::Control(control))?;
    }
    for handle in buffer.background_accepted() {
        let control = ControlEntry::BackgroundTaskTracked(handle.clone());
        physical_attempt
            .append_output_control(session, control.clone())
            .await?;
        handler.handle(RunEvent::Control(control))?;
    }
    for status in buffer.background_statuses() {
        handler.handle(RunEvent::Notice(format!(
            "background task {} status {}",
            status.task_id, status.status
        )))?;
    }
    let pending_states = buffer.continuation_states().to_vec();
    for state in &pending_states {
        handler.handle(RunEvent::ContinuationState(state.clone()))?;
    }
    if !finalized.reasoning_trace.is_empty() {
        handler.handle(RunEvent::ReasoningDelta(finalized.reasoning_trace.clone()))?;
    }
    if !finalized.assistant_text.is_empty() {
        handler.handle(RunEvent::TextDelta(finalized.assistant_text.clone()))?;
    }
    for projection in &completed_calls {
        handler.handle(RunEvent::ToolCallStarted(ToolCall {
            id: projection.durable_call.id.clone(),
            name: projection.durable_call.name.clone(),
            args_json: String::new(),
        }))?;
        handler.handle(RunEvent::ToolCallCompleted(projection.durable_call.clone()))?;
    }

    Ok(ProviderTurnOutput {
        assistant_text: finalized.assistant_text.clone(),
        reasoning_trace: finalized.reasoning_trace.clone(),
        completed_calls,
        pending_states,
        hosted_finalized: Some(finalized),
    })
}

fn provider_chunk_observes_generation(chunk: &ProviderChunk) -> bool {
    matches!(
        chunk,
        ProviderChunk::TextDelta(_)
            | ProviderChunk::ReasoningDelta(_)
            | ProviderChunk::ReasoningSummaryDelta(_)
            | ProviderChunk::ToolCallStart { .. }
            | ProviderChunk::ToolCallArgsDelta { .. }
            | ProviderChunk::ToolCallComplete(_)
            | ProviderChunk::Usage(_)
            | ProviderChunk::ResponseHandle(_)
            | ProviderChunk::BackgroundTaskAccepted(_)
            | ProviderChunk::BackgroundTaskStatus(_)
            | ProviderChunk::ReasoningArtifact(_)
            | ProviderChunk::ContinuationState(_)
            | ProviderChunk::HostedToolStarted { .. }
            | ProviderChunk::HostedEvidence { .. }
            | ProviderChunk::HostedToolFailed { .. }
            | ProviderChunk::HostedRequestUsage { .. }
    )
}

fn validate_streamed_tool_identity(id: &str, name: &str) -> Result<()> {
    crate::persistence::validate_tool_call_id(id)?;
    crate::persistence::validate_tool_call_name(name)?;
    Ok(())
}
