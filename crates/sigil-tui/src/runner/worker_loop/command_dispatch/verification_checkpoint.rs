use super::*;

pub(super) fn dispatch_verification_checkpoint_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: WorkerCommand,
) -> WorkerCommandDispatchControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerCommandContext {
        runtime,
        agent,
        root_config,
        provider_capabilities: _,
        workspace_root,
        options,
        message_tx,
        elicitation_handler: _,
        mcp_event_handler: _,
        role_provider_builder: _,
        context_resolver: _,
        state,
    } = context;
    let mut command_result: Option<Result<WorkerCommand, mpsc::RecvTimeoutError>> =
        Some(Ok(command));
    let control = WorkerCommandDispatchControl::Continue;
    while let Some(command_result) = command_result.take() {
        match command_result {
            Ok(WorkerCommand::CheckChangedFilesDiagnostics) => {
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
            Ok(WorkerCommand::PreviewCheckpointRestore {
                request_id,
                request,
            }) => {
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
            Ok(WorkerCommand::ExecuteCheckpointRestore {
                request_id,
                request,
            }) => {
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
            Ok(WorkerCommand::ForkConversationAtCheckpoint {
                request_id,
                request,
            }) => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                        request_id,
                        error: "cannot fork conversation while the agent is running".to_owned(),
                    });
                    continue;
                }
                let output = match fork_current_conversation(
                    &state.session.log_path,
                    state.session.current.as_ref(),
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
                    &output.destination_path,
                    state.session.current.as_ref(),
                ) {
                    Ok(mut session) => {
                        if state.session.current.as_ref().is_some_and(|session| {
                            session_workspace_is_trusted(session, workspace_root)
                        }) && let Err(error) = ensure_session_workspace_trust(
                            &mut session,
                            workspace_root,
                            "trusted workspace carried into conversation fork",
                        ) {
                            let _ = message_tx.send(WorkerMessage::CheckpointOperationFailed {
                                request_id,
                                error,
                            });
                            continue;
                        }
                        state.session.exact_prompts.clear();
                        let entries = session.entries().to_vec();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        state.session.log_path = output.destination_path.clone();
                        state.session.current = Some(session);
                        let _ = message_tx.send(WorkerMessage::ConversationForked {
                            request_id,
                            session_log_path: output.destination_path,
                            provider_name,
                            model_name,
                            copied_message_count: output.copied_message_count,
                            entries,
                        });
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
            Ok(WorkerCommand::CleanMutationArtifacts { target }) => {
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
            Ok(WorkerCommand::DeleteMutationArtifact { artifact_id }) => {
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
            Ok(WorkerCommand::ApproveVerificationCheck { check_spec_id }) => {
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
            Ok(WorkerCommand::SandboxVerificationCheck { check_spec_id }) => {
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
            Ok(WorkerCommand::RerunTaskVerification { request }) => {
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
            Ok(command) => unreachable!(
                "exhaustive classifier routed an unexpected command to verification: {command:?}"
            ),
            Err(error) => unreachable!("owned command dispatch received channel error: {error}"),
        }
    }
    control
}
