use super::*;

/// Input required to accept one exact durable user-input decision.
#[derive(Debug, Clone)]
pub struct ApplicationUserInputDecisionRequest {
    /// Resolved Sigil config path.
    pub config_path: PathBuf,
    /// Process launch working directory.
    pub launch_cwd: PathBuf,
    /// Exact durable V2 session path.
    pub session_path: PathBuf,
    /// Optional controller-owned attachment for the exact durable session.
    pub session_attachment:
        Option<Arc<crate::interactive_session_attachment::InteractiveSessionAttachmentLease>>,
    /// Durable session scope rendered to the caller before this command.
    pub expected_session_scope_id: String,
    /// Adapter-owned physical run id reserved for a submitted-answer continuation.
    pub run_id: String,
    /// Exact public request identity rendered to the caller.
    pub identity: sigil_kernel::UserInputIdentityV1,
    /// Exact request hash rendered to the caller.
    pub request_hash: String,
    /// Retry-stable command identity.
    pub command_id: sigil_kernel::UserInputCommandId,
    /// Typed decision and bounded answers.
    pub decision: sigil_kernel::UserInputDecisionV1,
    /// Whether the adapter can provide explicit approvals after execution starts.
    pub interaction: ApplicationRunInteraction,
    /// Optional user-selected permission mode for the continuation.
    pub permission_mode: Option<PermissionMode>,
}

/// Accepted decision plus an optional supervised continuation.
pub struct PreparedApplicationUserInputDecision {
    receipt: sigil_kernel::UserInputDecisionReceiptV1,
    continuation: Option<PreparedApplicationRun>,
    revision_request: Option<crate::PlanReviewRunRequest>,
}

impl PreparedApplicationUserInputDecision {
    /// Returns the exact durable decision receipt.
    #[must_use]
    pub fn receipt(&self) -> &sigil_kernel::UserInputDecisionReceiptV1 {
        &self.receipt
    }

    /// Returns whether this decision produced a provider continuation.
    #[must_use]
    pub fn has_continuation(&self) -> bool {
        self.continuation.is_some() || self.revision_request.is_some()
    }

    /// Separates the durable receipt from the optional supervised run.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PreparedApplicationRun>,
        Option<crate::PlanReviewRunRequest>,
    ) {
        (self.receipt, self.continuation, self.revision_request)
    }
}

pub(super) struct ApplicationUserInputContinuationContext {
    pub(super) identity: sigil_kernel::UserInputIdentityV1,
    pub(super) request_hash: String,
    pub(super) supervisor_instance_id: String,
}

/// Reads one exact immutable public user-input request from a durable session.
///
/// # Errors
///
/// Returns an error when the session binding, request identity, or request hash is stale.
pub fn application_user_input_request_view(
    session_path: &Path,
    expected_session_scope_id: &str,
    identity: &sigil_kernel::UserInputIdentityV1,
    request_hash: &str,
) -> Result<sigil_kernel::PublicUserInputRequestV1> {
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    let projection = sigil_kernel::UserInputProjectionV1::from_session_entries(&entries)?;
    if let Some(state) = projection.request(identity) {
        if state.requested.request_hash != request_hash {
            bail!("user input detail does not bind the exact request hash");
        }
        return Ok(state.public_view());
    }
    let plan_reviews = sigil_kernel::PlanReviewProjection::from_entries(&entries);
    plan_reviews
        .attempt_for_pending_user_input(identity, request_hash)
        .and_then(|attempt| attempt.pending_user_input.as_deref())
        .cloned()
        .context("user input detail references an unknown request")
}

/// Reads one exact immutable public request by its adapter-safe key.
///
/// Adapters do not need to accept the full kernel identity from untrusted clients. The durable
/// session scope, request id, generation, and request hash are sufficient to resolve exactly one
/// request; every remaining identity component comes from the append-only session itself.
pub fn application_user_input_request_view_by_key(
    session_path: &Path,
    expected_session_scope_id: &str,
    request_id: &str,
    generation: u32,
    expected_request_hash: &str,
) -> Result<sigil_kernel::PublicUserInputRequestV1> {
    let request_id = sigil_kernel::UserInputRequestId::new(request_id.to_owned())?;
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    let projection = sigil_kernel::UserInputProjectionV1::from_session_entries(&entries)?;
    let request = projection
        .public_requests()
        .into_iter()
        .find(|request| {
            request.identity.request_id == request_id
                && request.identity.generation == generation
                && request.identity.session_scope_id.as_str() == expected_session_scope_id
        })
        .filter(|request| request.request_hash == expected_request_hash);
    if let Some(request) = request {
        return Ok(request);
    }
    let plan_reviews = sigil_kernel::PlanReviewProjection::from_entries(&entries);
    plan_reviews
        .attempt_for_pending_user_input_key(&request_id, generation, expected_request_hash)
        .and_then(|attempt| attempt.pending_user_input.as_deref())
        .cloned()
        .context("user input detail references an unknown request generation")
}

