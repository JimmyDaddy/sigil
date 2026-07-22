use std::fmt;

use sigil_kernel::{
    AgentRunInput, ConversationInputPromotedEntry, ConversationInputQueueId,
    ConversationQueueDurableProjection, FrozenProviderRequestMaterial, MessageRole, SecretString,
    UserUrlCapabilityRegistration, project_conversation_prompt_for_persistence,
};

use crate::application_run::{
    ApplicationRunPrepareError, ApplicationRunRequest, ApplicationRunServices,
    PreparedApplicationRun, prepare_application_run_with_exact_first_request,
};

/// Exact prompt material available to the current application queue owner.
///
/// Durable queue state never stores the raw exact prompt when its persistence projection requires
/// redaction. The process-local variant therefore binds the carrier to the queue item and prompt
/// hash that admitted it. Unrelated queue mutations must not invalidate still-matching material.
/// `RequiresReentry` is an explicit fail-closed restart state;
/// it is never reconstructed from the durable safe projection.
pub enum ApplicationQueuedPromptMaterial {
    /// The durable safe prompt is also the exact prompt and may be dispatched directly.
    PersistedSafe,
    /// Exact prompt bytes retained only by the current application owner.
    AvailableProcessLocal {
        /// Queue item owning this material.
        queue_id: ConversationInputQueueId,
        /// Durable safe prompt hash under which this material was captured.
        prompt_hash: String,
        /// Non-serializable, redacted-Debug exact prompt carrier.
        exact_prompt: SecretString,
    },
    /// Exact prompt bytes were intentionally dropped across owner restart.
    RequiresReentry,
}

impl fmt::Debug for ApplicationQueuedPromptMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PersistedSafe => formatter.write_str("PersistedSafe"),
            Self::AvailableProcessLocal {
                queue_id,
                prompt_hash,
                ..
            } => formatter
                .debug_struct("AvailableProcessLocal")
                .field("queue_id", queue_id)
                .field("prompt_hash", prompt_hash)
                .field("exact_prompt", &"[redacted]")
                .finish(),
            Self::RequiresReentry => formatter.write_str("RequiresReentry"),
        }
    }
}

/// Application-owned input for preparing and committing one queued main-thread run.
///
/// `run.prompt` must be the durable safe queue projection, never the process-local exact prompt.
/// Runtime request assembly receives the exact prompt only through `prompt_material`, freezes the
/// complete first provider request before promotion, and does not return an executable run until
/// the writer-lock promotion chain commits.
pub struct ApplicationQueuedRunRequest {
    /// Ordinary run configuration, identity, permissions, model, and constraints.
    pub run: ApplicationRunRequest,
    /// Exact durable queue projection used as the promotion CAS candidate.
    pub durable_queue: ConversationQueueDurableProjection,
    /// Not-yet-appended promotion binding the safe user message and logical dispatch run id.
    pub promotion: ConversationInputPromotedEntry,
    /// Exact-material state owned by the current application process.
    pub prompt_material: ApplicationQueuedPromptMaterial,
    /// Process-local URL capabilities whose durable descriptors are bound by `promotion`.
    pub capability_registrations: Vec<UserUrlCapabilityRegistration>,
}

impl fmt::Debug for ApplicationQueuedRunRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationQueuedRunRequest")
            .field("run_id", &self.run.run_id)
            .field("session_path", &self.run.session_path)
            .field("queue_id", &self.promotion.queue_id)
            .field("queue_revision", &self.promotion.expected_queue_revision)
            .field("prompt_material", &self.prompt_material)
            .field(
                "capability_registration_count",
                &self.capability_registrations.len(),
            )
            .finish_non_exhaustive()
    }
}

