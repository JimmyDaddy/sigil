use super::*;

const MAX_EXACT_CONVERSATION_PROMPTS: usize = 128;
const EXACT_PROMPT_REQUIRED_HASH_PREFIX: &str = "exact-required:";

pub(in crate::runner) type ExactConversationPromptStore =
    BTreeMap<ConversationInputQueueId, SecretString>;

/// Durable terminal classification for a promoted queued input.
///
/// `Stale` is deliberately non-replayable: it means a provider attempt may have consumed the
/// request but the durable evidence cannot prove a final delivery outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runner) enum QueuedConversationTerminalClassification {
    Delivered { reason: Option<String> },
    Rejected { reason: String },
    Stale { reason: String },
}

/// A no-write, exact request candidate for the next queued chat input.
///
/// The frozen request and capability registrations can carry exact user material. They are
/// process-local only and must not be logged or persisted. The queue scheduler consumes this
/// candidate behind the durable promotion and pre-send barriers.
#[derive(Clone)]
pub(in crate::runner) struct PreparedQueuedConversationCandidate {
    pub(in crate::runner) promotion: sigil_kernel::ConversationInputPromotedEntry,
    pub(in crate::runner) frozen_request: sigil_kernel::FrozenProviderRequestMaterial,
    pub(in crate::runner) reasoning_effort: Option<ReasoningEffort>,
    pub(in crate::runner) background_ready_context: Vec<ModelMessage>,
    pub(in crate::runner) runtime_context: RuntimeContextCandidates,
    pub(in crate::runner) capability_registrations:
        Vec<sigil_kernel::UserUrlCapabilityRegistration>,
}

impl std::fmt::Debug for PreparedQueuedConversationCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedQueuedConversationCandidate")
            .field("queue_id", &self.promotion.queue_id)
            .field("queue_revision", &self.promotion.expected_queue_revision)
            .field("dispatch_run_id", &self.promotion.dispatch_run_id)
            .field("frozen_request", &self.frozen_request)
            .field("reasoning_effort", &self.reasoning_effort)
            .field(
                "background_ready_context_count",
                &self.background_ready_context.len(),
            )
            .field(
                "capability_registration_count",
                &self.capability_registrations.len(),
            )
            .finish()
    }
}

/// A process-local exact-fit reservation for one frozen queued conversation request.
///
/// The proof carries the versioned provider/tokenizer profile plus the explicit output and safety
/// budget. It is valid only for `candidate.frozen_request`; neither the request bytes nor the
/// raw queued prompt enter the durable stream here.
pub(in crate::runner) struct AdmittedQueuedConversationCandidate {
    pub(in crate::runner) candidate: PreparedQueuedConversationCandidate,
    pub(in crate::runner) token_binding: sigil_kernel::TokenMeasurementBinding,
    pub(in crate::runner) request_fit: sigil_kernel::RequestFitProof,
}

impl std::fmt::Debug for AdmittedQueuedConversationCandidate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdmittedQueuedConversationCandidate")
            .field("candidate", &self.candidate)
            .field("token_binding", &self.token_binding)
            .field("request_fit", &self.request_fit)
            .finish()
    }
}

/// Result of trying to materialize the next queued request without changing session state.
pub(in crate::runner) enum QueuedConversationCandidatePreparation {
    NoQueuedInput,
    Prepared(Box<PreparedQueuedConversationCandidate>),
    Blocked {
        queue_id: ConversationInputQueueId,
        reason: String,
    },
}

impl std::fmt::Debug for QueuedConversationCandidatePreparation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoQueuedInput => {
                formatter.write_str("QueuedConversationCandidatePreparation::NoQueuedInput")
            }
            Self::Prepared(candidate) => formatter
                .debug_tuple("QueuedConversationCandidatePreparation::Prepared")
                .field(candidate)
                .finish(),
            Self::Blocked { queue_id, reason } => formatter
                .debug_struct("QueuedConversationCandidatePreparation::Blocked")
                .field("queue_id", queue_id)
                .field("reason", reason)
                .finish(),
        }
    }
}

/// Result of the local pre-turn pressure admission for one queued conversation input.
///
/// `ExactFit` binds the explicit output/safety reservation to the exact frozen request that the
/// queue scheduler must either send or discard. `Blocked` is deliberately no-write and never guesses a
/// provider output default or downloads a tokenizer.
pub(in crate::runner) enum QueuedConversationPressureAdmission {
    NoQueuedInput,
    ExactFit(Box<AdmittedQueuedConversationCandidate>),
    PortablePreflightRequired {
        candidate: Box<PreparedQueuedConversationCandidate>,
        input_tokens: u64,
        budget: sigil_kernel::EffectiveTokenBudget,
    },
    Blocked {
        queue_id: ConversationInputQueueId,
        reason: String,
        candidate: Option<Box<PreparedQueuedConversationCandidate>>,
    },
}

