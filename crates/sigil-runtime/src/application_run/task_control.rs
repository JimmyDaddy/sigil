use super::*;

/// Input required to prepare one application-owned durable Task continuation.
#[derive(Debug, Clone)]
pub struct ApplicationTaskContinuationRequest {
    /// Resolved Sigil config path.
    pub config_path: PathBuf,
    /// Process launch working directory.
    pub launch_cwd: PathBuf,
    /// Exact durable V2 session path.
    pub session_path: PathBuf,
    /// Optional controller-owned cross-process attachment for the exact durable session.
    pub session_attachment:
        Option<Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>>,
    /// Durable session scope rendered to the application before this command.
    pub expected_session_scope_id: String,
    /// Adapter-owned run identifier for this continuation attempt.
    pub run_id: String,
    /// Exact durable Task selected by the user or application.
    pub task_id: TaskId,
    /// Optional user guidance applied by the orchestrator at its continuation safe point.
    pub guidance: Option<String>,
    /// Whether the adapter can provide explicit approvals after execution starts.
    pub interaction: ApplicationRunInteraction,
    /// Optional user-selected permission mode for this continuation.
    pub permission_mode: Option<PermissionMode>,
}

/// Prepared durable Task continuation and its root cancellation authority.
pub struct PreparedApplicationTaskContinuation {
    execution: ApplicationTaskContinuationExecution,
    control: ApplicationRunControl,
    terminal_control: ApplicationTerminalTaskControl,
}

impl PreparedApplicationTaskContinuation {
    /// Returns the typed persistent-terminal owner retained beyond the foreground Task turn.
    #[must_use]
    pub fn terminal_control(&self) -> ApplicationTerminalTaskControl {
        self.terminal_control.clone()
    }

    /// Separates Task execution from the root cancellation authority retained by the adapter.
    #[must_use]
    pub fn into_parts(self) -> (ApplicationTaskContinuationExecution, ApplicationRunControl) {
        (self.execution, self.control)
    }

    /// Returns the exact durable Task selected for continuation.
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.execution.task.task_id
    }

    /// Returns the adapter-owned continuation run identifier.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.execution.run_id
    }

    /// Returns the durable session scope.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.execution.session_id
    }

    /// Returns the exact durable V2 session path.
    #[must_use]
    pub fn session_log_path(&self) -> &Path {
        &self.execution.session_log_path
    }
}

/// Application-owned execution of one exact durable Task continuation.
pub struct ApplicationTaskContinuationExecution {
    task: crate::agent_supervisor::task_execution::ResolvedTaskContinuation,
    guidance: Option<String>,
    public_prompt: String,
    task_execution: ApplicationTaskExecutionRuntime,
    session: Session,
    session_id: String,
    run_id: String,
    session_log_path: PathBuf,
    cancellation_handle: RunCancellationHandle,
    root_task_guard: RunTaskGuard,
    warnings: Vec<String>,
    redactor: sigil_kernel::SecretRedactor,
    interaction: ApplicationRunInteraction,
    conversation_lifecycle: ConversationRunLifecycleRecorder,
    conversation_start: ConversationRunStartedEntryV1,
    events: ApplicationRunEventSequence,
    route_transition: crate::provider_connections::SessionRouteTransitionView,
    managed_session_log: Option<ManagedApplicationSessionLogLease>,
    managed_artifact_store: Option<ManagedApplicationArtifactStoreLease>,
    _session_lease: Arc<ApplicationSessionLease>,
}

/// Successful terminal output from one application-owned durable Task continuation.
#[derive(Debug, Clone)]
pub struct ApplicationTaskContinuationOutput {
    /// Durable session scope.
    pub session_id: String,
    /// Adapter-owned continuation run identifier.
    pub run_id: String,
    /// Exact durable Task that was continued.
    pub task_id: TaskId,
    /// Durable V2 JSONL path.
    pub session_log_path: PathBuf,
    /// Durable Task status reached by this continuation attempt.
    pub task_status: TaskRunStatus,
    /// Terminal application classification projected from `task_status`.
    pub terminal_status: ApplicationRunTerminalStatus,
    /// Machine-readable receipt for the route admitted by this continuation.
    pub route_transition: crate::provider_connections::SessionRouteTransitionView,
    /// Safe final answer when the Task completed.
    pub final_text: Option<String>,
}