/// Pure input required to bind one durable queue candidate to an exact frozen provider request.
///
/// The caller must first construct the frozen request without mutating durable state. Promotion
/// remains a separate writer-lock CAS barrier. This request is consumed so exact prompt material
/// does not remain copied in the preparation result.
pub(crate) struct ApplicationQueuedRunPreparationRequest {
    /// Durable session scope that will execute the queued run.
    pub(crate) session_scope_id: String,
    /// Exact durable queue projection used as the promotion CAS candidate.
    pub(crate) durable_queue: ConversationQueueDurableProjection,
    /// Not-yet-appended promotion binding the safe user message and logical dispatch run id.
    pub(crate) promotion: ConversationInputPromotedEntry,
    /// Exact-material state owned by the current application process.
    pub(crate) prompt_material: ApplicationQueuedPromptMaterial,
    /// Process-local URL capabilities whose durable descriptors are bound by `promotion`.
    pub(crate) capability_registrations: Vec<UserUrlCapabilityRegistration>,
    /// Complete first provider request built from the exact prompt.
    pub(crate) frozen_request: FrozenProviderRequestMaterial,
}

impl fmt::Debug for ApplicationQueuedRunPreparationRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationQueuedRunPreparationRequest")
            .field("session_scope_id", &self.session_scope_id)
            .field("queue_id", &self.promotion.queue_id)
            .field("queue_revision", &self.promotion.expected_queue_revision)
            .field("dispatch_run_id", &self.promotion.dispatch_run_id)
            .field("prompt_material", &self.prompt_material)
            .field(
                "capability_registration_count",
                &self.capability_registrations.len(),
            )
            .field("frozen_request", &self.frozen_request)
            .finish_non_exhaustive()
    }
}

/// Stable routing class for queued-run preparation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationQueuedRunPrepareErrorClass {
    /// The supplied session or promotion identity is malformed.
    InvalidInvocation,
    /// Normal application provider/session/tool assembly failed before promotion.
    ApplicationPreparation,
    /// The durable queue changed, is paused, or no longer admits this candidate.
    QueueConflict,
    /// Exact prompt material is unavailable after owner restart.
    RequiresReentry,
    /// Process-local prompt material is stale or does not match its durable projection.
    PromptMaterialMismatch,
    /// The frozen provider request is cross-session or does not contain the exact promoted turn.
    FrozenRequestMismatch,
    /// The writer-lock promotion barrier, live projection, or capability commit failed.
    PromotionCommit,
}

/// Typed, secret-safe queued-run preparation failure.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationQueuedRunPrepareError {
    /// Invalid caller-provided identity.
    #[error("invalid queued run preparation: {message}")]
    InvalidInvocation {
        /// Safe validation detail.
        message: &'static str,
    },
    /// Normal application provider/session/tool assembly failed before promotion.
    #[error("application queued run assembly failed")]
    ApplicationPreparation {
        #[source]
        source: ApplicationRunPrepareError,
    },
    /// Durable queue CAS candidate is no longer current.
    #[error("queued run conflicts with the durable queue state")]
    QueueConflict {
        #[source]
        source: anyhow::Error,
    },
    /// Exact prompt bytes are not recoverable from durable state.
    #[error("queued prompt requires reentry before dispatch")]
    RequiresReentry,
    /// Exact material and its queue binding disagree.
    #[error("queued prompt material does not match its durable binding: {message}")]
    PromptMaterialMismatch {
        /// Safe validation detail.
        message: &'static str,
    },
    /// The prebuilt provider request is not the exact promoted turn.
    #[error("frozen queued provider request is invalid: {message}")]
    FrozenRequestMismatch {
        /// Safe validation detail.
        message: &'static str,
    },
    /// Durable promotion, live projection, or capability commit failure.
    #[error("queued promotion could not be committed at {stage}")]
    PromotionCommit {
        /// Stable failing stage without local path or prompt content.
        stage: &'static str,
        #[source]
        source: anyhow::Error,
    },
}

impl ApplicationQueuedRunPrepareError {
    /// Returns the stable machine-routing class without parsing display text.
    #[must_use]
    pub const fn class(&self) -> ApplicationQueuedRunPrepareErrorClass {
        match self {
            Self::InvalidInvocation { .. } => {
                ApplicationQueuedRunPrepareErrorClass::InvalidInvocation
            }
            Self::ApplicationPreparation { .. } => {
                ApplicationQueuedRunPrepareErrorClass::ApplicationPreparation
            }
            Self::QueueConflict { .. } => ApplicationQueuedRunPrepareErrorClass::QueueConflict,
            Self::RequiresReentry => ApplicationQueuedRunPrepareErrorClass::RequiresReentry,
            Self::PromptMaterialMismatch { .. } => {
                ApplicationQueuedRunPrepareErrorClass::PromptMaterialMismatch
            }
            Self::FrozenRequestMismatch { .. } => {
                ApplicationQueuedRunPrepareErrorClass::FrozenRequestMismatch
            }
            Self::PromotionCommit { .. } => ApplicationQueuedRunPrepareErrorClass::PromotionCommit,
        }
    }

