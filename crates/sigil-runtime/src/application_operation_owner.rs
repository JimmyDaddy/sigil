//! Application K/F translation and reconciliation against the session domain owner.

use sigil_application::{
    ApplicationCommand, ApplicationCommandRequest, ApplicationDomainCommitRef,
    ApplicationDomainReceipt, ApplicationError, ApplicationFrontier, ApplicationQueueAction,
    ApplicationQueueItemKind, ApplicationQueueTarget, PlanTaskCommand, UserInputCommand,
};
use sigil_kernel::{
    ApplicationOperationBindingV1, ApplicationOperationTargetV1, ConversationInputKind,
    ConversationInputTarget, SessionRecordReadHandle,
};

/// A domain append was attempted and its durable effect must be reconciled by the same owner.
/// Validation failures before this boundary remain distinguishable from uncertain publication.
#[derive(Debug, thiserror::Error)]
#[error("application domain publication requires reconciliation: {0}")]
pub struct ApplicationPublicationError(#[source] pub anyhow::Error);

pub fn application_operation_binding(
    request: &ApplicationCommandRequest,
) -> Result<Option<ApplicationOperationBindingV1>, ApplicationError> {
    use ApplicationOperationTargetV1 as Target;
    request.validate()?;
    let target = match &request.envelope.command {
        ApplicationCommand::Conversation(
            sigil_application::ConversationCommand::SubmitPrompt {
                prompt: Some(prompt),
                options,
            },
        ) if foreground_run_options(options.as_deref()) => Target::ConversationRunAdmission {
            input_digest: sigil_kernel::conversation_run_input_digest(prompt.as_str(), &[])
                .map_err(unavailable)?,
        },
        ApplicationCommand::Conversation(
            sigil_application::ConversationCommand::SubmitPromptWithAttachments {
                prompt,
                attachments,
                options,
            },
        ) if !attachments.is_empty() && foreground_run_options(options.as_deref()) => {
            Target::ConversationRunAdmission {
                input_digest: sigil_kernel::conversation_run_input_digest(
                    prompt.as_ref().map_or("", |text| text.as_str()),
                    attachments,
                )
                .map_err(unavailable)?,
            }
        }
        ApplicationCommand::Agent(sigil_application::AgentCommand::InvokeInlineSkill {
            arguments,
            attachments,
            ..
        }) => Target::ConversationRunAdmission {
            // Skill identity and invocation options remain in the complete command F.
            // Bind only the original input here, before the host adds skill context.
            input_digest: sigil_kernel::conversation_run_input_digest(
                arguments.as_ref().map_or("", |text| text.as_str()),
                attachments,
            )
            .map_err(unavailable)?,
        },
        ApplicationCommand::PlanTask(
            PlanTaskCommand::SubmitTask { prompt }
            | PlanTaskCommand::SubmitPlanPrompt { prompt, .. },
        ) => Target::ConversationRunAdmission {
            input_digest: sigil_kernel::conversation_run_input_digest(prompt.as_str(), &[])
                .map_err(unavailable)?,
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::ContinueTask { task_id, guidance }) => {
            Target::ConversationRunAdmission {
                input_digest: task_continuation_input_digest(
                    task_id.as_ref().map(|id| id.as_str()),
                    guidance.as_ref().map(|text| text.as_str()),
                )
                .map_err(unavailable)?,
            }
        }
        ApplicationCommand::Conversation(
            sigil_application::ConversationCommand::SubmitPrompt {
                prompt: None,
                options: Some(options),
            },
        ) if options.task_continuation.is_some() => {
            let task = options
                .task_continuation
                .as_ref()
                .ok_or(ApplicationError::Unavailable)?;
            Target::ConversationRunAdmission {
                input_digest: task_continuation_input_digest(
                    Some(task.task_id.as_str()),
                    task.guidance.as_ref().map(|text| text.as_str()),
                )
                .map_err(unavailable)?,
            }
        }
        ApplicationCommand::Agent(sigil_application::AgentCommand::InvokeProfile {
            profile_id,
            prompt,
            ..
        }) => Target::AgentInvocation {
            profile_id: profile_id.as_str().to_owned(),
            prompt_hash: sigil_kernel::sha256_hex(
                sigil_kernel::safe_persistence_text(prompt.as_str()).as_bytes(),
            ),
        },
        ApplicationCommand::Agent(sigil_application::AgentCommand::InvokeChildSessionSkill {
            skill_id,
            arguments,
        }) => Target::DirectTaskAdmission {
            objective_hash:
                sigil_kernel::direct_task_execution::task_direct_execution_objective_hash(
                    &skill_child_task_objective(skill_id.as_str(), arguments.as_str()),
                ),
        },
        ApplicationCommand::Conversation(sigil_application::ConversationCommand::Recovery {
            action:
                sigil_application::ApplicationRecoveryAction::ForkConversation {
                    source_turn_digest,
                    connection_id,
                    model_id,
                    ..
                },
        }) => Target::ForkConversation {
            source_turn_digest: source_turn_digest.as_str().to_owned(),
            connection_id: connection_id.as_str().to_owned(),
            model_id: model_id.as_str().to_owned(),
        },
        ApplicationCommand::Conversation(sigil_application::ConversationCommand::Recovery {
            action:
                sigil_application::ApplicationRecoveryAction::ReviewPlugin {
                    plugin_id,
                    manifest_hash,
                    capability_digest,
                    enabled,
                },
        }) => Target::ReviewPlugin {
            plugin_id: plugin_id.as_str().to_owned(),
            manifest_hash: manifest_hash.as_str().to_owned(),
            capability_digest: capability_digest.as_str().to_owned(),
            decision: if *enabled {
                sigil_kernel::PluginTrustDecision::Trusted
            } else {
                sigil_kernel::PluginTrustDecision::Disabled
            },
        },
        ApplicationCommand::Conversation(sigil_application::ConversationCommand::Recovery {
            action:
                sigil_application::ApplicationRecoveryAction::ImportBranchKnowledge {
                    source_session_id,
                    source_turn_digest,
                    source_message_id,
                    source_text_sha256,
                    summary_sha256,
                    ..
                },
        }) => Target::ImportBranchKnowledge {
            source_session_id: source_session_id.as_str().to_owned(),
            source_turn_digest: source_turn_digest.as_str().to_owned(),
            source_message_id: source_message_id.as_str().to_owned(),
            source_text_sha256: source_text_sha256.as_str().to_owned(),
            summary_sha256: summary_sha256.as_str().to_owned(),
        },
        ApplicationCommand::Conversation(sigil_application::ConversationCommand::Queue {
            action,
            ..
        }) => match action {
            ApplicationQueueAction::Enqueue {
                target,
                prompt,
                kind,
                ..
            } => Target::QueueEnqueue {
                prompt_hash: queue_prompt_hash(prompt.as_str()),
                target: queue_target(target)?,
                input_kind: match kind {
                    ApplicationQueueItemKind::Chat => ConversationInputKind::Chat,
                    ApplicationQueueItemKind::PlanPrompt => ConversationInputKind::PlanPrompt,
                    ApplicationQueueItemKind::AgentMention => ConversationInputKind::AgentMention,
                    ApplicationQueueItemKind::AgentMessage => ConversationInputKind::AgentMessage,
                    ApplicationQueueItemKind::TaskGuidance => ConversationInputKind::TaskGuidance,
                    ApplicationQueueItemKind::Unknown => ConversationInputKind::Unknown,
                },
            },
            ApplicationQueueAction::Edit {
                entry_id, prompt, ..
            } => Target::QueueEdit {
                queue_id: entry_id.as_str().to_owned(),
                prompt_hash: queue_prompt_hash(prompt.as_str()),
            },
            ApplicationQueueAction::Remove { entry_id, .. } => Target::QueueCancel {
                queue_id: entry_id.as_str().to_owned(),
            },
            ApplicationQueueAction::Move { entry_id, .. }
            | ApplicationQueueAction::Promote { entry_id, .. }
            | ApplicationQueueAction::Reorder { entry_id, .. } => Target::QueueReorder {
                queue_id: entry_id.as_str().to_owned(),
            },
            ApplicationQueueAction::Pause => Target::QueuePause { paused: true },
            ApplicationQueueAction::Resume => Target::QueuePause { paused: false },
            _ => return Ok(None),
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::SavePlan {
            plan_id,
            expected_plan_hash,
        }) => Target::PlanDecision {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
            decision: sigil_kernel::PlanDecision::SavedOnly,
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::RejectPlan {
            plan_id,
            expected_plan_hash,
        }) => Target::PlanDecision {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
            decision: sigil_kernel::PlanDecision::Rejected,
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash,
            ..
        }) => Target::PlanAdoption {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::AdoptPlanCandidate {
            plan_id,
            expected_candidate_hash,
        }) => Target::PlanCandidateAdoption {
            plan_id: plan_id.as_str().to_owned(),
            candidate_hash: expected_candidate_hash.as_str().to_owned(),
        },
        ApplicationCommand::PlanTask(PlanTaskCommand::RevisePlan {
            plan_id,
            expected_plan_hash,
        }) => Target::PlanRevisionGuidance {
            plan_id: plan_id.as_str().to_owned(),
            plan_hash: expected_plan_hash.as_str().to_owned(),
        },
        ApplicationCommand::UserInput(UserInputCommand::ResumeCommittedUserInput {
            binding,
            generation,
            expected_request_hash,
            ..
        }) => {
            let original =
                resumed_user_input_operation(request)?.ok_or(ApplicationError::ScopeMismatch)?;
            Target::UserInputContinuation {
                original_operation_id: original.operation_id,
                request_id: binding.clone(),
                generation: *generation,
                request_hash: expected_request_hash.as_str().to_owned(),
            }
        }
        ApplicationCommand::UserInput(UserInputCommand::Resolve {
            binding,
            generation,
            expected_request_hash,
            ..
        }) => Target::UserInputDecision {
            request_id: binding.clone(),
            generation: *generation,
            request_hash: expected_request_hash.as_str().to_owned(),
            command_id: request.envelope.command_id.as_str().to_owned(),
        },
        _ => return Ok(None),
    };
    let key = request
        .admission
        .reservation_key(&request.envelope.command_id);
    let key_digest = application_reservation_key_digest(&key)?;
    let scope = request
        .admission
        .scope
        .session
        .as_ref()
        .ok_or(ApplicationError::ScopeRequired)?
        .as_str()
        .to_owned();
    ApplicationOperationBindingV1::new(
        scope,
        key_digest,
        sigil_application::command_fingerprint(request)?,
        target,
    )
    .map(Some)
    .map_err(unavailable)
}