/// Prepares one exact durable Task continuation under the shared application session lease.
///
/// No user conversation message is synthesized. The exact Task is resolved from durable state,
/// then the cancellation scope is bound before execution authority is returned.
///
/// # Errors
///
/// Returns a typed preparation error when the session or Task binding is stale, Task execution is
/// disabled, another foreground operation owns the session, or provider/tool assembly fails.
pub async fn prepare_application_task_continuation(
    request: ApplicationTaskContinuationRequest,
    services: &ApplicationRunServices,
) -> std::result::Result<PreparedApplicationTaskContinuation, ApplicationRunPrepareError> {
    if request.expected_session_scope_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "expected Task continuation session scope must not be empty".to_owned(),
        });
    }
    if request.run_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "Task continuation run id must not be empty".to_owned(),
        });
    }
    if request
        .guidance
        .as_deref()
        .is_some_and(|guidance| guidance.trim().is_empty())
    {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "Task continuation guidance must not be empty".to_owned(),
        });
    }
    if !services.task_executor_attached() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "application Task continuation requires an attached Task executor".to_owned(),
        });
    }
    application_bound_session_entries(&request.session_path, &request.expected_session_scope_id)
        .map_err(ApplicationRunPrepareError::execution)?;
    let conversation_start =
        ConversationRunStartedEntryV1::new(request.run_id.clone(), current_unix_time_ms())
            .map_err(|error| ApplicationRunPrepareError::InvalidInvocation {
                message: safe_persistence_text(&error.to_string()),
            })?;
    let public_prompt = request.guidance.as_deref().map_or_else(
        || format!("Continue task {}", request.task_id.as_str()),
        safe_persistence_text,
    );
    let blocking_request = ApplicationRunRequest {
        config_path: request.config_path,
        launch_cwd: request.launch_cwd,
        prompt: public_prompt.clone(),
        run_id: request.run_id,
        session_path: Some(request.session_path),
        session_attachment: request.session_attachment,
        interaction: request.interaction,
        permission_mode: request.permission_mode,
        model_connection_id: None,
        model_name: None,
        model_selection_binding: None,
        route_recovery_binding: None,
        reasoning_effort: None,
        reasoning_effort_binding: None,
        skill_binding: None,
        agent_binding: None,
        constraints: None,
    };
    let session_leases = Arc::clone(&services.session_leases);
    let managed_session_log_writer = current_schema_managed_session_log_writer(services);
    let managed_artifact_store_writer = current_schema_managed_artifact_store_writer(services);
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_application_run_blocking_with_writer(
            blocking_request,
            session_leases,
            true,
            None,
            managed_session_log_writer,
            managed_artifact_store_writer,
        )
    })
    .await
    .map_err(|error| ApplicationRunPrepareError::Internal {
        source: anyhow!(error).context("application Task continuation preparation worker failed"),
    })??;
    let BlockingApplicationRunPreparation {
        mut root_config,
        workspace_root,
        session_path,
        session_lease,
        mutation_recorder,
        mut session,
        workspace_trust,
        cancellation_recorder,
        cancellation_owner,
        cancellation_handle,
        root_task_guard,
        model_ref,
        options,
        run_id,
        interaction,
        redactor,
        task_agent_registry,
        route_transition,
        managed_session_log,
        managed_artifact_store,
        ..
    } = prepared;
    if session.session_scope_id() != request.expected_session_scope_id {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "durable session identity changed before Task continuation".to_owned(),
        });
    }
    let task = crate::agent_supervisor::task_execution::resolve_task_continuation(
        &session,
        Some(request.task_id.as_str()),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    if task.needs_planning() && request.guidance.is_some() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message:
                "recovered Task has no accepted plan; continue it without guidance to rerun the planner"
                    .to_owned(),
        });
    }
    let task_agent_registry =
        task_agent_registry.ok_or_else(|| ApplicationRunPrepareError::InvalidInvocation {
            message: "Task execution is disabled for this application session".to_owned(),
        })?;
    let role_provider_builder = services
        .task_role_provider_builder
        .as_ref()
        .ok_or_else(|| ApplicationRunPrepareError::Internal {
            source: anyhow!("attached Task executor disappeared during preparation"),
        })?;
    let provider = crate::build_provider_for_model_ref_async(&root_config, &model_ref)
        .await
        .map_err(ApplicationRunPrepareError::provider_unavailable)?;
    let provider_capabilities = provider.capabilities();
    let orchestration_route_guard = crate::OrchestrationRouteGuard::new(
        session.provider_name(),
        session.model_name(),
        crate::ORCHESTRATION_RUNTIME_BUILD_ID,
    );
    orchestration_route_guard
        .enforce(&mut session, current_unix_time_ms())
        .map_err(ApplicationRunPrepareError::execution)?;
    orchestration_route_guard.apply_effective_task_config(&session, &mut root_config.task);
    let terminal_lifecycle_sink = Arc::new(crate::ApplicationTerminalLifecycleRouter::new(
        mutation_recorder.clone(),
        session.session_scope_id(),
        &run_id,
        services.terminal_lifecycle_handler.clone(),
    )) as Arc<dyn sigil_kernel::TerminalLifecycleSink>;
    let (surface, warnings) = assemble_application_tool_surface(
        &root_config,
        &provider_capabilities,
        &workspace_root,
        mutation_recorder,
        workspace_trust,
        &options,
        &session,
        services,
        &redactor,
        None,
        None,
        terminal_lifecycle_sink,
    )
    .await
    .map_err(ApplicationRunPrepareError::execution)?;
    crate::agent_supervisor::task_execution::bind_task_run_cancellation_scope(
        &mut session,
        &task.task_id,
        &cancellation_handle,
    )
    .map_err(ApplicationRunPrepareError::execution)?;

    let session_id = session.session_scope_id().to_owned();
    let conversation_lifecycle = session
        .conversation_run_lifecycle_recorder()
        .map_err(ApplicationRunPrepareError::execution)?;
    let events = ApplicationRunEventSequence::with_outbox(
        session_id.clone(),
        run_id.clone(),
        PublicEventOutboxRecorder::new(
            JsonlSessionStore::new(&session_path).map_err(ApplicationRunPrepareError::execution)?,
        ),
    );
    let task_execution = ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        workspace_root: workspace_root.clone(),
        parent_session_ref: task.parent_session_ref.clone(),
        options,
        base_registry: surface.registry,
        agent_supervisor: crate::AgentSupervisor::new(
            task_agent_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            provider_capabilities,
        ),
        role_provider_builder: Arc::clone(role_provider_builder),
    };
    let terminal_control = ApplicationTerminalTaskControl::new(
        workspace_root.clone(),
        surface.terminal_control,
        session_lease.as_ref(),
        session.session_scope_id(),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    Ok(PreparedApplicationTaskContinuation {
        execution: ApplicationTaskContinuationExecution {
            task: task.clone(),
            guidance: request.guidance,
            public_prompt,
            task_execution,
            session,
            session_id,
            run_id,
            session_log_path: session_path,
            cancellation_handle,
            root_task_guard,
            warnings,
            redactor,
            interaction,
            conversation_lifecycle: conversation_lifecycle.clone(),
            conversation_start: conversation_start.clone(),
            events: events.clone(),
            route_transition,
            managed_session_log,
            managed_artifact_store,
            _session_lease: Arc::clone(&session_lease),
        },
        control: ApplicationRunControl {
            owner: cancellation_owner,
            recorder: cancellation_recorder,
            cancellation_target: RunCancellationTarget::Task {
                task_id: task.task_id.as_str().to_owned(),
            },
            conversation_lifecycle,
            conversation_start,
            events,
            _session_lease: session_lease,
        },
        terminal_control,
    })
}