    pub(crate) const fn invalid_invocation(message: &'static str) -> Self {
        Self::InvalidInvocation { message }
    }

    fn application_preparation(source: ApplicationRunPrepareError) -> Self {
        Self::ApplicationPreparation { source }
    }

    pub(crate) const fn prompt_material_mismatch(message: &'static str) -> Self {
        Self::PromptMaterialMismatch { message }
    }

    pub(crate) const fn frozen_request_mismatch(message: &'static str) -> Self {
        Self::FrozenRequestMismatch { message }
    }

    pub(crate) fn promotion_commit(stage: &'static str, source: anyhow::Error) -> Self {
        Self::PromotionCommit { stage, source }
    }
}

/// Performs application-owned exact request assembly and commits one queued run.
///
/// The normal request carries only the durable safe prompt. The exact prompt remains in a
/// redacted, process-local carrier while runtime resolves context, constrains the final tool
/// registry, and freezes the complete first provider request. The returned run is executable only
/// after the promotion CAS and live capability commit both succeed. The promotion is the unique
/// durable user event and embeds the safe user message plus capability descriptors.
///
/// # Errors
///
/// Returns a typed error for stale queue state, unavailable or mismatched exact material,
/// application assembly failure, invalid frozen material, or promotion commit failure. A failure
/// never returns a prepared run.
pub async fn prepare_application_queued_run(
    request: ApplicationQueuedRunRequest,
    services: &ApplicationRunServices,
) -> Result<PreparedApplicationRun, ApplicationQueuedRunPrepareError> {
    let exact_prompt = validate_application_queued_run_request(&request)?;
    let ApplicationQueuedRunRequest {
        run,
        durable_queue,
        promotion,
        prompt_material,
        capability_registrations,
    } = request;
    let (prepared, assembly) = prepare_application_run_with_exact_first_request(
        run,
        services,
        exact_prompt,
        promotion.durable_user_message.id.clone(),
    )
    .await
    .map_err(ApplicationQueuedRunPrepareError::application_preparation)?;
    let mut queued =
        prepare_application_queued_run_input(ApplicationQueuedRunPreparationRequest {
            session_scope_id: prepared.session_id().to_owned(),
            durable_queue,
            promotion,
            prompt_material,
            capability_registrations,
            frozen_request: assembly.frozen_request,
        })?;
    queued.input = assembly.run_input;
    prepared.commit_queued_promotion(queued)
}

/// Validated queued first-turn input consumed by [`crate::application_run::PreparedApplicationRun`].
///
/// The only constructor is [`prepare_application_queued_run_input`]. Its inner `AgentRunInput`
/// has no persisted user message, owns the exact frozen first request, and uses the promotion's
/// logical dispatch run id for provider physical-attempt audit.
pub(crate) struct PreparedApplicationQueuedRunInput {
    pub(crate) session_scope_id: String,
    pub(crate) promotion: ConversationInputPromotedEntry,
    pub(crate) safe_prompt: String,
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
    pub(crate) input: AgentRunInput,
    pub(crate) capability_registrations: Vec<UserUrlCapabilityRegistration>,
    frozen_request_fingerprint: String,
}

impl fmt::Debug for PreparedApplicationQueuedRunInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedApplicationQueuedRunInput")
            .field("session_scope_id", &self.session_scope_id)
            .field("queue_id", &self.promotion.queue_id)
            .field("dispatch_run_id", &self.promotion.dispatch_run_id)
            .field(
                "durable_user_message_id",
                &self.promotion.durable_user_message.id,
            )
            .field(
                "capability_registration_count",
                &self.capability_registrations.len(),
            )
            .field(
                "frozen_request_fingerprint",
                &self.frozen_request_fingerprint,
            )
            .finish_non_exhaustive()
    }
}

impl PreparedApplicationQueuedRunInput {
    /// Returns the durable queue item identity.
    #[must_use]
    #[cfg(test)]
    pub fn queue_id(&self) -> &ConversationInputQueueId {
        &self.promotion.queue_id
    }