impl std::fmt::Debug for QueuedConversationPressureAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoQueuedInput => {
                formatter.write_str("QueuedConversationPressureAdmission::NoQueuedInput")
            }
            Self::ExactFit(candidate) => formatter
                .debug_tuple("QueuedConversationPressureAdmission::ExactFit")
                .field(candidate)
                .finish(),
            Self::PortablePreflightRequired {
                candidate,
                input_tokens,
                budget,
            } => formatter
                .debug_struct("QueuedConversationPressureAdmission::PortablePreflightRequired")
                .field("candidate", candidate)
                .field("input_tokens", input_tokens)
                .field("budget", budget)
                .finish(),
            Self::Blocked {
                queue_id,
                reason,
                candidate,
            } => formatter
                .debug_struct("QueuedConversationPressureAdmission::Blocked")
                .field("queue_id", queue_id)
                .field("reason", reason)
                .field("has_frozen_candidate", &candidate.is_some())
                .finish(),
        }
    }
}

/// Builds and freezes a candidate for the current next queued chat input without mutating it.
///
/// This does not promote the input, remove its exact overlay, stage URL capabilities, write a
/// compaction lifecycle, or call a provider. Plan prompts deliberately remain blocked here:
/// their current transient-only plan semantics need their own durable promotion contract before
/// they can use the chat pre-turn path.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(in crate::runner) fn prepare_next_queued_conversation_candidate(
    session: &Session,
    exact_prompts: &ExactConversationPromptStore,
    workspace_root: &Path,
    memory_config: &MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    default_reasoning_effort: Option<ReasoningEffort>,
    traffic_partition_key: Option<String>,
) -> std::result::Result<QueuedConversationCandidatePreparation, String> {
    prepare_next_queued_conversation_candidate_with_target_max_tokens(
        session,
        exact_prompts,
        workspace_root,
        memory_config,
        tools,
        None,
        default_reasoning_effort,
        traffic_partition_key,
        |query| {
            sigil_runtime::context_candidates_from_safe_sources(workspace_root, query, None)
                .unwrap_or_default()
        },
    )
}