fn foreground_run_options(options: Option<&sigil_application::RunStartOptions>) -> bool {
    // An inline skill uses the same prepared foreground run and exact K/F. Its fresh
    // content/trust/tool-scope validation still happens during normal preparation.
    options.is_none_or(|options| options.task_continuation.is_none())
}

/// Binds a continuation's original selected Task and guidance before host context is added.
/// The digest records input identity; existing Task resolution remains the execution authority.
///
/// # Errors
/// Returns an error if the structured original input cannot be serialized.
pub fn task_continuation_input_digest(
    task_id: Option<&str>,
    guidance: Option<&str>,
) -> anyhow::Result<String> {
    let value = sigil_kernel::canonicalize_cache_stable_json(
        &serde_json::json!({"task_id": task_id, "guidance": guidance}),
    )?;
    Ok(sigil_kernel::sha256_hex(&serde_json::to_vec(&value)?))
}

/// Exact objective consumed by the existing child-skill Task owner and its admission receipt.
/// This only formats an already-typed invocation; it does not select a route or grant permission.
pub fn skill_child_task_objective(skill_id: &str, arguments: &str) -> String {
    let trimmed = arguments.trim();
    let summary = if trimmed.is_empty() {
        format!("invoke agent {skill_id}")
    } else {
        format!("invoke agent {skill_id} with arguments: {trimmed}")
    };
    format!("{summary}\n\nInvoke skill {skill_id} with arguments: {arguments}")
}

