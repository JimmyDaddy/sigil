use super::*;

const MAX_INTEGRATION_REVIEW_DIFF_BYTES: usize = 64 * 1024;

pub(super) fn dispatch_verification_checkpoint_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: VerificationCheckpointCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent,
        root_config,
        provider_capabilities,
        workspace_root,
        options,
        message_tx,
        elicitation_handler: _,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver: _,
        state,
    } = context;
    let mut command_result = Some(command);
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            VerificationCheckpointCommand::CheckChangedFilesDiagnostics => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot check changes while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let changed_paths = match changed_source_files(&options.workspace_root) {
                    Ok(paths) => paths,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        continue;
                    }
                };
                if changed_paths.is_empty() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "no changed source files to check".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = state.session.current.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                match check_changed_files_diagnostics(
                    runtime,
                    agent.tool_registry(),
                    session,
                    options,
                    root_config.code_intelligence.max_results,
                    changed_paths,
                ) {
                    Ok(result) => {
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(
                            diagnostics_tool_event(result),
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            VerificationCheckpointCommand::PreviewCheckpointRestore {
                request_id,
                request,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                        request_id,
                        error: "cannot preview checkpoint restore while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                match preview_current_checkpoint_restore(
                    &state.session.log_path,
                    state.session.current.as_ref(),
                    workspace_root,
                    &request,
                ) {
                    Ok(preview) => {
                        let _ = message_tx.send(WorkerMessage::CheckpointRestorePreviewed {
                            request_id,
                            preview,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                    }
                }
            }
            VerificationCheckpointCommand::ExecuteCheckpointRestore {
                request_id,
                request,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                        request_id,
                        error: "cannot restore checkpoint while the agent is running".to_owned(),
                    });
                    continue;
                }
                let output = match execute_current_checkpoint_restore(
                    &state.session.log_path,
                    state.session.current.as_ref(),
                    workspace_root,
                    &request,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                        continue;
                    }
                };
                match load_session_with_runtime_attachments(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &state.session.log_path,
                    state.session.current.as_ref(),
                ) {
                    Ok(session) => {
                        let entries = session.entries().to_vec();
                        state.session.current = Some(session);
                        let _ = message_tx.send(WorkerMessage::CheckpointRestoreCompleted {
                            request_id,
                            preview: output.preview,
                            batch_id: output.batch_id,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                            request_id,
                            error: format!(
                                "checkpoint restored but session reload failed: {error:#}"
                            ),
                        });
                    }
                }
            }
            VerificationCheckpointCommand::ForkConversationAtCheckpoint {
                request_id,
                request,
            } => {
                if let Err(error) =
                    ensure_session_transition_allowed(SessionTransitionKind::CheckpointFork, state)
                {
                    let _ = message_tx
                        .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                    continue;
                }
                let output = match fork_current_conversation(
                    &state.session.log_path,
                    state.session.current.as_ref(),
                    root_config,
                    &request,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::CheckpointOperationFailed { request_id, error });
                        continue;
                    }
                };
                match transition_session(
                    SessionTransitionKind::CheckpointFork,
                    output.destination_path.clone(),
                    runtime,
                    root_config,
                    provider_capabilities,
                    workspace_root,
                    agent,
                    state,
                    message_tx,
                ) {
                    Ok(transition) => {
                        let _ = message_tx.send(WorkerMessage::ConversationForked {
                            request_id,
                            session_log_path: transition.session_log_path,
                            provider_name: transition.provider_name,
                            model_name: transition.model_name,
                            copied_message_count: output.copied_message_count,
                            entries: transition.entries,
                        });
                        return WorkerCommandDispatchControl::Break;
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                            request_id,
                            error: format!(
                                "conversation fork created but session switch failed: {error:#}"
                            ),
                        });
                    }
                }
            }
            VerificationCheckpointCommand::CleanMutationArtifacts { target } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before cleaning mutation artifacts".to_owned(),
                    ));
                    continue;
                }
                match clean_mutation_artifacts(
                    root_config,
                    &state.session.log_path,
                    &state.session.current,
                    &target,
                ) {
                    Ok(report) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            format_mutation_artifact_cleanup_report(&report),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            VerificationCheckpointCommand::DeleteMutationArtifact { artifact_id } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before deleting mutation artifacts".to_owned(),
                    ));
                    continue;
                }
                match delete_mutation_artifact(
                    &state.session.log_path,
                    &state.session.current,
                    &artifact_id,
                ) {
                    Ok(payload) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            format_mutation_artifact_delete_report(&payload),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            VerificationCheckpointCommand::ApproveVerificationCheck { check_spec_id } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before approving verification checks".to_owned(),
                    ));
                    continue;
                }
                match promote_workspace_verification_check(
                    &options.workspace_root,
                    root_config,
                    &mut state.session.current,
                    &check_spec_id,
                    VerificationCheckPromotionKind::Approve,
                ) {
                    Ok(VerificationCheckPromotionOutcome::AlreadyPromoted { check_spec_id }) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check already approved: {check_spec_id}"
                        )));
                    }
                    Ok(VerificationCheckPromotionOutcome::Promoted { entry }) => {
                        let check_spec_id = entry.trusted_check.check_spec.check_spec_id.clone();
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(RunEvent::Control(
                            ControlEntry::CheckSpecRecorded(*entry),
                        ))));
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check approved: {check_spec_id}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            VerificationCheckpointCommand::SandboxVerificationCheck { check_spec_id } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before sandboxing verification checks".to_owned(),
                    ));
                    continue;
                }
                match promote_workspace_verification_check(
                    &options.workspace_root,
                    root_config,
                    &mut state.session.current,
                    &check_spec_id,
                    VerificationCheckPromotionKind::Sandbox,
                ) {
                    Ok(VerificationCheckPromotionOutcome::AlreadyPromoted { check_spec_id }) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check already sandboxed: {check_spec_id}"
                        )));
                    }
                    Ok(VerificationCheckPromotionOutcome::Promoted { entry }) => {
                        let check_spec_id = entry.trusted_check.check_spec.check_spec_id.clone();
                        let _ = message_tx.send(WorkerMessage::Event(Box::new(RunEvent::Control(
                            ControlEntry::CheckSpecRecorded(*entry),
                        ))));
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check sandboxed: {check_spec_id}"
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            VerificationCheckpointCommand::RerunTaskVerification { request } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "wait for the active run before running verification".to_owned(),
                    ));
                    continue;
                }
                let Some(session) = state.session.current.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "verification rerun requires an active session".to_owned(),
                    ));
                    continue;
                };
                let execution_backend =
                    match sigil_runtime::build_configured_execution_backend(root_config) {
                        Ok(backend) => backend,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                                "failed to build verification execution backend: {error:#}"
                            )));
                            continue;
                        }
                    };
                let mut handler = ChannelEventHandler::new(message_tx.clone());
                match runtime.block_on(rerun_task_verification_check(
                    session,
                    &mut handler,
                    execution_backend.as_ref(),
                    &options.workspace_root,
                    &request,
                )) {
                    Ok(output) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "verification check {} {}",
                            output.check_run.check_spec_id,
                            match output.check_run.status {
                                sigil_kernel::VerificationCheckRunStatus::Succeeded => "passed",
                                sigil_kernel::VerificationCheckRunStatus::Failed => "failed",
                                sigil_kernel::VerificationCheckRunStatus::Skipped => "skipped",
                                sigil_kernel::VerificationCheckRunStatus::Inconclusive => {
                                    "inconclusive"
                                }
                                sigil_kernel::VerificationCheckRunStatus::Errored => "errored",
                                sigil_kernel::VerificationCheckRunStatus::Queued
                                | sigil_kernel::VerificationCheckRunStatus::Running => "finished",
                            }
                        )));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "verification rerun failed: {error:#}"
                        )));
                    }
                }
            }
            VerificationCheckpointCommand::ReviewTaskIntegration { request } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::TaskIntegrationReviewFailed {
                        request,
                        error: "wait for the active run before reviewing integration".to_owned(),
                    });
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::TaskIntegrationReviewFailed {
                        request,
                        error: "integration review requires an active session".to_owned(),
                    });
                    continue;
                };
                match load_task_integration_review(session, &request) {
                    Ok(aggregate_diff) => {
                        let _ = message_tx.send(WorkerMessage::TaskIntegrationReviewLoaded {
                            request,
                            aggregate_diff,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx
                            .send(WorkerMessage::TaskIntegrationReviewFailed { request, error });
                    }
                }
            }
            VerificationCheckpointCommand::AcceptTaskIntegration { request } => {
                if state.run.active.is_some() {
                    let entries = state
                        .session
                        .current
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::TaskIntegrationAcceptanceFailed {
                        request,
                        error: "wait for the active run before accepting integration".to_owned(),
                        entries,
                    });
                    continue;
                }
                let execution_backend =
                    match sigil_runtime::build_configured_execution_backend(root_config) {
                        Ok(backend) => backend,
                        Err(error) => {
                            let entries = state
                                .session
                                .current
                                .as_ref()
                                .map(|session| session.entries().to_vec())
                                .unwrap_or_default();
                            let _ =
                                message_tx.send(WorkerMessage::TaskIntegrationAcceptanceFailed {
                                    request,
                                    error: format!(
                                        "failed to build parent verification backend: {error:#}"
                                    ),
                                    entries,
                                });
                            continue;
                        }
                    };
                let Some(session) = state.session.current.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::TaskIntegrationAcceptanceFailed {
                        request,
                        error: "integration acceptance requires an active session".to_owned(),
                        entries: Vec::new(),
                    });
                    continue;
                };
                let mut handler = ChannelEventHandler::new(message_tx.clone());
                match runtime.block_on(
                    sigil_runtime::integration_lanes::accept_task_integration_review(
                        session,
                        &mut handler,
                        execution_backend,
                        &options.workspace_root,
                        &request,
                    ),
                ) {
                    Ok(output) => {
                        let _ = message_tx.send(WorkerMessage::TaskIntegrationAccepted {
                            request,
                            promotion_status: output.promotion.record.status,
                            parent_verdict: output
                                .parent_verification
                                .map(|parent| parent.record.verdict),
                            entries: session.entries().to_vec(),
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::TaskIntegrationAcceptanceFailed {
                            request,
                            error: format!("{error:#}"),
                            entries: session.entries().to_vec(),
                        });
                    }
                }
            }
        }
    }
    control
}