/// Builds a pure queued candidate with an explicit provider output reservation when supplied.
///
/// The reservation is only materialized in the frozen request. This helper performs no token
/// proof, durable write, capability staging, or provider I/O.
#[allow(clippy::too_many_arguments)]
fn prepare_next_queued_conversation_candidate_with_target_max_tokens<F>(
    session: &Session,
    exact_prompts: &ExactConversationPromptStore,
    workspace_root: &Path,
    memory_config: &MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    target_max_tokens: Option<u32>,
    default_reasoning_effort: Option<ReasoningEffort>,
    traffic_partition_key: Option<String>,
    resolve_runtime_context: F,
) -> std::result::Result<QueuedConversationCandidatePreparation, String>
where
    F: FnOnce(&str) -> RuntimeContextCandidates,
{
    let Some(durable_queue) = session
        .try_conversation_queue_durable_projection_from_durable()
        .map_err(|error| format!("failed to read durable conversation queue state: {error:#}"))?
    else {
        let Some(queue_id) = session.conversation_queue_projection().next_dispatchable else {
            return Ok(QueuedConversationCandidatePreparation::NoQueuedInput);
        };
        return Ok(QueuedConversationCandidatePreparation::Blocked {
            queue_id,
            reason: "queued pre-turn admission requires a durable session store".to_owned(),
        });
    };
    let Some(queue_id) = durable_queue.queue.next_dispatchable.clone() else {
        return Ok(QueuedConversationCandidatePreparation::NoQueuedInput);
    };
    let Some(queue_revision) = durable_queue.revision.clone() else {
        return Ok(QueuedConversationCandidatePreparation::Blocked {
            queue_id,
            reason: "queued pre-turn admission requires a durable queue revision".to_owned(),
        });
    };
    let Some(queued) = durable_queue
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == queue_id)
        .map(|item| item.queued.clone())
    else {
        return Ok(QueuedConversationCandidatePreparation::Blocked {
            queue_id,
            reason: "next queued input is missing from the durable queue projection".to_owned(),
        });
    };
    if queued.target != ConversationInputTarget::MainThread {
        return Ok(QueuedConversationCandidatePreparation::Blocked {
            queue_id,
            reason: "follow-up target is not dispatchable by the main conversation worker"
                .to_owned(),
        });
    }
    if queued.kind != ConversationInputKind::Chat {
        return Ok(QueuedConversationCandidatePreparation::Blocked {
            queue_id,
            reason: "queued pre-turn admission is not available for this follow-up kind".to_owned(),
        });
    }

    let exact_prompt = match exact_prompts.get(&queued.queue_id) {
        Some(prompt) => prompt.expose_secret().to_owned(),
        None if queued
            .prompt_hash
            .starts_with(EXACT_PROMPT_REQUIRED_HASH_PREFIX) =>
        {
            return Ok(QueuedConversationCandidatePreparation::Blocked {
                queue_id,
                reason: "exact sensitive follow-up was lost after restart".to_owned(),
            });
        }
        None => queued.prompt.clone(),
    };
    let prompt_projection =
        sigil_kernel::project_conversation_prompt_for_persistence(&exact_prompt);
    if prompt_projection.prompt_hash != queued.prompt_hash
        || prompt_projection.safe_prompt != queued.prompt
    {
        return Ok(QueuedConversationCandidatePreparation::Blocked {
            queue_id,
            reason: "queued follow-up exact material no longer matches its durable projection"
                .to_owned(),
        });
    }

    let promotion_seed = format!(
        "{}:{}:{}",
        session.session_scope_id(),
        queued.queue_id.as_str(),
        queue_revision.event_id
    );
    let durable_message_id =
        stable_event_uuid("sigil-queued-conversation-user-message", &promotion_seed);
    let dispatch_run_id = stable_event_uuid("sigil-queued-conversation-dispatch", &promotion_seed);
    let promoted_at_ms = current_unix_time_ms();
    let capability_projection =
        sigil_kernel::project_user_message_for_persistence_with_nonce_and_issued_at(
            durable_message_id.clone(),
            exact_prompt.clone(),
            Some(&dispatch_run_id),
            promoted_at_ms,
            None,
        )
        .map_err(|error| format!("failed to project queued user capabilities: {error:#}"))?;
    let mut capability_registrations = capability_projection.capability_registrations;
    capability_registrations.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let capability_descriptors = capability_registrations
        .iter()
        .map(|registration| registration.durable_descriptor(session.session_scope_id()))
        .collect::<Vec<_>>();
    let capability_digest =
        sigil_kernel::conversation_promotion_capability_digest(&capability_descriptors)
            .map_err(|error| format!("failed to digest queued user capabilities: {error:#}"))?;
    let mut durable_user_message = ModelMessage::user(queued.prompt.clone());
    durable_user_message.id = durable_message_id.clone();
    let promotion = sigil_kernel::ConversationInputPromotedEntry {
        queue_id: queued.queue_id.clone(),
        expected_queue_revision: queue_revision,
        prompt_hash: queued.prompt_hash.clone(),
        exact_prompt_required: prompt_projection.exact_prompt_required,
        durable_user_message,
        capability_descriptors,
        capability_digest,
        dispatch_run_id,
        promoted_at_ms,
    };
    promotion
        .validate_for_session(session.session_scope_id())
        .map_err(|error| format!("queued promotion candidate is invalid: {error:#}"))?;

    let mut exact_user_message = ModelMessage::user(exact_prompt.clone());
    exact_user_message.id = durable_message_id;
    let background_ready_context = queued_background_ready_transient_context(Some(session));
    let mut transient_messages = vec![exact_user_message];
    transient_messages.extend(background_ready_context.clone());
    let runtime_context = resolve_runtime_context(&exact_prompt);
    let request = session
        .build_pre_turn_candidate_request(
            workspace_root,
            memory_config,
            tools,
            target_max_tokens,
            queued.reasoning_effort.clone().or(default_reasoning_effort),
            session.latest_response_handle(session.provider_name()),
            traffic_partition_key,
            &transient_messages,
            runtime_context.clone(),
            &[],
        )
        .map_err(|error| format!("failed to build queued pre-turn candidate: {error:#}"))?;
    let frozen_request =
        sigil_kernel::FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request)
            .map_err(|error| format!("failed to freeze queued pre-turn candidate: {error:#}"))?;

    Ok(QueuedConversationCandidatePreparation::Prepared(Box::new(
        PreparedQueuedConversationCandidate {
            promotion,
            frozen_request,
            reasoning_effort: queued.reasoning_effort,
            background_ready_context,
            runtime_context,
            capability_registrations,
        },
    )))
}

