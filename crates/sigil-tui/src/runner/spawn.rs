use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

use anyhow::{Context, Result};
use sigil_kernel::{
    Agent, EgressAuditRecorder, EgressDisclosurePresenter, ExtensionProcessNetworkAdmission,
    InteractionMode, JsonlSessionStore, McpServerStartup, MutationEventRecorder,
    ProviderCapabilities, ResolvedModelRoute, RootConfig, Session, SessionLogEntry, WorkspaceTrust,
    workspace_trust_from_entries,
};
use sigil_runtime::{McpElicitationHandler, McpRuntimeEventHandler};
use tokio::runtime::Runtime;

use super::{
    elicitation_bridge::ChannelMcpElicitationHandler,
    mcp_event_bridge::ChannelMcpRuntimeEventHandler,
    protocol::{McpActivationStatus, WorkerCommandSender, WorkerMessage},
    terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter,
    worker_event::WorkerMcpRuntimeEventSender,
    worker_loop::{
        RuntimeTaskRoleProviderBuilder, WorkerLoopMcpHandlers, WorkerLoopSessionAttachment,
        WorkerLoopTerminalRuntime, run_worker_loop,
    },
};

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkerSessionRouteDirective {
    pub(crate) recovery_confirmation: Option<String>,
    pub(crate) explicit_selection: Option<(String, ResolvedModelRoute)>,
}

pub(crate) struct SpawnedAgentWorker {
    pub(crate) command_tx: WorkerCommandSender,
    pub(crate) message_rx: mpsc::Receiver<WorkerMessage>,
    pub(crate) join_handle: thread::JoinHandle<()>,
}

pub fn spawn_agent_worker(
    root_config: RootConfig,
    config_path: PathBuf,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
) -> Result<(WorkerCommandSender, mpsc::Receiver<WorkerMessage>)> {
    let worker = spawn_agent_worker_with_route_directive(
        root_config,
        config_path,
        session_log_path,
        workspace_root,
        interaction_mode,
        WorkerSessionRouteDirective::default(),
    )?;
    let SpawnedAgentWorker {
        command_tx,
        message_rx,
        join_handle,
    } = worker;
    drop(join_handle);
    Ok((command_tx, message_rx))
}

pub(crate) fn spawn_agent_worker_with_route_directive(
    root_config: RootConfig,
    config_path: PathBuf,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
    route_directive: WorkerSessionRouteDirective,
) -> Result<SpawnedAgentWorker> {
    spawn_agent_worker_with_route_directive_and_attachment(
        root_config,
        config_path,
        session_log_path,
        workspace_root,
        interaction_mode,
        route_directive,
        None,
        None,
        None,
    )
}