impl ApplicationTaskContinuationExecution {
    /// Executes the prepared Task continuation with adapter-provided event and approval handlers.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable Task changed, role execution failed, or the adapter
    /// rejected an ordered public event.
    pub async fn execute<H, A>(
        self,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<ApplicationTaskContinuationOutput>
    where
        H: ApplicationRunEventHandler + Send,
        A: ApprovalHandler + Send,
    {
        validate_execution_contract(self.interaction, approval_handler, false)?;
        self.execute_inner(handler, approval_handler).await
    }

    /// Executes an externally interactive Task continuation on an owned blocking worker.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval contract is not explicit, the blocking worker cannot
    /// join, or Task execution fails.
    pub async fn execute_on_owned_blocking<H, A>(
        self,
        mut handler: H,
        mut approval_handler: A,
    ) -> Result<ApplicationTaskContinuationOutput>
    where
        H: ApplicationRunEventHandler + Send + 'static,
        A: ApprovalHandler + Send + 'static,
    {
        validate_execution_contract(self.interaction, &approval_handler, true)?;
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            runtime.block_on(self.execute_inner(&mut handler, &mut approval_handler))
        })
        .await
        .context("application Task continuation owned blocking worker failed")?
    }

    async fn execute_inner<H, A>(
        mut self,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<ApplicationTaskContinuationOutput>
    where
        H: ApplicationRunEventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let _root_task_guard = self.root_task_guard;
        self.conversation_lifecycle
            .append_started(&self.conversation_start)
            .context("failed to persist application Task continuation start")?;
        let mut bridge = PublicApplicationEventBridge::new(self.events.clone(), handler);
        bridge.emit(PublicRunEventKind::RunStarted {
            prompt: self.public_prompt,
        })?;
        bridge.emit(PublicRunEventKind::RouteTransition {
            transition: application_public_route_transition(&self.route_transition),
        })?;
        for warning in std::mem::take(&mut self.warnings) {
            bridge.emit(PublicRunEventKind::Notice { message: warning })?;
        }

        let ApplicationTaskExecutionRuntime {
            root_config,
            workspace_root: _,
            parent_session_ref: _,
            options,
            base_registry,
            agent_supervisor,
            role_provider_builder,
        } = self.task_execution;
        let continuation_entry_frontier = self.session.entries().len();
        let result = crate::agent_supervisor::task_execution::continue_task_execution(
            &mut self.session,
            crate::agent_supervisor::task_execution::ContinuedTaskExecution {
                requested_task_id: Some(self.task.task_id.clone()),
                guidance: self.guidance,
                guidance_promotion: None,
                continuation_guidance_receipt: None,
                root_config,
                options,
                base_registry,
                agent_supervisor,
                role_provider_builder: role_provider_builder.as_ref(),
                handler: &mut bridge,
                cancellation_handle: self.cancellation_handle.clone(),
                tool_artifact_read_budget: None,
            },
            approval_handler,
        )
        .await;
        let result = crate::agent_supervisor::task_execution::finalize_task_continuation_root(
            &mut self.session,
            &self.task.task_id,
            &self.task.parent_session_ref,
            &self.task.objective,
            &self.cancellation_handle,
            continuation_entry_frontier,
            result,
        );
        let task_status = match result {
            Ok(status) => status,
            Err(error) if self.cancellation_handle.is_cancel_requested() => {
                return Err(error).context(
                    "application Task cancellation is pending terminal cleanup confirmation",
                );
            }
            Err(error) => {
                let safe_error = self.redactor.redact_text(&format!("{error:#}"));
                append_application_conversation_terminal(
                    &self.conversation_lifecycle,
                    &self.run_id,
                    ConversationRunTerminalStatusV1::Failed,
                    None,
                    Some(&safe_error),
                    &self.redactor,
                )?;
                bridge.emit(PublicRunEventKind::RunFailed { error: safe_error })?;
                return Err(error);
            }
        };
        let (terminal_status, durable_status, final_answer, terminal_event) =
            application_task_continuation_terminal(&self.session, &self.task.task_id, task_status)?;
        let terminal_summary = match &terminal_event {
            PublicRunEventKind::RunFailed { error } => Some(error.as_str()),
            PublicRunEventKind::RunBlocked { reason }
            | PublicRunEventKind::RunPaused { reason }
            | PublicRunEventKind::RunInterrupted { reason } => Some(reason.as_str()),
            _ => None,
        };
        append_application_conversation_terminal(
            &self.conversation_lifecycle,
            &self.run_id,
            durable_status,
            final_answer
                .as_ref()
                .map(|answer| answer.message_id.clone()),
            terminal_summary,
            &self.redactor,
        )?;
        bridge.emit(terminal_event)?;
        if let Some(managed_session_log) = self.managed_session_log.take() {
            managed_session_log
                .finalize()
                .context("failed to finalize managed session-log namespace")?;
        }
        if let Some(managed_artifact_store) = self.managed_artifact_store.take() {
            managed_artifact_store
                .finalize()
                .context("failed to finalize managed artifact namespaces")?;
        }
        Ok(ApplicationTaskContinuationOutput {
            session_id: self.session_id,
            run_id: self.run_id,
            task_id: self.task.task_id,
            session_log_path: self.session_log_path,
            task_status,
            terminal_status,
            route_transition: self.route_transition,
            final_text: final_answer.map(|answer| answer.text),
        })
    }
}