/// Prepares a local, proof-carrying exact-fit decision for the next queued conversation input.
///
/// Only the default DeepSeek V4 Flash profile is currently admitted because it is the only
/// provider/model pair with a versioned exact tokenizer and explicit portable target budget.
/// Missing local proof or an unsupported profile returns a no-write block. This function does not
/// start compaction; the portable-preflight branch owns that separately.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(in crate::runner) fn prepare_next_queued_conversation_pressure_admission(
    session: &Session,
    exact_prompts: &ExactConversationPromptStore,
    workspace_root: &Path,
    memory_config: &MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    default_reasoning_effort: Option<ReasoningEffort>,
    traffic_partition_key: Option<String>,
    cache_root: &Path,
) -> std::result::Result<QueuedConversationPressureAdmission, String> {
    prepare_next_queued_conversation_pressure_admission_with_context(
        session,
        exact_prompts,
        workspace_root,
        memory_config,
        tools,
        default_reasoning_effort,
        traffic_partition_key,
        cache_root,
        |query| {
            sigil_runtime::context_candidates_from_safe_sources(workspace_root, query, None)
                .unwrap_or_default()
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn prepare_next_queued_conversation_pressure_admission_with_resolver(
    session: &Session,
    exact_prompts: &ExactConversationPromptStore,
    workspace_root: &Path,
    memory_config: &MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    default_reasoning_effort: Option<ReasoningEffort>,
    traffic_partition_key: Option<String>,
    cache_root: &Path,
    context_resolver: &sigil_runtime::RequestContextResolver,
    runtime_handle: &tokio::runtime::Handle,
) -> std::result::Result<QueuedConversationPressureAdmission, String> {
    prepare_next_queued_conversation_pressure_admission_with_context(
        session,
        exact_prompts,
        workspace_root,
        memory_config,
        tools,
        default_reasoning_effort,
        traffic_partition_key,
        cache_root,
        |query| {
            runtime_handle
                .block_on(context_resolver.resolve(query))
                .unwrap_or_default()
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_next_queued_conversation_pressure_admission_with_context<F>(
    session: &Session,
    exact_prompts: &ExactConversationPromptStore,
    workspace_root: &Path,
    memory_config: &MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    default_reasoning_effort: Option<ReasoningEffort>,
    traffic_partition_key: Option<String>,
    cache_root: &Path,
    resolve_runtime_context: F,
) -> std::result::Result<QueuedConversationPressureAdmission, String>
where
    F: FnOnce(&str) -> RuntimeContextCandidates,
{
    let profile_admitted = sigil_runtime::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    );
    let target_max_tokens = profile_admitted
        .then_some(sigil_runtime::deepseek_v4_flash_portable_target_output_tokens());
    let preparation = prepare_next_queued_conversation_candidate_with_target_max_tokens(
        session,
        exact_prompts,
        workspace_root,
        memory_config,
        tools,
        target_max_tokens,
        default_reasoning_effort,
        traffic_partition_key,
        resolve_runtime_context,
    )?;
    let candidate = match preparation {
        QueuedConversationCandidatePreparation::NoQueuedInput => {
            return Ok(QueuedConversationPressureAdmission::NoQueuedInput);
        }
        QueuedConversationCandidatePreparation::Blocked { queue_id, reason } => {
            return Ok(QueuedConversationPressureAdmission::Blocked {
                queue_id,
                reason,
                candidate: None,
            });
        }
        QueuedConversationCandidatePreparation::Prepared(candidate) => candidate,
    };
    if !profile_admitted {
        return Ok(QueuedConversationPressureAdmission::Blocked {
            queue_id: candidate.promotion.queue_id.clone(),
            reason: "queued pre-turn exact admission is unavailable for this provider/model"
                .to_owned(),
            candidate: Some(candidate),
        });
    }

    let pressure = match sigil_runtime::deepseek_v4_flash_portable_target_pressure(
        cache_root,
        &candidate.frozen_request,
    ) {
        Ok(pressure) => pressure,
        Err(_) => {
            return Ok(QueuedConversationPressureAdmission::Blocked {
                queue_id: candidate.promotion.queue_id.clone(),
                reason:
                    "queued pre-turn exact admission is unavailable from the local token profile"
                        .to_owned(),
                candidate: Some(candidate),
            });
        }
    };
    let (token_binding, request_fit) = match pressure {
        sigil_runtime::DeepSeekV4FlashPortableTargetPressure::ExactFit { binding, proof } => {
            (binding, *proof)
        }
        sigil_runtime::DeepSeekV4FlashPortableTargetPressure::ExceedsBudget {
            input_tokens,
            budget,
        } => {
            return Ok(
                QueuedConversationPressureAdmission::PortablePreflightRequired {
                    candidate,
                    input_tokens,
                    budget,
                },
            );
        }
    };
    request_fit
        .validate_for(
            candidate.frozen_request.fingerprint(),
            sigil_kernel::TokenMeasurementScope::RenderedTargetInput,
            &token_binding,
        )
        .map_err(|error| format!("queued pre-turn exact proof validation failed: {error:#}"))?;

    Ok(QueuedConversationPressureAdmission::ExactFit(Box::new(
        AdmittedQueuedConversationCandidate {
            candidate: *candidate,
            token_binding,
            request_fit,
        },
    )))
}

pub(in crate::runner) fn queue_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    prompt: String,
    kind: ConversationInputKind,
    target: ConversationInputTarget,
    reasoning_effort: ReasoningEffort,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = match current_session.as_ref() {
        Some(session) => session.entries().to_vec(),
        None => JsonlSessionStore::read_entries(session_log_path)
            .map_err(|error| format!("failed to load session for follow-up: {error:#}"))?,
    };
    let active_queue_count = ConversationQueueProjection::from_entries(&entries)
        .items
        .len();
    if active_queue_count >= MAX_EXACT_CONVERSATION_PROMPTS
        || exact_prompts.len() >= MAX_EXACT_CONVERSATION_PROMPTS
    {
        return Err("conversation input queue capacity is exhausted".to_owned());
    }
    let queue_id = next_conversation_queue_id(&entries)?;
    let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
    let prompt_hash = durable_conversation_prompt_hash(&prompt, &safe_prompt);
    let entry = ConversationInputQueuedEntry {
        queue_id: queue_id.clone(),
        target,
        kind,
        prompt_hash,
        prompt: safe_prompt,
        reasoning_effort: Some(reasoning_effort),
        created_at_ms: Some(current_unix_time_ms()),
    };
    let control = ControlEntry::ConversationInputQueued(entry);
    if let Some(session) = current_session.as_mut() {
        session
            .append_control(control)
            .map_err(|error| format!("failed to append follow-up: {error:#}"))?;
        exact_prompts.insert(queue_id, SecretString::new(prompt));
        Ok(session.entries().to_vec())
    } else {
        let store = JsonlSessionStore::new(session_log_path.to_path_buf())
            .map_err(|error| format!("failed to open session store for follow-up: {error:#}"))?;
        store
            .append(&SessionLogEntry::Control(control))
            .map_err(|error| format!("failed to persist follow-up: {error:#}"))?;
        exact_prompts.insert(queue_id, SecretString::new(prompt));
        JsonlSessionStore::read_entries(session_log_path)
            .map_err(|error| format!("failed to reload follow-up: {error:#}"))
    }
}

pub(in crate::runner) fn cancel_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    queue_id: ConversationInputQueueId,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    ensure_queued_conversation_item_is_mutable(session_log_path, current_session, &queue_id)?;
    let entries = append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputStatusChanged(
            ConversationInputStatusEntry {
                queue_id: queue_id.clone(),
                status: ConversationInputStatus::Cancelled,
                reason: Some("cancelled by user".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )?;
    exact_prompts.remove(&queue_id);
    Ok(entries)
}

pub(in crate::runner) fn edit_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    exact_prompts: &mut ExactConversationPromptStore,
    queue_id: ConversationInputQueueId,
    prompt: String,
    reasoning_effort: ReasoningEffort,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    if prompt.trim().is_empty() {
        return Err("follow-up prompt cannot be empty".to_owned());
    }
    ensure_queued_conversation_item_is_mutable(session_log_path, current_session, &queue_id)?;
    let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
    let prompt_hash = durable_conversation_prompt_hash(&prompt, &safe_prompt);
    let entries = append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputEdited(
            ConversationInputEditedEntry {
                queue_id: queue_id.clone(),
                prompt_hash,
                prompt: safe_prompt,
                reasoning_effort: Some(reasoning_effort),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )?;
    exact_prompts.insert(queue_id, SecretString::new(prompt));
    Ok(entries)
}

pub(in crate::runner) fn move_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
    direction: QueueMoveDirection,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, &queue_id)?;
    let Some(index) = projection
        .items
        .iter()
        .position(|item| item.queued.queue_id == queue_id)
    else {
        return Err(format!("follow-up {} not found", queue_id.as_str()));
    };
    let after_queue_id = match direction {
        QueueMoveDirection::Up if index == 0 => return Ok(entries),
        QueueMoveDirection::Up if index == 1 => None,
        QueueMoveDirection::Up => Some(projection.items[index - 2].queued.queue_id.clone()),
        QueueMoveDirection::Down if index + 1 >= projection.items.len() => return Ok(entries),
        QueueMoveDirection::Down => Some(projection.items[index + 1].queued.queue_id.clone()),
    };
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputReordered(
            ConversationInputReorderedEntry {
                queue_id,
                after_queue_id,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

pub(in crate::runner) fn promote_queued_conversation_input(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    queue_id: ConversationInputQueueId,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, &queue_id)?;
    let mut controls = Vec::new();
    if projection.paused {
        controls.push(ControlEntry::ConversationInputQueueControl(
            ConversationInputQueueControlEntry {
                action: ConversationInputQueueControlAction::Resume,
                reason: Some("next turn".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        ));
    }
    controls.push(ControlEntry::ConversationInputReordered(
        ConversationInputReorderedEntry {
            queue_id,
            after_queue_id: None,
            updated_at_ms: Some(current_unix_time_ms()),
        },
    ));
    append_conversation_queue_control_entries(session_log_path, current_session, controls)
}

pub(in crate::runner) fn set_conversation_queue_paused(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    paused: bool,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    append_conversation_queue_control_entries(
        session_log_path,
        current_session,
        vec![ControlEntry::ConversationInputQueueControl(
            ConversationInputQueueControlEntry {
                action: if paused {
                    ConversationInputQueueControlAction::Pause
                } else {
                    ConversationInputQueueControlAction::Resume
                },
                reason: Some("user control".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            },
        )],
    )
}

pub(in crate::runner) fn ensure_queued_conversation_item_is_mutable(
    session_log_path: &Path,
    current_session: &Option<Session>,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<(), String> {
    let entries = read_conversation_queue_entries(session_log_path, current_session)?;
    let projection = ConversationQueueProjection::from_entries(&entries);
    ensure_projection_item_is_mutable(&projection, queue_id)
}

pub(in crate::runner) fn ensure_projection_item_is_mutable(
    projection: &ConversationQueueProjection,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<(), String> {
    let Some(item) = projection
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
    else {
        return Err(format!("follow-up {} not found", queue_id.as_str()));
    };
    if item.status != ConversationInputStatus::Queued {
        return Err(format!(
            "follow-up {} is already {}",
            queue_id.as_str(),
            queue_status_label(item.status)
        ));
    }
    Ok(())
}

pub(in crate::runner) fn append_conversation_queue_control_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    controls: Vec<ControlEntry>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    append_session_control_entries(
        session_log_path,
        current_session,
        controls,
        "conversation queue",
    )
    .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) fn append_agent_result_continuation_status_entries(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    thread_ids: &[AgentThreadId],
    status: AgentResultContinuationStatus,
    reason: Option<&str>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    let controls = thread_ids
        .iter()
        .cloned()
        .map(|thread_id| {
            ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
                thread_id,
                status,
                reason: reason.map(str::to_owned),
                updated_at_ms: Some(current_unix_time_ms()),
            })
        })
        .collect::<Vec<_>>();
    append_session_control_entries(
        session_log_path,
        current_session,
        controls,
        "agent result continuation",
    )
    .map_err(|error| format!("{error:#}"))
}

pub(in crate::runner) fn append_agent_result_continuation_status_and_notify(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    thread_ids: &[AgentThreadId],
    status: AgentResultContinuationStatus,
    reason: Option<&str>,
) {
    let Some(session) = current_session.as_mut() else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "agent result continuation status skipped: session state unavailable".to_owned(),
        ));
        return;
    };
    for thread_id in thread_ids {
        let entry = AgentResultContinuationEntry {
            thread_id: thread_id.clone(),
            status,
            reason: reason.map(str::to_owned),
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) = session.append_control(ControlEntry::AgentResultContinuation(entry)) {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "agent result continuation status append failed: {error:#}"
            )));
            return;
        }
    }
}

pub(in crate::runner) fn read_conversation_queue_entries(
    session_log_path: &Path,
    current_session: &Option<Session>,
) -> std::result::Result<Vec<SessionLogEntry>, String> {
    if let Some(session) = current_session.as_ref() {
        return Ok(session.entries().to_vec());
    }
    JsonlSessionStore::read_entries(session_log_path)
        .map_err(|error| format!("failed to read conversation queue state: {error:#}"))
}

pub(in crate::runner) fn next_conversation_queue_id(
    entries: &[SessionLogEntry],
) -> std::result::Result<ConversationInputQueueId, String> {
    let existing = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued)) => {
                Some(queued.queue_id.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for index in 1..=existing.len().saturating_add(1024) {
        let candidate = format!("queue_{index}");
        if !existing.contains(candidate.as_str()) {
            return ConversationInputQueueId::new(candidate)
                .map_err(|error| format!("failed to allocate queue id: {error:#}"));
        }
    }
    Err("failed to allocate queue id".to_owned())
}

pub(in crate::runner) fn conversation_prompt_hash(prompt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn durable_conversation_prompt_hash(raw_prompt: &str, safe_prompt: &str) -> String {
    let safe_hash = conversation_prompt_hash(safe_prompt);
    if raw_prompt == safe_prompt {
        format!("safe:{safe_hash}")
    } else {
        format!("{EXACT_PROMPT_REQUIRED_HASH_PREFIX}{safe_hash}")
    }
}

pub(in crate::runner) fn queue_status_label(status: ConversationInputStatus) -> &'static str {
    match status {
        ConversationInputStatus::Queued => "queued",
        ConversationInputStatus::Dispatching => "dispatching",
        ConversationInputStatus::Delivered => "delivered",
        ConversationInputStatus::Rejected => "rejected",
        ConversationInputStatus::Cancelled => "cancelled",
        ConversationInputStatus::Stale => "stale",
        ConversationInputStatus::Unknown => "unknown",
    }
}

pub(in crate::runner) fn send_conversation_queue_update(
    message_tx: &mpsc::Sender<WorkerMessage>,
    entries: &[SessionLogEntry],
) {
    let projection = sigil_kernel::ConversationQueueProjection::from_entries(entries);
    let _ = message_tx.send(WorkerMessage::ConversationQueueUpdated {
        items: projection.items,
        paused: projection.paused,
        entries: entries.to_vec(),
    });
}

pub(in crate::runner) fn mark_stale_dispatching_conversation_queue_items(
    session: &mut Session,
    exact_prompts: &ExactConversationPromptStore,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    let physical_attempts = match session.provider_physical_attempt_projection() {
        Ok(projection) => projection,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue recovery skipped: provider attempt evidence is unavailable: {error:#}"
            )));
            return;
        }
    };
    let stale_queue_items = session
        .conversation_queue_projection()
        .items
        .into_iter()
        .filter_map(|item| {
            let missing_exact = item.status == ConversationInputStatus::Queued
                && item
                    .queued
                    .prompt_hash
                    .starts_with(EXACT_PROMPT_REQUIRED_HASH_PREFIX)
                && !exact_prompts.contains_key(&item.queued.queue_id);
            if missing_exact {
                return Some((
                    item.queued.queue_id,
                    QueuedConversationTerminalClassification::Stale {
                        reason: "exact sensitive follow-up was lost after restart".to_owned(),
                    },
                ));
            }
            (item.status == ConversationInputStatus::Dispatching).then(|| {
                (
                    item.queued.queue_id.clone(),
                    classify_promoted_queued_conversation(
                        session,
                        &physical_attempts,
                        &item.queued.queue_id,
                    )
                    .unwrap_or_else(|reason| {
                        QueuedConversationTerminalClassification::Stale {
                            reason: format!(
                                "session restore cannot establish the queued provider outcome: {reason}"
                            ),
                        }
                    }),
                )
            })
        })
        .collect::<Vec<_>>();
    if stale_queue_items.is_empty() {
        return;
    }

    let mut changed = false;
    for (queue_id, classification) in stale_queue_items {
        let (status, reason) = match classification {
            QueuedConversationTerminalClassification::Delivered { reason } => {
                (ConversationInputStatus::Delivered, reason)
            }
            QueuedConversationTerminalClassification::Rejected { reason } => {
                (ConversationInputStatus::Rejected, Some(reason))
            }
            QueuedConversationTerminalClassification::Stale { reason } => {
                (ConversationInputStatus::Stale, Some(reason))
            }
        };
        let status = ConversationInputStatusEntry {
            queue_id,
            status,
            reason,
            updated_at_ms: Some(current_unix_time_ms()),
        };
        if let Err(error) =
            session.append_control(ControlEntry::ConversationInputStatusChanged(status))
        {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue restore skipped: {error:#}"
            )));
            break;
        }
        changed = true;
    }

    if changed {
        send_conversation_queue_update(message_tx, session.entries());
    }
}