    /// Returns the logical run id bound to the first provider physical attempt.
    #[must_use]
    #[cfg(test)]
    pub fn dispatch_run_id(&self) -> &str {
        &self.promotion.dispatch_run_id
    }

    /// Returns the process-keyed frozen request fingerprint for safe audit correlation.
    #[must_use]
    #[cfg(test)]
    pub fn frozen_request_fingerprint(&self) -> &str {
        &self.frozen_request_fingerprint
    }
}

fn validate_application_queued_run_request(
    request: &ApplicationQueuedRunRequest,
) -> Result<SecretString, ApplicationQueuedRunPrepareError> {
    if request.run.run_id != request.promotion.dispatch_run_id {
        return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
            "application run id does not match the queued dispatch run id",
        ));
    }
    if request.run.skill_binding.is_some() || request.run.agent_binding.is_some() {
        return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
            "queued main-thread chat cannot invoke a skill or agent profile binding",
        ));
    }
    request
        .durable_queue
        .validate_promotion(&request.promotion)
        .map_err(|source| ApplicationQueuedRunPrepareError::QueueConflict { source })?;
    let queued = request
        .durable_queue
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == request.promotion.queue_id)
        .ok_or(ApplicationQueuedRunPrepareError::QueueConflict {
            source: anyhow::anyhow!("promoted queue item is absent from the durable projection"),
        })?;
    if request.run.prompt != queued.queued.prompt {
        return Err(ApplicationQueuedRunPrepareError::prompt_material_mismatch(
            "application run prompt is not the durable safe prompt",
        ));
    }
    if request.run.reasoning_effort != queued.queued.reasoning_effort {
        return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
            "application run reasoning effort does not match the queued input",
        ));
    }
    let exact_prompt = resolve_exact_prompt(
        &request.prompt_material,
        &request.promotion,
        &queued.queued.prompt,
    )?;
    Ok(SecretString::new(exact_prompt))
}

/// Validates and freezes the handoff from a durable queued candidate to one application run.
///
/// This function performs no durable write and no provider I/O. It rejects a stale promotion CAS,
/// unavailable exact material, cross-session frozen material, and a request that does not contain
/// exactly one exact user turn under the promoted durable message id.
///
/// # Errors
///
/// Returns a typed error before execution when any queue, material, or frozen-request binding is
/// missing or stale.
pub(crate) fn prepare_application_queued_run_input(
    request: ApplicationQueuedRunPreparationRequest,
) -> Result<PreparedApplicationQueuedRunInput, ApplicationQueuedRunPrepareError> {
    if request.session_scope_id.trim().is_empty() {
        return Err(ApplicationQueuedRunPrepareError::invalid_invocation(
            "session scope id must not be empty",
        ));
    }
    request
        .promotion
        .validate_for_session(&request.session_scope_id)
        .map_err(|source| ApplicationQueuedRunPrepareError::QueueConflict { source })?;
    request
        .durable_queue
        .validate_promotion(&request.promotion)
        .map_err(|source| ApplicationQueuedRunPrepareError::QueueConflict { source })?;

    let queued = request
        .durable_queue
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == request.promotion.queue_id)
        .ok_or(ApplicationQueuedRunPrepareError::QueueConflict {
            source: anyhow::anyhow!("promoted queue item is absent from the durable projection"),
        })?;
    let exact_prompt = resolve_exact_prompt(
        &request.prompt_material,
        &request.promotion,
        &queued.queued.prompt,
    )?;

    if request.frozen_request.session_scope_id() != request.session_scope_id {
        return Err(ApplicationQueuedRunPrepareError::frozen_request_mismatch(
            "session scope does not match the queued session",
        ));
    }
    validate_frozen_exact_user_message(
        &request.frozen_request,
        &request.promotion.durable_user_message.id,
        exact_prompt,
    )?;
    validate_capability_registrations(
        &request.session_scope_id,
        &request.promotion,
        &request.capability_registrations,
    )?;

    let provider_name = request.frozen_request.request().provider_name.clone();
    let model_name = request.frozen_request.request().model_name.clone();
    let frozen_request_fingerprint = request.frozen_request.fingerprint().to_owned();
    let input = AgentRunInput::without_persisted_user_message(Vec::new())
        .with_initial_frozen_provider_request(request.frozen_request)
        .with_logical_run_id(request.promotion.dispatch_run_id.clone());

    Ok(PreparedApplicationQueuedRunInput {
        session_scope_id: request.session_scope_id,
        promotion: request.promotion,
        safe_prompt: queued.queued.prompt.clone(),
        provider_name,
        model_name,
        input,
        capability_registrations: request.capability_registrations,
        frozen_request_fingerprint,
    })
}