fn load_task_integration_review(
    session: &Session,
    request: &sigil_kernel::TaskIntegrationReviewRequest,
) -> std::result::Result<String, String> {
    let product = sigil_kernel::task_integration_review_product(session.entries())
        .ok_or_else(|| "integration review is no longer current".to_owned())?;
    if product.request != *request || request.validate_for_preview(&product.preview).is_err() {
        return Err("integration review request is stale or belongs to another preview".to_owned());
    }
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| "integration review requires a durable artifact store".to_owned())?;
    let bytes = recorder
        .read_immutable_content_artifact(&product.preview.aggregate_diff_artifact_ref)
        .map_err(|error| format!("failed to read integration diff artifact: {error:#}"))?;
    if bytes.is_empty() {
        return Err("integration diff artifact is empty".to_owned());
    }
    if bytes.len() > MAX_INTEGRATION_REVIEW_DIFF_BYTES {
        return Err(format!(
            "integration diff artifact exceeds the {} byte review limit",
            MAX_INTEGRATION_REVIEW_DIFF_BYTES
        ));
    }
    let digest = format!("sha256:{}", sigil_kernel::sha256_hex(&bytes));
    if digest != product.preview.aggregate_diff_digest {
        return Err("integration diff artifact digest does not match the preview".to_owned());
    }
    String::from_utf8(bytes)
        .map_err(|_| "integration diff artifact is not valid UTF-8 text".to_owned())
}