pub(crate) fn spawn_agent_worker_with_route_directive_and_attachment(
    root_config: RootConfig,
    config_path: PathBuf,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
    route_directive: WorkerSessionRouteDirective,
    authority_composition: Option<
        std::sync::Arc<sigil_runtime::r71_authority_composition::RuntimeAuthorityCompositionV1>,
    >,
    boot_cutover: Option<std::sync::Arc<sigil_runtime::r71_global_cutover::RuntimeGlobalCutoverV1>>,
    supplied_attachment: Option<
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    >,
) -> Result<SpawnedAgentWorker> {
    // Production launch must receive the current-schema composition from the boot owner. The
    // no-composition branch is retained only for this crate's unit fixtures; it opens the
    // explicit test session directly and never selects or persists a legacy epoch.
    let boot_cutover = match authority_composition.is_some() {
        true => Some(boot_cutover.ok_or_else(|| {
            anyhow::anyhow!(
                "current-schema boot cutover is required when authority composition is attached"
            )
        })?),
        false => {
            #[cfg(test)]
            {
                let _ = boot_cutover;
                None
            }
            #[cfg(not(test))]
            {
                anyhow::bail!(
                    "current-schema authority composition is required before worker spawn"
                )
            }
        }
    };
    let session_epoch = sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema;
    if let Some(boot_cutover) = boot_cutover.as_ref() {
        boot_cutover
            .admit_session_open(session_epoch)
            .map_err(anyhow::Error::new)?;
    }
    let authority_composition = authority_composition;
    let attachment_lease = if let Some(attachment) = supplied_attachment {
        let store = match boot_cutover.as_ref() {
            Some(boot_cutover) => sigil_runtime::r71_global_cutover::guarded_session_open(
                &session_log_path,
                boot_cutover.as_ref(),
                session_epoch,
            )
            .map_err(anyhow::Error::new)?,
            None => JsonlSessionStore::new(&session_log_path)?,
        };
        anyhow::ensure!(
            attachment.session_path() == store.path(),
            "transferred worker attachment belongs to another durable session"
        );
        attachment
    } else {
        Arc::new(
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                &session_log_path,
            )
            .map_err(anyhow::Error::new)?,
        )
    };
    let (_, route, route_rebound) = initialize_worker_session_route(
        &root_config,
        &session_log_path,
        &route_directive,
        attachment_lease.as_ref(),
    )?;
    let (event_tx, event_rx) = mpsc::channel();
    let (urgent_tx, urgent_rx) = mpsc::channel();
    let command_tx = WorkerCommandSender::new(event_tx.clone(), urgent_tx);
    let (message_tx, message_rx) = mpsc::channel();

    let join_handle = thread::Builder::new()
        .name("sigil-agent-worker".to_owned())
        .spawn(move || {
            tracing::debug!(
                attached = authority_composition.is_some(),
                "rfc-0071: authority composition state"
            );
            let Some(runtime) = report_runtime_build_result(build_worker_runtime(), &message_tx)
            else {
                return;
            };

            let provider = match runtime.block_on(
                sigil_runtime::build_provider_for_model_ref_async(&root_config, &route.model_ref),
            ) {
                Ok(provider) => provider,
                Err(error) => {
                    tracing::debug!(%error, "provider startup is unavailable");
                    send_worker_startup_recovery(
                        &message_tx,
                        sigil_kernel::PublicRouteRecoveryCode::ProviderUnavailable,
                        vec![
                            sigil_kernel::PublicRouteRecoveryAction::RetryProvider,
                            sigil_kernel::PublicRouteRecoveryAction::RepairConnection,
                            sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                            sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                        ],
                        true,
                    );
                    return;
                }
            };
            let permission_mode_override =
                std::sync::Arc::new(sigil_kernel::PermissionModeOverride::new());
            let mut options = sigil_runtime::build_run_options(
                &root_config,
                workspace_root.clone(),
                interaction_mode,
                Some(permission_mode_override.as_ref().clone()),
            );
            if let Some(composition) = authority_composition.as_ref() {
                options = options.with_tool_authority(std::sync::Arc::new(
                    composition.tool_authority.clone(),
                ));
            }
            let managed_extension_execution = authority_composition
                .as_ref()
                .map(|composition| Arc::clone(&composition.extension_execution));
            let managed_verification_execution = authority_composition.as_ref().map(|composition| {
                Arc::clone(&composition.command_execution)
                    as Arc<dyn sigil_kernel::verification::VerificationExecutionPortV1>
            });
            let managed_plan_review_child_resources = authority_composition
                .as_ref()
                .map(|composition| composition.plan_review_child_resource_provisioner());
            let extension_network_admission = ExtensionProcessNetworkAdmission::new(
                options.permission_context.network_policy,
                false,
            );
            let provider_capabilities = provider.capabilities();
            let elicitation_handler =
                Arc::new(ChannelMcpElicitationHandler::new(message_tx.clone()));
            let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(
                WorkerMcpRuntimeEventSender::new(event_tx.clone()),
            ));
            let (session_entries, workspace_trust) =
                match load_session_entries_with_workspace_trust(&session_log_path, &workspace_root)
                {
                    Ok(projection) => projection,
                    Err(error) => {
                        tracing::debug!(%error, "session stream startup is unavailable");
                        send_worker_startup_recovery(
                            &message_tx,
                            sigil_kernel::PublicRouteRecoveryCode::SessionStreamInvalid,
                            vec![
                                sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                                sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                            ],
                            false,
                        );
                        return;
                    }
                };
            if route_rebound {
                let _ = message_tx.send(WorkerMessage::Notice(
                    "连接配置已更新，已使用当前配置继续；服务端上下文缓存已重置。".to_owned(),
                ));
            }
            // Session-open guard applies inside the worker too: every current-schema session
            // store open must be admitted by the boot decision, fail closed otherwise.
            if let Some(boot_cutover) = boot_cutover.as_ref()
                && let Err(error) = boot_cutover.admit_session_open(session_epoch)
            {
                tracing::debug!(%error, "session epoch guard rejected the open");
                send_worker_startup_recovery(
                    &message_tx,
                    sigil_kernel::PublicRouteRecoveryCode::SessionWriterBusy,
                    vec![
                        sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                        sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    true,
                );
                return;
            }
            let store_result: Result<JsonlSessionStore> = match boot_cutover.as_ref() {
                Some(boot_cutover) => sigil_runtime::r71_global_cutover::guarded_session_open(
                    &session_log_path,
                    boot_cutover.as_ref(),
                    session_epoch,
                )
                .map_err(|error| anyhow::anyhow!(error)),
                None => JsonlSessionStore::new(&session_log_path),
            };
            let store = match store_result {
                Ok(store) => store,
                Err(error) => {
                    tracing::debug!(%error, "session writer startup is unavailable");
                    send_worker_startup_recovery(
                        &message_tx,
                        sigil_kernel::PublicRouteRecoveryCode::SessionWriterBusy,
                        vec![
                            sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                            sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                        ],
                        true,
                    );
                    return;
                }
            };
            let recorder_session = Session::new("runtime", "eager-mcp").with_store(store.clone());
            let egress_recorder = match recorder_session.egress_audit_recorder() {
                Ok(recorder) => recorder,
                Err(error) => {
                    tracing::debug!(%error, "session audit writer startup is unavailable");
                    send_worker_startup_recovery(
                        &message_tx,
                        sigil_kernel::PublicRouteRecoveryCode::SessionWriterBusy,
                        vec![
                            sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                            sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                        ],
                        true,
                    );
                    return;
                }
            };
            let mutation_recorder = MutationEventRecorder::new(store);
            let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());
            let terminal_lifecycle_factory: Arc<
                dyn sigil_kernel::TerminalLifecycleSinkFactory,
            > =
                Arc::new(terminal_lifecycle_router.clone());
            let surface = match sigil_runtime::build_tool_surface_without_eager_mcp_with_workspace_trust_and_terminal_lifecycle_factory_and_managed_extension_execution(
                    &root_config,
                    &provider_capabilities,
                    workspace_root.clone(),
                    elicitation_handler.clone(),
                    mcp_event_handler.clone(),
                    workspace_trust,
                    terminal_lifecycle_factory,
                    managed_extension_execution.clone(),
                ) {
                    Ok(surface) => surface,
                    Err(error) => {
                        tracing::debug!(%error, "tool surface startup is unavailable");
                        send_worker_startup_recovery(
                            &message_tx,
                            sigil_kernel::PublicRouteRecoveryCode::ConnectionConfigInvalid,
                            vec![
                                sigil_kernel::PublicRouteRecoveryAction::RepairConnection,
                                sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                                sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                            ],
                            false,
                        );
                        return;
                    }
                };
            let terminal_control = surface.terminal_control.clone();
            let mut registry = surface.registry;
            let context_resolver = surface.context_resolver;
            let disclosure_presenter: Arc<dyn EgressDisclosurePresenter> =
                if root_config.web.network_mode == sigil_kernel::NetworkPolicy::Ask {
                    Arc::new(
                        super::egress_disclosure_bridge::ChannelEgressDisclosurePresenter::new(
                            message_tx.clone(),
                        ),
                    )
                } else {
                    Arc::new(
                        super::egress_disclosure_bridge::AutoAcceptDisclosurePresenter,
                    )
                };
            sigil_runtime::attach_remote_mcp_activation_presenter_with_managed_extension_execution(
                &mut registry,
                &root_config,
                &provider_capabilities,
                workspace_root.clone(),
                elicitation_handler.clone(),
                mcp_event_handler.clone(),
                Arc::clone(&disclosure_presenter),
                managed_extension_execution.clone(),
            );
            if let Err(error) = sigil_runtime::register_agent_tools_with_workspace_and_entries(
                &mut registry,
                &root_config,
                &workspace_root,
                &session_entries,
            ) {
                tracing::debug!(%error, "agent tool startup is unavailable");
                send_worker_startup_recovery(
                    &message_tx,
                    sigil_kernel::PublicRouteRecoveryCode::ConnectionConfigInvalid,
                    vec![
                        sigil_kernel::PublicRouteRecoveryAction::RepairConnection,
                        sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                        sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                    ],
                    false,
                );
                return;
            }
            spawn_eager_mcp_startup_tasks(
                &runtime,
                registry.clone(),
                &root_config,
                &provider_capabilities,
                workspace_root.clone(),
                &message_tx,
                elicitation_handler.clone(),
                mcp_event_handler.clone(),
                mutation_recorder,
                egress_recorder,
                disclosure_presenter,
                extension_network_admission,
                managed_extension_execution.clone(),
            );
            let managed_artifact_store = match authority_composition.as_ref() {
                Some(composition)
                    if composition.declared_channels.contains(
                        &sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging,
                    ) && composition.declared_channels.contains(
                        &sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore,
                    ) => match super::ManagedTuiArtifactStoreLease::acquire(
                        Arc::clone(&composition.storage_writer),
                        &session_log_path,
                        &sigil_kernel::stable_event_uuid(
                            "sigil-session-path",
                            &session_log_path.to_string_lossy(),
                        ),
                    ) {
                        Ok(lease) => Some(lease),
                        Err(error) => {
                            tracing::debug!(%error, "managed TUI artifact storage startup is unavailable");
                            send_worker_startup_recovery(
                                &message_tx,
                                sigil_kernel::PublicRouteRecoveryCode::SessionWriterBusy,
                                vec![
                                    sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                                    sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                                ],
                                true,
                            );
                            return;
                        }
                    },
                _ => None,
            };
            let agent = Arc::new(Agent::new(provider, registry));
            run_worker_loop(
                runtime,
                agent,
                root_config,
                config_path,
                workspace_root,
                WorkerLoopSessionAttachment::from_shared(session_log_path, attachment_lease),
                options,
                permission_mode_override,
                (event_tx, event_rx, urgent_rx),
                message_tx,
                WorkerLoopMcpHandlers {
                    elicitation_handler,
                    event_handler: mcp_event_handler,
                    role_provider_builder: Arc::new(RuntimeTaskRoleProviderBuilder),
                    context_resolver,
                    managed_extension_execution,
                    managed_verification_execution,
                    managed_plan_review_child_resources,
                },
                WorkerLoopTerminalRuntime::new(
                    terminal_lifecycle_router,
                    Some(terminal_control),
                )
                .with_scratch_control(surface.scratch_control),
                authority_composition
                    .as_ref()
                    .map(|composition| Arc::clone(&composition.storage_writer)),
                managed_artifact_store,
            );
        })
        .context("failed to spawn sigil agent worker")?;

    Ok(SpawnedAgentWorker {
        command_tx,
        message_rx,
        join_handle,
    })
}