/// Canonical identity shared by original dispatch and typed continuation validation.
pub fn application_reservation_key_digest(
    key: &sigil_application::CommandReservationKey,
) -> Result<String, ApplicationError> {
    key.validate()?;
    let value = serde_json::to_value(key).map_err(unavailable)?;
    let canonical = sigil_kernel::canonicalize_cache_stable_json(&value).map_err(unavailable)?;
    Ok(sigil_kernel::sha256_hex(
        &serde_json::to_vec(&canonical).map_err(unavailable)?,
    ))
}

pub fn resumed_user_input_operation(
    request: &ApplicationCommandRequest,
) -> Result<Option<ApplicationOperationBindingV1>, ApplicationError> {
    let ApplicationCommand::UserInput(UserInputCommand::ResumeCommittedUserInput {
        original_key,
        original_fingerprint,
        original_operation_id,
        original_domain_session_scope_id,
        binding,
        generation,
        expected_request_hash,
    }) = &request.envelope.command
    else {
        return Ok(None);
    };
    if original_key.authority_scope != request.admission.scope
        || original_key.principal != request.admission.principal
        || original_key.command_id == request.envelope.command_id
    {
        return Err(ApplicationError::ScopeMismatch);
    }
    let mut operation = ApplicationOperationBindingV1::new(
        original_key
            .authority_scope
            .session
            .as_ref()
            .ok_or(ApplicationError::ScopeRequired)?
            .as_str()
            .to_owned(),
        application_reservation_key_digest(original_key)?,
        original_fingerprint.clone(),
        ApplicationOperationTargetV1::UserInputDecision {
            request_id: binding.clone(),
            generation: *generation,
            request_hash: expected_request_hash.as_str().to_owned(),
            command_id: original_key.command_id.as_str().to_owned(),
        },
    )
    .map_err(unavailable)?;
    operation.domain_session_scope_id = original_domain_session_scope_id.clone();
    operation.validate().map_err(unavailable)?;
    if &operation.operation_id != original_operation_id {
        return Err(ApplicationError::ScopeMismatch);
    }
    Ok(Some(operation))
}