pub(in crate::runner) fn classify_promoted_queued_conversation(
    session: &Session,
    physical_attempts: &sigil_kernel::ProviderPhysicalAttemptProjection,
    queue_id: &ConversationInputQueueId,
) -> std::result::Result<QueuedConversationTerminalClassification, String> {
    let promotion = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promoted))
                if promoted.queue_id == *queue_id =>
            {
                Some(promoted)
            }
            _ => None,
        })
        .ok_or_else(|| {
            format!(
                "queued input {} was dispatching without a durable promotion chain",
                queue_id.as_str()
            )
        })?;
    let attempts = physical_attempts.attempts_for_logical_run_id(&promotion.dispatch_run_id);
    let [attempt] = attempts.as_slice() else {
        return Ok(match attempts.len() {
            0 => QueuedConversationTerminalClassification::Rejected {
                reason: "queued promotion was not followed by a provider physical attempt"
                    .to_owned(),
            },
            _ => QueuedConversationTerminalClassification::Stale {
                reason: "queued promotion has multiple provider physical attempts".to_owned(),
            },
        });
    };
    let Some(terminal) = attempt.terminal.as_ref() else {
        return Ok(QueuedConversationTerminalClassification::Stale {
            reason: "queued provider physical attempt has no durable terminal".to_owned(),
        });
    };
    Ok(match terminal.outcome {
        sigil_kernel::ProviderPhysicalAttemptOutcome::Completed
        | sigil_kernel::ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect
        | sigil_kernel::ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput => {
            QueuedConversationTerminalClassification::Delivered { reason: None }
        }
        sigil_kernel::ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption => {
            QueuedConversationTerminalClassification::Rejected {
                reason: "queued provider attempt confirmed no model consumption".to_owned(),
            }
        }
        sigil_kernel::ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain
        | sigil_kernel::ProviderPhysicalAttemptOutcome::Interrupted => {
            QueuedConversationTerminalClassification::Stale {
                reason:
                    "queued provider outcome is uncertain and will not be replayed automatically"
                        .to_owned(),
            }
        }
    })
}