fn validate_capability_registrations(
    session_scope_id: &str,
    promotion: &ConversationInputPromotedEntry,
    registrations: &[UserUrlCapabilityRegistration],
) -> Result<(), ApplicationQueuedRunPrepareError> {
    let descriptors = registrations
        .iter()
        .map(|registration| registration.durable_descriptor(session_scope_id))
        .collect::<Vec<_>>();
    if descriptors != promotion.capability_descriptors {
        return Err(ApplicationQueuedRunPrepareError::frozen_request_mismatch(
            "URL capability registrations do not match the promotion descriptors",
        ));
    }
    Ok(())
}

fn resolve_exact_prompt<'a>(
    material: &'a ApplicationQueuedPromptMaterial,
    promotion: &ConversationInputPromotedEntry,
    safe_prompt: &'a str,
) -> Result<&'a str, ApplicationQueuedRunPrepareError> {
    let exact_prompt = match material {
        ApplicationQueuedPromptMaterial::PersistedSafe => {
            if promotion.exact_prompt_required {
                return Err(ApplicationQueuedRunPrepareError::RequiresReentry);
            }
            safe_prompt
        }
        ApplicationQueuedPromptMaterial::RequiresReentry => {
            if promotion.exact_prompt_required {
                return Err(ApplicationQueuedRunPrepareError::RequiresReentry);
            }
            return Err(ApplicationQueuedRunPrepareError::prompt_material_mismatch(
                "persisted-safe prompt was marked as requiring reentry",
            ));
        }
        ApplicationQueuedPromptMaterial::AvailableProcessLocal {
            queue_id,
            prompt_hash,
            exact_prompt,
        } => {
            if queue_id != &promotion.queue_id {
                return Err(ApplicationQueuedRunPrepareError::prompt_material_mismatch(
                    "queue id does not match the promotion",
                ));
            }
            if prompt_hash != &promotion.prompt_hash {
                return Err(ApplicationQueuedRunPrepareError::prompt_material_mismatch(
                    "prompt hash does not match the promotion",
                ));
            }
            exact_prompt.expose_secret()
        }
    };

    let projection = project_conversation_prompt_for_persistence(exact_prompt);
    if projection.prompt_hash != promotion.prompt_hash
        || projection.safe_prompt != safe_prompt
        || projection.exact_prompt_required != promotion.exact_prompt_required
    {
        return Err(ApplicationQueuedRunPrepareError::prompt_material_mismatch(
            "exact prompt does not reproduce the durable safe projection",
        ));
    }
    Ok(exact_prompt)
}

fn validate_frozen_exact_user_message(
    frozen_request: &FrozenProviderRequestMaterial,
    durable_user_message_id: &str,
    exact_prompt: &str,
) -> Result<(), ApplicationQueuedRunPrepareError> {
    let mut matches = frozen_request
        .request()
        .messages
        .iter()
        .filter(|message| message.id == durable_user_message_id);
    let Some(message) = matches.next() else {
        return Err(ApplicationQueuedRunPrepareError::frozen_request_mismatch(
            "promoted exact user message is missing",
        ));
    };
    if matches.next().is_some() {
        return Err(ApplicationQueuedRunPrepareError::frozen_request_mismatch(
            "promoted exact user message appears more than once",
        ));
    }
    if message.role != MessageRole::User
        || message.tool_call_id.is_some()
        || message.assistant_kind.is_some()
        || !message.tool_calls.is_empty()
        || message.content.as_deref() != Some(exact_prompt)
    {
        return Err(ApplicationQueuedRunPrepareError::frozen_request_mismatch(
            "promoted user message does not contain the exact prompt",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/application_queue_tests.rs"]
mod tests;