fn initialize_worker_session_route(
    root_config: &RootConfig,
    session_log_path: &Path,
    directive: &WorkerSessionRouteDirective,
    attachment: &sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
) -> Result<(String, ResolvedModelRoute, bool)> {
    let (_, fallback_route) =
        sigil_runtime::provider_connections::resolve_default_model_route(root_config)
            .map_err(anyhow::Error::new)
            .context("model_route_not_configured: complete provider setup before starting")?;
    let previous_route = JsonlSessionStore::read_entries(session_log_path)?
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::SessionIdentity {
                resolved_model_route,
                ..
            }) => resolved_model_route.clone(),
            SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionModelSelected {
                    resolved_model_route,
                    ..
                }
                | sigil_kernel::ControlEntry::SessionRouteRebound {
                    resolved_model_route,
                    ..
                },
            ) => Some(resolved_model_route.clone()),
            _ => None,
        });
    let store = JsonlSessionStore::new(session_log_path)?;
    let session = sigil_runtime::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            directive.recovery_confirmation.as_deref(),
            directive
                .explicit_selection
                .as_ref()
                .map(|(provider_name, route)| (provider_name.as_str(), route)),
            Some(attachment),
        )?;
    let route = session.resolved_model_route().cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "session_route_missing: durable session has no frozen connection route; \
             start a new session"
        )
    })?;
    let provider_name = session.provider_name().to_owned();
    anyhow::ensure!(
        route.model_ref.model_id == session.model_name(),
        "session_route_drift: durable model identity does not match its frozen route"
    );
    let route_rebound = previous_route
        .as_ref()
        .is_some_and(|previous| previous != &route);
    Ok((provider_name, route, route_rebound))
}