fn unavailable(_: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::Unavailable
}

fn queue_prompt_hash(raw: &str) -> String {
    sigil_kernel::project_conversation_prompt_for_persistence(raw).prompt_hash
}

fn queue_target(
    target: &ApplicationQueueTarget,
) -> Result<ConversationInputTarget, ApplicationError> {
    Ok(match target {
        ApplicationQueueTarget::MainThread => ConversationInputTarget::MainThread,
        ApplicationQueueTarget::AgentThread { thread_id } => ConversationInputTarget::AgentThread {
            thread_id: sigil_kernel::AgentThreadId::new(thread_id.as_str()).map_err(unavailable)?,
        },
        ApplicationQueueTarget::Task { task_id } => ConversationInputTarget::Task {
            task_id: sigil_kernel::TaskId::new(task_id.as_str()).map_err(unavailable)?,
        },
    })
}

/// Uses actual domain records, never a matching current value or transport acknowledgement.
pub fn reconcile_application_operation_receipt(
    request: &ApplicationCommandRequest,
    reader: &SessionRecordReadHandle,
    source_frontier: &ApplicationFrontier,
) -> Result<Option<crate::RuntimeApplicationDispatch>, ApplicationError> {
    let Some(binding) = application_operation_binding(request)? else {
        return Ok(None);
    };
    reconcile_resolved_application_operation_receipt(request, &binding, reader, source_frontier)
}

/// The resolved reader and binding are issued together by the original Session owner. The
/// parent frontier remains the application CAS; a child source has its own sequence space.
pub fn reconcile_resolved_application_operation_receipt(
    request: &ApplicationCommandRequest,
    binding: &ApplicationOperationBindingV1,
    reader: &SessionRecordReadHandle,
    source_frontier: &ApplicationFrontier,
) -> Result<Option<crate::RuntimeApplicationDispatch>, ApplicationError> {
    let Some(mut expected) = application_operation_binding(request)? else {
        return Ok(None);
    };
    expected.domain_session_scope_id = binding.domain_session_scope_id.clone();
    if &expected != binding {
        return Err(ApplicationError::ScopeMismatch);
    }
    let Some(proof) = sigil_kernel::session::reconcile_application_operation(reader, binding)
        .map_err(unavailable)?
    else {
        return Ok(None);
    };
    application_operation_receipt_from_proof(request, binding, &proof, source_frontier).map(Some)
}