pub(in crate::runner) fn append_queue_status_and_notify(
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    queue_id: ConversationInputQueueId,
    status: ConversationInputStatus,
    reason: Option<String>,
) {
    let Some(session) = current_session.as_mut() else {
        let _ = message_tx.send(WorkerMessage::Notice(
            "conversation queue status skipped: session state unavailable".to_owned(),
        ));
        return;
    };
    let entry = ConversationInputStatusEntry {
        queue_id,
        status,
        reason,
        updated_at_ms: Some(current_unix_time_ms()),
    };
    if let Err(error) = session.append_control(ControlEntry::ConversationInputStatusChanged(entry))
    {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "conversation queue status append failed: {error:#}"
        )));
        return;
    }
    send_conversation_queue_update(message_tx, session.entries());
}

pub(in crate::runner) fn append_queue_failure_and_pause_and_notify(
    session_log_path: &Path,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    queue_id: ConversationInputQueueId,
    reason: String,
) {
    let controls = vec![
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Rejected,
            reason: Some(reason),
            updated_at_ms: Some(current_unix_time_ms()),
        }),
        ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
            action: ConversationInputQueueControlAction::Pause,
            reason: Some("queued run failed".to_owned()),
            updated_at_ms: Some(current_unix_time_ms()),
        }),
    ];
    match append_conversation_queue_control_entries(session_log_path, current_session, controls) {
        Ok(entries) => send_conversation_queue_update(message_tx, &entries),
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "conversation queue failure handling skipped: {error}"
            )));
        }
    }
}