pub(super) fn load_session_entries_with_workspace_trust(
    session_log_path: &Path,
    workspace_root: &Path,
) -> Result<(Vec<SessionLogEntry>, WorkspaceTrust)> {
    let entries = JsonlSessionStore::read_entries(session_log_path)?;
    let workspace_trust = workspace_trust_from_entries(&entries, workspace_root)?;
    Ok((entries, workspace_trust))
}

#[allow(clippy::too_many_arguments)]
fn spawn_eager_mcp_startup_tasks(
    runtime: &Runtime,
    registry: sigil_kernel::ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    mcp_event_handler: Arc<ChannelMcpRuntimeEventHandler>,
    mutation_recorder: MutationEventRecorder,
    egress_recorder: EgressAuditRecorder,
    disclosure_presenter: Arc<dyn EgressDisclosurePresenter>,
    network_admission: ExtensionProcessNetworkAdmission,
    managed_extension_execution: Option<
        Arc<sigil_runtime::managed_resource_adapters::RuntimeManagedExtensionExecutionRouteV1>,
    >,
) {
    for server in root_config
        .mcp_servers
        .iter()
        .filter(|server| server.startup == McpServerStartup::Eager)
    {
        let server_name = server.name.clone();
        let _ = message_tx.send(WorkerMessage::McpActivationStatus {
            server_name: Some(server_name.clone()),
            status: McpActivationStatus::Activating,
        });

        let mut registry = registry.clone();
        let mut root_config = root_config.clone();
        for configured in &mut root_config.mcp_servers {
            if configured.name == server_name {
                configured.required = true;
            }
        }
        let provider_capabilities = provider_capabilities.clone();
        let workspace_root = workspace_root.clone();
        let message_tx = message_tx.clone();
        let elicitation_handler: Arc<dyn McpElicitationHandler> = elicitation_handler.clone();
        let mcp_event_handler: Arc<dyn McpRuntimeEventHandler> = mcp_event_handler.clone();
        let mutation_recorder = mutation_recorder.clone();
        let egress_recorder = egress_recorder.clone();
        let disclosure_presenter = Arc::clone(&disclosure_presenter);
        let is_remote = server.streamable_http().is_some();

        let activation = runtime.block_on(async {
            if is_remote {
                sigil_runtime::activate_eager_remote_mcp_server(
                    &mut registry,
                    &root_config,
                    &server_name,
                    provider_capabilities.tool_name_max_chars,
                    workspace_root,
                    egress_recorder,
                    disclosure_presenter,
                    elicitation_handler,
                )
                .await
                .map(|added_tools| sigil_runtime::McpRefreshResult {
                    matched_servers: 1,
                    added_tools,
                    removed_tools: 0,
                    process_launch_receipts: Vec::new(),
                })
            } else {
                sigil_runtime::refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder_and_network_admission_and_managed_extension_execution(
                    &mut registry,
                    &root_config,
                    &provider_capabilities,
                    workspace_root,
                    &server_name,
                    elicitation_handler,
                    mcp_event_handler,
                    Some(mutation_recorder),
                    network_admission,
                    managed_extension_execution.clone(),
                )
                .await
            }
        });
        match activation {
            Ok(result) => {
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::Ready {
                        added_tools: result.added_tools,
                        process_coverage: sigil_runtime::mcp_process_receipts_summary(
                            &result.process_launch_receipts,
                        ),
                    },
                });
            }
            Err(error) => {
                let error = format!("{error:#}");
                let _ = message_tx.send(WorkerMessage::McpActivationStatus {
                    server_name: Some(server_name.clone()),
                    status: McpActivationStatus::from_error(error),
                });
            }
        }
    }
}

fn build_worker_runtime() -> Result<Runtime, std::io::Error> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}

fn send_worker_startup_recovery(
    message_tx: &mpsc::Sender<WorkerMessage>,
    code: sigil_kernel::PublicRouteRecoveryCode,
    actions: Vec<sigil_kernel::PublicRouteRecoveryAction>,
    retryable: bool,
) {
    let _ = message_tx.send(WorkerMessage::SessionRouteRecoveryRequired {
        code,
        actions,
        recovery_binding: String::new(),
        retryable,
        target_session: None,
    });
}

pub(super) fn report_runtime_build_result(
    result: Result<Runtime, std::io::Error>,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> Option<Runtime> {
    match result {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            tracing::debug!(%error, "worker runtime startup is unavailable");
            send_worker_startup_recovery(
                message_tx,
                sigil_kernel::PublicRouteRecoveryCode::ProviderUnavailable,
                vec![
                    sigil_kernel::PublicRouteRecoveryAction::RetryProvider,
                    sigil_kernel::PublicRouteRecoveryAction::StartNewSession,
                    sigil_kernel::PublicRouteRecoveryAction::BackToSessionLibrary,
                ],
                true,
            );
            None
        }
    }
}