pub fn application_operation_receipt_from_proof(
    request: &ApplicationCommandRequest,
    binding: &ApplicationOperationBindingV1,
    proof: &sigil_kernel::session::ApplicationOperationCommitProofV1,
    source_frontier: &ApplicationFrontier,
) -> Result<crate::RuntimeApplicationDispatch, ApplicationError> {
    let Some(mut expected) = application_operation_binding(request)? else {
        return Err(ApplicationError::ScopeMismatch);
    };
    expected.domain_session_scope_id = binding.domain_session_scope_id.clone();
    if &expected != binding {
        return Err(ApplicationError::ScopeMismatch);
    }
    proof.validate_binding(binding).map_err(unavailable)?;
    if source_frontier.scope != request.admission.scope
        || source_frontier.writer_generation != request.envelope.expected_frontier.writer_generation
        || proof.source_session_scope_id() != binding.domain_session_scope_id()
        || (binding.domain_session_scope_id() == binding.session_scope_id
            && source_frontier.through_sequence < proof.stream_sequence())
    {
        return Err(ApplicationError::ScopeMismatch);
    }
    let receipt = ApplicationDomainReceipt {
        command_id: request.envelope.command_id.clone(),
        command_kind: request.envelope.command.kind().to_owned(),
        frontier: source_frontier.clone(),
        settlement: request.envelope.command.policy().settlement,
        summary: "domain operation committed".to_owned(),
        domain_commit: ApplicationDomainCommitRef {
            source_session_scope_id: Some(proof.source_session_scope_id().to_owned()),
            source_event_id: proof.event_id().to_owned(),
            source_sequence: proof.stream_sequence(),
            source_digest: sigil_kernel::sha256_hex(proof.record_checksum().as_bytes()),
        },
        outcome: match proof.matched_control() {
            sigil_kernel::ControlEntry::ConversationRunAcceptedV1(entry) => Some(Box::new(
                sigil_application::ApplicationCommandOutcome::ConversationRunAccepted {
                    run_id: sigil_application::SafeText::new(entry.run_id.clone())?,
                },
            )),
            _ => recovery_outcome_from_proof(proof)?.map(|outcome| {
                Box::new(sigil_application::ApplicationCommandOutcome::Recovery(
                    outcome,
                ))
            }),
        },
    };
    receipt.validate_for(
        &request
            .admission
            .reservation_key(&request.envelope.command_id),
    )?;
    Ok(crate::RuntimeApplicationDispatch::Settled(receipt))
}

fn recovery_outcome_from_proof(
    proof: &sigil_kernel::session::ApplicationOperationCommitProofV1,
) -> Result<Option<sigil_application::ApplicationRecoveryOutcome>, ApplicationError> {
    use sigil_application::{ApplicationRecoveryOutcome as Outcome, SafeText};
    let count = |value: usize| {
        u64::try_from(value).map_err(|_| {
            ApplicationError::CorruptProjection("recovery count exceeds u64".to_owned())
        })
    };
    Ok(match proof.matched_control() {
        sigil_kernel::ControlEntry::PluginReviewCompletedV1(entry) => Some(Outcome::PluginReview {
            plugin_id: SafeText::new(entry.plugin_id.clone())?,
            enabled: entry.decision == sigil_kernel::PluginTrustDecision::Trusted,
            process_cleanup: entry.process_cleanup.map(plugin_cleanup_status),
        }),
        sigil_kernel::ControlEntry::BranchKnowledgeImportedV1(entry) => {
            Some(Outcome::BranchKnowledge {
                import_id: SafeText::new(entry.import_id.clone())?,
                already_imported: proof.reasserts_prior_target(),
            })
        }
        sigil_kernel::ControlEntry::ConversationForkCommittedV1(entry) => Some(Outcome::Fork {
            session_ref: SafeText::new(
                entry
                    .destination_session_ref
                    .as_path()
                    .to_string_lossy()
                    .into_owned(),
            )?,
            session_id: SafeText::new(entry.destination_session_id.clone())?,
            copied_message_count: count(entry.copied_message_count)?,
            copied_external_provenance_count: count(entry.copied_external_provenance_count)?,
        }),
        _ => None,
    })
}

/// Narrows the kernel-owned result to the application contract without a reverse dependency.
pub fn plugin_cleanup_status(
    status: sigil_kernel::PluginCleanupStatus,
) -> sigil_application::PluginCleanupStatus {
    match status {
        sigil_kernel::PluginCleanupStatus::Confirmed => {
            sigil_application::PluginCleanupStatus::Confirmed
        }
        sigil_kernel::PluginCleanupStatus::Unknown => {
            sigil_application::PluginCleanupStatus::Unknown
        }
        sigil_kernel::PluginCleanupStatus::Unconfirmed => {
            sigil_application::PluginCleanupStatus::Unconfirmed
        }
    }
}
