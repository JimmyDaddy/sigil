use super::*;

pub(super) fn dispatch_session_command<P>(
    context: WorkerCommandContext<'_, P>,
    command: SessionCommand,
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
        options: _,
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
            SessionCommand::ReadToolArtifactPage {
                request_id,
                artifact_ref,
                selector,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::ToolArtifactPageReadFailed {
                        request_id,
                        artifact_ref,
                        failure: ToolArtifactDisplayReadFailure::Rejected,
                        entries: state
                            .session
                            .current
                            .as_ref()
                            .map(|session| session.entries().to_vec())
                            .unwrap_or_default(),
                    });
                    continue;
                }
                let budget = state.session.tool_artifact_read_budget.clone();
                let Some(session) = state.session.current.as_mut() else {
                    let _ = message_tx.send(WorkerMessage::ToolArtifactPageReadFailed {
                        request_id,
                        artifact_ref,
                        failure: ToolArtifactDisplayReadFailure::Rejected,
                        entries: Vec::new(),
                    });
                    continue;
                };
                match read_tool_artifact_page_for_display(session, &budget, &artifact_ref, selector)
                {
                    Ok(page) => {
                        let _ = message_tx.send(WorkerMessage::ToolArtifactPageRead {
                            request_id,
                            page,
                            entries: session.entries().to_vec(),
                        });
                    }
                    Err(failure) => {
                        let _ = message_tx.send(WorkerMessage::ToolArtifactPageReadFailed {
                            request_id,
                            artifact_ref,
                            failure,
                            entries: session.entries().to_vec(),
                        });
                    }
                }
            }
            SessionCommand::InspectLocalSession {
                request_id,
                source_path,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot inspect session actions while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match inspect_local_session(&service, &source_path) {
                    Ok(entry) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionInspected { request_id, entry });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::ForkLocalSession {
                request_id,
                source_path,
                current_model_route,
            } => {
                if let Err(error) =
                    ensure_session_transition_allowed(SessionTransitionKind::LocalFork, state)
                {
                    let _ = message_tx
                        .send(WorkerMessage::LocalSessionLifecycleFailed { request_id, error });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                let active_session = local_session_lifecycle_service_for_source(
                    root_config,
                    workspace_root,
                    &source_path,
                )
                .and_then(|_| {
                    state
                        .session
                        .current
                        .as_ref()
                        .map(|session| (state.session.log_path.as_path(), session))
                });
                let output = match fork_local_session(
                    &service,
                    &source_path,
                    active_session,
                    root_config,
                    &current_model_route,
                ) {
                    Ok(output) => output,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                        continue;
                    }
                };
                match transition_session(
                    SessionTransitionKind::LocalFork,
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
                        let _ = message_tx.send(WorkerMessage::LocalSessionForked {
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
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!(
                                "conversation fork created but session switch failed: {error:#}"
                            ),
                        });
                    }
                }
            }
            SessionCommand::ExportLocalSession {
                request_id,
                source_path,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot export a local session while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match export_local_session(&service, &source_path) {
                    Ok(output) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionExported { request_id, output });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::SetLocalSessionPin {
                request_id,
                source_path,
                pinned,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot change a session pin while the agent is running".to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match set_local_session_pin(&service, &source_path, pinned) {
                    Ok(entry) => {
                        if std::fs::canonicalize(&source_path).ok()
                            == std::fs::canonicalize(&state.session.log_path).ok()
                        {
                            state.session.artifact_gc_dirty = true;
                        }
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionPinChanged { request_id, entry });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::PreviewLocalSessionDelete {
                request_id,
                source_path,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot preview session deletion while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match preview_local_session_delete(
                    &service,
                    &source_path,
                    std::slice::from_ref(&state.session.log_path),
                ) {
                    Ok(preview) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionDeletePreviewed {
                            request_id,
                            preview,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::ApplyLocalSessionDelete {
                request_id,
                preview,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot delete a local session while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match apply_local_session_delete(
                    &service,
                    &preview,
                    std::slice::from_ref(&state.session.log_path),
                ) {
                    Ok(output) => {
                        let _ = message_tx
                            .send(WorkerMessage::LocalSessionDeleted { request_id, output });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::PreviewSessionRetention { request_id, policy } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot preview session retention while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match preview_session_retention(
                    &service,
                    policy,
                    std::slice::from_ref(&state.session.log_path),
                ) {
                    Ok(preview) => {
                        let _ = message_tx.send(WorkerMessage::SessionRetentionPreviewed {
                            request_id,
                            preview,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::ApplySessionRetention {
                request_id,
                preview,
            } => {
                if state.run.active.is_some() {
                    let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                        request_id,
                        error: "cannot apply session retention while the agent is running"
                            .to_owned(),
                    });
                    continue;
                }
                let service = local_session_lifecycle_service(root_config, workspace_root);
                match apply_session_retention(
                    &service,
                    &preview,
                    std::slice::from_ref(&state.session.log_path),
                ) {
                    Ok(output) => {
                        let _ = message_tx
                            .send(WorkerMessage::SessionRetentionApplied { request_id, output });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::LocalSessionLifecycleFailed {
                            request_id,
                            error: format!("{error:#}"),
                        });
                    }
                }
            }
            SessionCommand::SwitchSession { session_log_path } => {
                cancel_all_mcp_oauth_flows(state);
                match transition_session(
                    SessionTransitionKind::Switch,
                    session_log_path,
                    runtime,
                    root_config,
                    provider_capabilities,
                    workspace_root,
                    agent,
                    state,
                    message_tx,
                ) {
                    Ok(transition) => {
                        let _ = message_tx.send(WorkerMessage::SessionSwitched {
                            session_log_path: transition.session_log_path,
                            provider_name: transition.provider_name,
                            model_name: transition.model_name,
                            entries: transition.entries,
                        });
                        return WorkerCommandDispatchControl::Break;
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
            SessionCommand::StartNewSession { session_log_path } => {
                cancel_all_mcp_oauth_flows(state);
                match transition_session(
                    SessionTransitionKind::StartNew,
                    session_log_path,
                    runtime,
                    root_config,
                    provider_capabilities,
                    workspace_root,
                    agent,
                    state,
                    message_tx,
                ) {
                    Ok(transition) => {
                        let _ = message_tx.send(WorkerMessage::NewSessionStarted {
                            session_log_path: transition.session_log_path,
                            provider_name: transition.provider_name,
                            model_name: transition.model_name,
                            entries: transition.entries,
                        });
                        return WorkerCommandDispatchControl::Break;
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    }
                }
            }
        }
    }
    control
}

pub(in crate::runner) fn read_tool_artifact_page_for_display(
    session: &mut Session,
    budget: &ToolArtifactReadBudgetV1,
    artifact_ref: &sigil_kernel::ToolArtifactRefV1,
    selector: sigil_kernel::ToolArtifactSelectorV1,
) -> std::result::Result<sigil_kernel::ToolArtifactPageV1, ToolArtifactDisplayReadFailure> {
    if artifact_ref.validate().is_err() || selector.validate().is_err() {
        return Err(ToolArtifactDisplayReadFailure::Rejected);
    }
    let projection = session
        .active_projection_snapshot()
        .map_err(|_| ToolArtifactDisplayReadFailure::AuditUnavailable)?
        .ok_or(ToolArtifactDisplayReadFailure::AuditUnavailable)?;
    let pressure = projection.tool_output_pressure();
    let binding = pressure
        .artifact_source_binding(artifact_ref)
        .ok_or(ToolArtifactDisplayReadFailure::Rejected)?;
    let active_epoch_id = pressure.active_epoch_id.clone();
    if binding.artifact_availability != sigil_kernel::ToolArtifactAvailability::Available {
        return fail_tool_artifact_display_read_with_receipt(
            session,
            artifact_ref,
            selector,
            &binding.source_event_id,
            &active_epoch_id,
            &binding.artifact_sha256,
            ToolArtifactDisplayReadFailure::Unavailable(binding.artifact_availability),
            sigil_kernel::ToolArtifactReadOutcome::Unavailable,
        );
    }

    let store = session
        .tool_artifact_store()
        .ok_or(ToolArtifactDisplayReadFailure::AuditUnavailable)?;
    let descriptor = match store.resolve(artifact_ref) {
        Ok(descriptor) => descriptor,
        Err(_) => {
            return fail_tool_artifact_display_read_with_receipt(
                session,
                artifact_ref,
                selector,
                &binding.source_event_id,
                &active_epoch_id,
                &binding.artifact_sha256,
                ToolArtifactDisplayReadFailure::Unavailable(
                    sigil_kernel::ToolArtifactAvailability::Missing,
                ),
                sigil_kernel::ToolArtifactReadOutcome::Unavailable,
            );
        }
    };
    if descriptor.content_sha256 != binding.artifact_sha256
        || descriptor.persisted_bytes != binding.persisted_bytes
        || descriptor.tool_call_id != binding.call_id
        || descriptor.tool_name != binding.tool_name
    {
        return fail_tool_artifact_display_read_with_receipt(
            session,
            artifact_ref,
            selector,
            &binding.source_event_id,
            &active_epoch_id,
            &binding.artifact_sha256,
            ToolArtifactDisplayReadFailure::Unavailable(
                sigil_kernel::ToolArtifactAvailability::HashMismatch,
            ),
            sigil_kernel::ToolArtifactReadOutcome::Corrupt,
        );
    }
    let availability = store.availability(&descriptor);
    if availability != sigil_kernel::ToolArtifactAvailability::Available {
        let outcome = if availability == sigil_kernel::ToolArtifactAvailability::HashMismatch {
            sigil_kernel::ToolArtifactReadOutcome::Corrupt
        } else {
            sigil_kernel::ToolArtifactReadOutcome::Unavailable
        };
        return fail_tool_artifact_display_read_with_receipt(
            session,
            artifact_ref,
            selector,
            &binding.source_event_id,
            &active_epoch_id,
            &binding.artifact_sha256,
            ToolArtifactDisplayReadFailure::Unavailable(availability),
            outcome,
        );
    }
    if descriptor.retrieval_policy == sigil_kernel::ToolArtifactRetrievalPolicyV1::Unavailable {
        return fail_tool_artifact_display_read_with_receipt(
            session,
            artifact_ref,
            selector,
            &binding.source_event_id,
            &active_epoch_id,
            &binding.artifact_sha256,
            ToolArtifactDisplayReadFailure::Unavailable(
                sigil_kernel::ToolArtifactAvailability::PolicyRevoked,
            ),
            sigil_kernel::ToolArtifactReadOutcome::Rejected,
        );
    }

    let call_id = format!("tui-artifact-read-{}", uuid::Uuid::new_v4().simple());
    let read = match budget.read_page_for_call(&store, artifact_ref, selector.clone(), &call_id) {
        Ok(read) => read,
        Err(_) => {
            let (reads, bytes) = budget.usage();
            let failure = if reads >= sigil_kernel::TOOL_ARTIFACT_READS_PER_TURN
                || bytes >= sigil_kernel::TOOL_ARTIFACT_READ_BYTES_PER_TURN
            {
                ToolArtifactDisplayReadFailure::BudgetExhausted
            } else {
                ToolArtifactDisplayReadFailure::Rejected
            };
            return fail_tool_artifact_display_read_with_receipt(
                session,
                artifact_ref,
                selector,
                &binding.source_event_id,
                &active_epoch_id,
                &binding.artifact_sha256,
                failure,
                sigil_kernel::ToolArtifactReadOutcome::Rejected,
            );
        }
    };
    let outcome = if read.deduplicated_from_call_id.is_some() {
        sigil_kernel::ToolArtifactReadOutcome::Unchanged
    } else {
        sigil_kernel::ToolArtifactReadOutcome::Returned
    };
    let receipt = sigil_kernel::ToolArtifactReadRecordedV1 {
        schema_version: sigil_kernel::TOOL_ARTIFACT_READ_SCHEMA_VERSION,
        call_id,
        artifact_ref: artifact_ref.clone(),
        source_descriptor_event_id: binding.source_event_id,
        active_epoch_id,
        selector,
        returned_bytes: read.page.returned_bytes,
        page_sha256: read.page.page_sha256.clone(),
        artifact_sha256: read.page.artifact_sha256.clone(),
        outcome,
        deduplicated_from_call_id: read.deduplicated_from_call_id,
    };
    receipt
        .validate()
        .map_err(|_| ToolArtifactDisplayReadFailure::AuditUnavailable)?;
    session
        .append_control(ControlEntry::ToolArtifactRead(receipt))
        .map_err(|_| ToolArtifactDisplayReadFailure::AuditUnavailable)?;
    Ok(read.page)
}

#[allow(clippy::too_many_arguments)]
fn fail_tool_artifact_display_read_with_receipt(
    session: &mut Session,
    artifact_ref: &sigil_kernel::ToolArtifactRefV1,
    selector: sigil_kernel::ToolArtifactSelectorV1,
    source_descriptor_event_id: &str,
    active_epoch_id: &str,
    artifact_sha256: &str,
    failure: ToolArtifactDisplayReadFailure,
    outcome: sigil_kernel::ToolArtifactReadOutcome,
) -> std::result::Result<sigil_kernel::ToolArtifactPageV1, ToolArtifactDisplayReadFailure> {
    let receipt = sigil_kernel::ToolArtifactReadRecordedV1 {
        schema_version: sigil_kernel::TOOL_ARTIFACT_READ_SCHEMA_VERSION,
        call_id: format!("tui-artifact-read-{}", uuid::Uuid::new_v4().simple()),
        artifact_ref: artifact_ref.clone(),
        source_descriptor_event_id: source_descriptor_event_id.to_owned(),
        active_epoch_id: active_epoch_id.to_owned(),
        selector,
        returned_bytes: 0,
        page_sha256: sigil_kernel::stable_event_hash([]),
        artifact_sha256: artifact_sha256.to_owned(),
        outcome,
        deduplicated_from_call_id: None,
    };
    receipt
        .validate()
        .map_err(|_| ToolArtifactDisplayReadFailure::AuditUnavailable)?;
    session
        .append_control(ControlEntry::ToolArtifactRead(receipt))
        .map_err(|_| ToolArtifactDisplayReadFailure::AuditUnavailable)?;
    Err(failure)
}