/// Commits the durable queue-promotion barrier and its safe user-message/capability material.
///
/// The exact request remains only in `candidate.frozen_request`; this function records only the
/// prevalidated safe projection. A provider call is deliberately outside this boundary and must
/// consume the returned frozen material without rebuilding it.
pub(in crate::runner) fn commit_prepared_queued_conversation_candidate(
    session_log_path: &Path,
    session: &mut Session,
    candidate: PreparedQueuedConversationCandidate,
) -> std::result::Result<PreparedQueuedConversationCandidate, String> {
    candidate
        .promotion
        .validate_for_session(session.session_scope_id())
        .map_err(|error| format!("queued promotion candidate is invalid: {error:#}"))?;
    let durable_message_id = candidate.promotion.durable_user_message.id.clone();
    let registrar = session.user_url_capability_registrar();
    if let Some(registrar) = registrar.as_ref() {
        for registration in &candidate.capability_registrations {
            if let Err(error) = registrar.stage(registration.clone()) {
                let _ = registrar.rollback_message(&durable_message_id);
                return Err(format!(
                    "failed to stage queued URL capability material before promotion: {error:#}"
                ));
            }
        }
    }

    let store = JsonlSessionStore::new(session_log_path)
        .map_err(|error| format!("failed to open queued promotion store: {error:#}"))?;
    if let Err(error) = store.append_conversation_input_promoted(candidate.promotion.clone()) {
        if let Some(registrar) = registrar.as_ref() {
            let _ = registrar.rollback_message(&durable_message_id);
        }
        return Err(format!(
            "queued promotion compare-and-swap refused: {error:#}"
        ));
    }

    if let Err(error) =
        session.append_user_message(candidate.promotion.durable_user_message.clone())
    {
        if let Some(registrar) = registrar.as_ref() {
            let _ = registrar.rollback_message(&durable_message_id);
        }
        return Err(format!(
            "queued promotion was recorded but its safe user message could not be appended: {error:#}"
        ));
    }
    for descriptor in &candidate.promotion.capability_descriptors {
        if let Err(error) =
            session.append_control(ControlEntry::WebUrlCapabilityDescriptor(descriptor.clone()))
        {
            if let Some(registrar) = registrar.as_ref() {
                let _ = registrar.rollback_message(&durable_message_id);
            }
            return Err(format!(
                "queued promotion was recorded but its URL capability descriptor could not be appended: {error:#}"
            ));
        }
    }
    if let Some(registrar) = registrar.as_ref()
        && let Err(error) = registrar.commit_message(&durable_message_id)
    {
        let rollback_error = registrar.rollback_message(&durable_message_id).err();
        return Err(match rollback_error {
            Some(rollback_error) => format!(
                "queued promotion was recorded but URL capabilities could not be committed: {error:#}; rollback also failed: {rollback_error:#}"
            ),
            None => format!(
                "queued promotion was recorded but URL capabilities could not be committed: {error:#}"
            ),
        });
    }
    Ok(candidate)
}

#[cfg(test)]
#[path = "../tests/queue_driver_tests.rs"]
mod tests;