fn application_task_continuation_terminal(
    session: &Session,
    task_id: &TaskId,
    status: TaskRunStatus,
) -> Result<(
    ApplicationRunTerminalStatus,
    ConversationRunTerminalStatusV1,
    Option<ApplicationTaskFinalAnswer>,
    PublicRunEventKind,
)> {
    match status {
        TaskRunStatus::Completed => {
            let answer = application_task_final_answer(session, task_id)?;
            let event = PublicRunEventKind::RunFinished {
                final_text: answer.text.clone(),
            };
            Ok((
                ApplicationRunTerminalStatus::Succeeded,
                ConversationRunTerminalStatusV1::Succeeded,
                Some(answer),
                event,
            ))
        }
        TaskRunStatus::Cancelled => Ok((
            ApplicationRunTerminalStatus::Interrupted,
            ConversationRunTerminalStatusV1::Cancelled,
            None,
            PublicRunEventKind::RunCancelled,
        )),
        TaskRunStatus::Interrupted => Ok((
            ApplicationRunTerminalStatus::Interrupted,
            ConversationRunTerminalStatusV1::Interrupted,
            None,
            PublicRunEventKind::RunInterrupted {
                reason: "Task continuation was interrupted".to_owned(),
            },
        )),
        TaskRunStatus::Paused => Ok((
            ApplicationRunTerminalStatus::Blocked,
            ConversationRunTerminalStatusV1::Blocked,
            None,
            PublicRunEventKind::RunPaused {
                reason: "Task continuation is durably paused".to_owned(),
            },
        )),
        TaskRunStatus::Started | TaskRunStatus::Running => Ok((
            ApplicationRunTerminalStatus::Blocked,
            ConversationRunTerminalStatusV1::Blocked,
            None,
            PublicRunEventKind::RunBlocked {
                reason: format!(
                    "Task continuation stopped with durable status {}",
                    task_status_label(status)
                ),
            },
        )),
        TaskRunStatus::Failed => Ok((
            ApplicationRunTerminalStatus::Blocked,
            ConversationRunTerminalStatusV1::Failed,
            None,
            PublicRunEventKind::RunFailed {
                error: "Task continuation failed".to_owned(),
            },
        )),
    }
}

fn task_status_label(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Started => "started",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Completed => "completed",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Interrupted => "interrupted",
    }
}