/// Returns whether durable session truth contains an unresolved user-input suspension.
///
/// This is an admission guard: while an answer/tool continuation remains unsettled, an unrelated
/// foreground run must not insert messages into that provider frontier.
pub fn application_session_has_unresolved_user_input(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<bool> {
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    let projection = sigil_kernel::UserInputProjectionV1::from_session_entries(&entries)?;
    if projection.pending().next().is_some() {
        return Ok(true);
    }
    let plan_reviews = sigil_kernel::PlanReviewProjection::from_entries(&entries);
    Ok(plan_reviews.reviews().any(|review| {
        review
            .attempts
            .iter()
            .any(|attempt| attempt.status == sigil_kernel::PlanReviewAttemptStatus::WaitingForInput)
    }))
}

/// Returns the exact private recovery command for one accepted answer whose continuation has not
/// started yet.
///
/// The command is reconstructed from scope-checked durable truth; public adapters never receive
/// the answer values. `None` means there is no ordinary replayable accepted answer in this
/// session.
pub fn application_recoverable_user_input_decision(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Option<sigil_kernel::UserInputDecisionCommandV1>> {
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    sigil_kernel::recoverable_user_input_decision_from_entries(&entries)
}

/// Accepts one exact user-input decision and prepares its continuation when required.
///
/// Decline and cancel decisions close the internal tool call without constructing a provider.
/// Submitted answers first assemble every fallible provider/tool/run dependency and only then
/// cross the durable acceptance boundary. A preparation failure therefore leaves the request in
/// `Requested`, where the same command can be retried without a stranded accepted answer.
///
/// # Errors
///
/// Returns a typed preparation error for stale bindings, invalid answers, unavailable providers,
/// or foreground ownership conflicts. A durably accepted submitted answer remains recoverable if
/// later provider preparation fails.
pub async fn prepare_application_user_input_decision(
    request: ApplicationUserInputDecisionRequest,
    services: &ApplicationRunServices,
) -> std::result::Result<PreparedApplicationUserInputDecision, ApplicationRunPrepareError> {
    if request.expected_session_scope_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "expected user-input session scope must not be empty".to_owned(),
        });
    }
    if request.run_id.trim().is_empty() {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "user-input continuation run id must not be empty".to_owned(),
        });
    }
    let initial_entries = application_bound_session_entries(
        &request.session_path,
        &request.expected_session_scope_id,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let initial_projection =
        sigil_kernel::UserInputProjectionV1::from_session_entries(&initial_entries)
            .map_err(ApplicationRunPrepareError::execution)?;
    let plan_review_projection = sigil_kernel::PlanReviewProjection::from_entries(&initial_entries);
    if let Some(attempt) = plan_review_projection
        .attempt_for_pending_user_input(&request.identity, &request.request_hash)
    {
        let pending = attempt
            .pending_user_input
            .as_deref()
            .context("suspended plan review lost its public input request")
            .map_err(ApplicationRunPrepareError::execution)?;
        if !matches!(
            pending.source,
            sigil_kernel::UserInputSourceV1::PlanReviewResearch { .. }
        ) {
            return Err(ApplicationRunPrepareError::InvalidInvocation {
                message: "suspended plan-review request has an invalid source".to_owned(),
            });
        }
        let root_config = RootConfig::load(&request.config_path)
            .map_err(ApplicationRunPrepareError::execution)?;
        let (receipt, revision_request) = crate::application_plan_review_research_input_decision(
            &root_config,
            &request.session_path,
            &request.expected_session_scope_id,
            sigil_kernel::UserInputDecisionCommandV1 {
                identity: request.identity,
                request_hash: request.request_hash,
                command_id: request.command_id,
                decision: request.decision,
            },
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        return Ok(PreparedApplicationUserInputDecision {
            receipt,
            continuation: None,
            revision_request,
        });
    }
    if request.identity.session_scope_id.as_str() != request.expected_session_scope_id {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "user-input identity belongs to a different session scope".to_owned(),
        });
    }
    let initial_state = initial_projection
        .request(&request.identity)
        .context("user input decision references an unknown durable request")
        .map_err(ApplicationRunPrepareError::execution)?;
    if matches!(
        initial_state.requested.request.source,
        sigil_kernel::UserInputSourceV1::PlanRevision { .. }
    ) {
        let root_config = RootConfig::load(&request.config_path)
            .map_err(ApplicationRunPrepareError::execution)?;
        let workspace_root = sigil_kernel::resolve_workspace_root(
            &request.config_path,
            &request.launch_cwd,
            &root_config.workspace.root,
        );
        let (receipt, revision_request) = crate::application_plan_revision_guidance_decision(
            &root_config,
            &workspace_root,
            &request.session_path,
            &request.expected_session_scope_id,
            sigil_kernel::UserInputDecisionCommandV1 {
                identity: request.identity,
                request_hash: request.request_hash,
                command_id: request.command_id,
                decision: request.decision,
            },
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        return Ok(PreparedApplicationUserInputDecision {
            receipt,
            continuation: None,
            revision_request,
        });
    }

    let public_prompt = "Continue after answering a requested question".to_owned();
    let blocking_request = ApplicationRunRequest {
        config_path: request.config_path,
        launch_cwd: request.launch_cwd,
        prompt: public_prompt.clone(),
        run_id: request.run_id.clone(),
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
    let task_executor_attached = services.task_executor_attached();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_application_run_blocking(blocking_request, session_leases, task_executor_attached)
    })
    .await
    .map_err(|error| ApplicationRunPrepareError::Internal {
        source: anyhow!(error).context("application user-input preparation worker failed"),
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
        route_transition,
        ..
    } = prepared;
    if session.session_scope_id() != request.expected_session_scope_id {
        return Err(ApplicationRunPrepareError::InvalidInvocation {
            message: "durable session identity changed before user-input decision".to_owned(),
        });
    }
    let accepted_at_unix_ms = current_unix_time_ms();
    let decision_command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: request.identity.clone(),
        request_hash: request.request_hash.clone(),
        command_id: request.command_id,
        decision: request.decision,
    };
    if !matches!(
        &decision_command.decision,
        sigil_kernel::UserInputDecisionV1::Submitted { .. }
    ) {
        let receipt = sigil_kernel::accept_user_input_decision(
            &mut session,
            decision_command,
            accepted_at_unix_ms,
        )
        .map_err(ApplicationRunPrepareError::execution)?;
        return Ok(PreparedApplicationUserInputDecision {
            receipt,
            continuation: None,
            revision_request: None,
        });
    }
    let preview =
        sigil_kernel::preview_user_input_decision(&session, &decision_command, accepted_at_unix_ms)
            .map_err(ApplicationRunPrepareError::execution)?;
    let provider = crate::build_provider_for_model_ref_async(&root_config, &model_ref)
        .await
        .map_err(ApplicationRunPrepareError::provider_unavailable)?;
    let orchestration_route_guard = crate::OrchestrationRouteGuard::new(
        session.provider_name(),
        session.model_name(),
        crate::ORCHESTRATION_RUNTIME_BUILD_ID,
    );
    orchestration_route_guard
        .enforce(&mut session, current_unix_time_ms())
        .map_err(ApplicationRunPrepareError::execution)?;
    orchestration_route_guard.apply_effective_task_config(&session, &mut root_config.task);
    let conversation_start =
        ConversationRunStartedEntryV1::new(run_id.clone(), current_unix_time_ms()).map_err(
            |error| ApplicationRunPrepareError::InvalidInvocation {
                message: safe_persistence_text(&error.to_string()),
            },
        )?;
    let terminal_lifecycle_sink = Arc::new(crate::ApplicationTerminalLifecycleRouter::new(
        mutation_recorder.clone(),
        session.session_scope_id(),
        &run_id,
        services.terminal_lifecycle_handler.clone(),
    )) as Arc<dyn sigil_kernel::TerminalLifecycleSink>;
    let (surface, warnings) = assemble_application_tool_surface(
        &root_config,
        &provider.capabilities(),
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
    let runtime_context = surface
        .context_resolver
        .resolve(&preview.request.prompt)
        .await
        .unwrap_or_default();
    let continuation_logical_run_id = sigil_kernel::user_input_continuation_logical_run_id(
        &request.identity,
        &request.request_hash,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let input = AgentRunInput::without_persisted_user_message(Vec::new())
        .with_runtime_context(runtime_context)
        .with_logical_run_id(continuation_logical_run_id.as_str())
        .with_user_input_continuation_context(
            request.identity.root_logical_run_id.as_str(),
            request.identity.source_thread_id.clone(),
        )
        .with_cancellation(cancellation_handle.clone())
        .with_pending_input_provider(Arc::new(
            crate::pending_input::DurableQueuePendingInputProvider,
        ));
    let parent_session_ref = SessionRef::new_relative(
        session_path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("session.jsonl"),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let conversation_coordinator = crate::ConversationCoordinator::new(
        root_config.task.enabled,
        root_config.task.routing_policy,
    )
    .with_orchestration_route_guard(orchestration_route_guard);
    let session_id = session.session_scope_id().to_owned();
    let conversation_lifecycle = session
        .conversation_run_lifecycle_recorder()
        .map_err(ApplicationRunPrepareError::execution)?;
    let events = ApplicationRunEventSequence::new(session_id.clone(), run_id.clone());
    let terminal_control = ApplicationTerminalTaskControl::new(
        workspace_root,
        surface.terminal_control,
        session_lease.as_ref(),
        session.session_scope_id(),
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    // This is deliberately the final fallible construction step. Once the answer is durable, the
    // returned value already owns a complete supervised continuation and requires no further
    // provider/config/tool preparation.
    let receipt = sigil_kernel::accept_user_input_decision(
        &mut session,
        decision_command,
        accepted_at_unix_ms,
    )
    .map_err(ApplicationRunPrepareError::execution)?;
    let continuation = PreparedApplicationRun {
        execution: ApplicationRunExecution {
            kind: ApplicationRunExecutionKind::Main {
                agent: Box::new(Agent::new(provider, surface.registry)),
                input: Box::new(input),
            },
            task_execution: None,
            plan_review_runtime: None,
            session,
            options,
            session_id,
            run_id,
            prompt: public_prompt,
            session_log_path: session_path,
            cancellation_handle,
            root_task_guard,
            warnings,
            redactor,
            interaction,
            conversation_lifecycle: conversation_lifecycle.clone(),
            conversation_start: conversation_start.clone(),
            events: events.clone(),
            conversation_coordinator,
            parent_session_ref,
            pending_session_title: None,
            pending_user_input_continuation: Some(ApplicationUserInputContinuationContext {
                identity: request.identity,
                request_hash: request.request_hash,
                supervisor_instance_id: services.supervisor_instance_id.to_string(),
            }),
            route_transition,
            _session_lease: Arc::clone(&session_lease),
        },
        control: ApplicationRunControl {
            owner: cancellation_owner,
            recorder: cancellation_recorder,
            cancellation_target: RunCancellationTarget::Run,
            conversation_lifecycle,
            conversation_start,
            events,
            _session_lease: session_lease,
        },
        terminal_control,
    };
    Ok(PreparedApplicationUserInputDecision {
        receipt,
        continuation: Some(continuation),
        revision_request: None,
    })
}

pub(super) fn start_application_user_input_continuation(
    session: &mut Session,
    context: &ApplicationUserInputContinuationContext,
    physical_attempt_id: &str,
) -> Result<sigil_kernel::PublicUserInputRequestV1> {
    let preparation = sigil_kernel::prepare_user_input_continuation(
        session,
        &context.identity,
        &context.request_hash,
        &context.supervisor_instance_id,
        physical_attempt_id,
        current_unix_time_ms(),
    )?;
    if preparation.continuation.physical_attempt_id != physical_attempt_id {
        bail!("user-input continuation is already bound to another physical attempt");
    }
    Ok(preparation.request)
}

pub(super) fn resolve_application_user_input_continuation(
    session: &mut Session,
    context: &ApplicationUserInputContinuationContext,
    resolution: sigil_kernel::UserInputResolutionV1,
) -> Result<sigil_kernel::PublicUserInputRequestV1> {
    session.append_user_input_lifecycle(vec![
        sigil_kernel::UserInputLifecycleEntryV1::Resolved(sigil_kernel::UserInputResolvedV1 {
            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
            identity: context.identity.clone(),
            request_hash: context.request_hash.clone(),
            resolution,
            resolved_at_unix_ms: current_unix_time_ms(),
        }),
    ])?;
    let projection = session.user_input_projection()?;
    projection
        .request(&context.identity)
        .map(sigil_kernel::UserInputRequestStateV1::public_view)
        .context("resolved user-input continuation lost its durable request")
}

pub(super) fn application_user_input_changed_event(
    request: sigil_kernel::PublicUserInputRequestV1,
) -> PublicRunEventKind {
    PublicRunEventKind::UserInputChanged {
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        request_hash: request.request_hash.clone(),
        status: request.status,
        request: Box::new(request),
    }
}
