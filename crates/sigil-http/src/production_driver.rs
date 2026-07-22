use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc as std_mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalHandler, CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX, ControlEntry,
    ConversationInputEditedEntry, ConversationInputKind, ConversationInputPromotedEntry,
    ConversationInputQueueId, ConversationInputQueuedEntry, ConversationInputReorderedEntry,
    ConversationInputStatus, ConversationInputStatusEntry, ConversationInputTarget,
    ConversationInputTerminalCommand, ConversationInputTerminalExpectation,
    ConversationInputTerminalFrontier, ConversationQueueDurableProjection,
    ConversationQueueMutation, ConversationQueueMutationCommand, ConversationQueueRevision,
    JsonlSessionStore, ModelMessage, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptProjection, PublicRunEvent, PublicRunEventKind, SecretString,
    SessionLogEntry, SessionRef, ToolApproval, ToolApprovalUserDecision, ToolCall, ToolSpec,
    conversation_promotion_capability_digest, project_conversation_prompt_for_persistence,
    project_user_message_for_persistence_with_nonce_and_issued_at, stable_event_uuid,
    tool_approval_session_grant_available_for_facets,
};
use sigil_runtime::application_queue::{
    ApplicationQueuedPromptMaterial, ApplicationQueuedRunRequest, prepare_application_queued_run,
};
use sigil_runtime::application_run::{
    ApplicationRunControl, ApplicationRunEventHandler, ApplicationRunInteraction,
    ApplicationRunOutput, ApplicationRunRequest, ApplicationRunServices,
    ApplicationRunTerminalStatus, ApplicationTranscriptRole, PreparedApplicationRun,
    application_agent_activity_view, application_run_context_view,
    application_session_frontier_view, application_session_transcript_page,
    application_verification_view, bind_application_session_with_model,
    bind_existing_application_session, prepare_application_run,
    record_application_preparation_cancellation, rerun_application_verification,
};
use sigil_runtime::conversation_display::{
    ConversationDisplayProjectionError, conversation_display_page,
};
use sigil_runtime::{LocalSessionLifecycleService, LocalSessionReopenError};
use tokio::{runtime::Handle, sync::mpsc};

use crate::{
    HTTP_APPROVAL_POLICY_VERSION, HttpAgentActivityItem, HttpAgentActivityStatus,
    HttpAgentActivityView, HttpAgentHandoffStatus, HttpAgentUsageSummary,
    HttpApplicationAgentCatalogEntry, HttpApplicationClientAction,
    HttpApplicationCommandCatalogEntry, HttpApplicationExtensionCatalog,
    HttpApplicationModelOption, HttpApplicationSkillBinding, HttpApplicationSkillCatalogEntry,
    HttpApprovalDecisionRecord, HttpContextWindowSource, HttpConversationDisplayDriverError,
    HttpConversationDisplayPage, HttpConversationQueueBlockedReason,
    HttpConversationQueueCommandAction, HttpConversationQueueDriverCommand,
    HttpConversationQueueDriverError, HttpConversationQueueGeneration, HttpConversationQueueItem,
    HttpConversationQueueItemKind, HttpConversationQueueItemStatus,
    HttpConversationQueuePromptMaterial, HttpConversationQueueView, HttpDurableCommandStore,
    HttpDurableEgressDisclosureJournal, HttpDurableEgressDisclosurePresenter, HttpLiveEventBus,
    HttpModelSelectionPolicy, HttpPendingApproval, HttpPermissionMode, HttpQueuedRunAdmission,
    HttpQueuedRunDriverStart, HttpRunContextView, HttpRunDriver, HttpRunDriverApproval,
    HttpRunDriverCancel, HttpRunDriverError, HttpRunDriverStart, HttpRunTerminalOutcome,
    HttpSessionBinding, HttpSessionOpenBindingError, HttpSessionRunRegistry,
    HttpSessionTranscriptMessage, HttpSessionTranscriptPage, HttpTranscriptAssistantKind,
    HttpTranscriptRole, HttpVerificationRerunRequest, HttpVerificationView,
};

const DEFAULT_HTTP_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DEFAULT_HTTP_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_EXACT_QUEUE_PROMPTS: usize = 128;
const MAX_HTTP_QUEUE_PREVIEW_CHARS: usize = 240;

/// Runtime inputs and bounded waits owned by the production HTTP driver.
#[derive(Debug, Clone)]
pub struct HttpProductionRunDriverOptions {
    /// Resolved Sigil configuration path.
    pub config_path: PathBuf,
    /// Process launch working directory used for workspace resolution.
    pub launch_cwd: PathBuf,
    /// Maximum time an externally interactive approval may remain pending.
    pub approval_timeout: Duration,
    /// Maximum time allowed for cooperative cancellation quiescence.
    pub cancellation_timeout: Duration,
    /// Workspace-bound lifecycle truth used to authorize historical session reopen.
    pub session_lifecycle: Option<LocalSessionLifecycleService>,
}

impl HttpProductionRunDriverOptions {
    /// Creates production defaults for one config/workspace pair.
    #[must_use]
    pub fn new(config_path: impl Into<PathBuf>, launch_cwd: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            approval_timeout: DEFAULT_HTTP_APPROVAL_TIMEOUT,
            cancellation_timeout: DEFAULT_HTTP_CANCELLATION_TIMEOUT,
            session_lifecycle: None,
        }
    }

    /// Attaches workspace-bound lifecycle truth for durable session reopen.
    #[must_use]
    pub fn with_session_lifecycle(
        mut self,
        session_lifecycle: LocalSessionLifecycleService,
    ) -> Self {
        self.session_lifecycle = Some(session_lifecycle);
        self
    }
}

#[async_trait]
trait HttpApplicationRunPreparer: Send + Sync {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun>;

    async fn prepare_queued(
        &self,
        request: ApplicationQueuedRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun>;
}

struct HttpSharedApplicationRunPreparer;

#[async_trait]
impl HttpApplicationRunPreparer for HttpSharedApplicationRunPreparer {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        prepare_application_run(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }

    async fn prepare_queued(
        &self,
        request: ApplicationQueuedRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        prepare_application_queued_run(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }
}

/// Production run driver backed by the shared runtime application service.
pub struct HttpProductionRunDriver {
    options: HttpProductionRunDriverOptions,
    services: ApplicationRunServices,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    runtime: Handle,
    registry: OnceLock<Weak<HttpSessionRunRegistry>>,
    active_runs: Arc<Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: Arc<Condvar>,
    exact_queue_prompts: Arc<Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpExactQueuePromptKey {
    session_scope_id: String,
    queue_id: ConversationInputQueueId,
}

#[derive(Clone)]
struct HttpExactQueuePrompt {
    prompt_hash: String,
    exact_prompt: SecretString,
}

struct HttpQueuedRunPreparation {
    durable_queue: ConversationQueueDurableProjection,
    promotion: ConversationInputPromotedEntry,
    prompt_material: ApplicationQueuedPromptMaterial,
    capability_registrations: Vec<sigil_kernel::UserUrlCapabilityRegistration>,
    exact_prompt_key: HttpExactQueuePromptKey,
}

#[derive(Clone)]
struct HttpQueuedRunTerminalContext {
    queue_id: ConversationInputQueueId,
    dispatch_run_id: String,
    expected_queue_revision: ConversationQueueRevision,
    prompt_hash: String,
    exact_prompt_key: HttpExactQueuePromptKey,
}

#[derive(Clone, Copy)]
enum HttpQueuedUnpromotedTerminal {
    Rejected,
    Cancelled,
}

impl std::fmt::Debug for HttpExactQueuePrompt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpExactQueuePrompt")
            .field("prompt_hash", &self.prompt_hash)
            .field("exact_prompt", &"[redacted]")
            .finish()
    }
}

impl std::fmt::Debug for HttpProductionRunDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpProductionRunDriver")
            .field("options", &self.options)
            .field("services", &self.services)
            .field("preparer", &"configured")
            .field("event_bus", &"configured")
            .finish_non_exhaustive()
    }
}

impl HttpProductionRunDriver {
    /// Creates a production driver. Call `build_registry` before starting runs.
    ///
    /// # Errors
    ///
    /// Returns an error when the event bus has no durable protocol journal.
    pub fn new(
        options: HttpProductionRunDriverOptions,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
        event_bus: Arc<HttpLiveEventBus>,
        runtime: Handle,
    ) -> Result<Self, HttpRunDriverError> {
        Self::new_with_preparer(
            options,
            disclosure_journal,
            event_bus,
            runtime,
            Arc::new(HttpSharedApplicationRunPreparer),
        )
    }

    fn new_with_preparer(
        options: HttpProductionRunDriverOptions,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
        event_bus: Arc<HttpLiveEventBus>,
        runtime: Handle,
        preparer: Arc<dyn HttpApplicationRunPreparer>,
    ) -> Result<Self, HttpRunDriverError> {
        if !event_bus.has_durable_journal() {
            return Err(HttpRunDriverError::new(
                "production driver requires a durable protocol journal",
            ));
        }
        let services = ApplicationRunServices::new(Arc::new(
            HttpDurableEgressDisclosurePresenter::new(disclosure_journal),
        ));
        Ok(Self {
            options,
            services,
            preparer,
            event_bus,
            runtime,
            registry: OnceLock::new(),
            active_runs: Arc::new(Mutex::new(BTreeMap::new())),
            active_runs_ready: Arc::new(Condvar::new()),
            exact_queue_prompts: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// Builds and attaches the one process-local registry driven by this instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver was already attached to another registry.
    pub fn build_registry(
        self: &Arc<Self>,
        command_store: Arc<HttpDurableCommandStore>,
    ) -> Result<Arc<HttpSessionRunRegistry>, HttpRunDriverError> {
        let driver: Arc<dyn HttpRunDriver> = self.clone();
        let registry = Arc::new(HttpSessionRunRegistry::with_durable_command_store(
            driver,
            command_store,
        ));
        self.registry
            .set(Arc::downgrade(&registry))
            .map_err(|_| HttpRunDriverError::new("production driver registry already attached"))?;
        Ok(registry)
    }

    /// Returns the number of owned run supervisors that have not completed cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error when the active-run state is unavailable.
    pub fn active_run_count(&self) -> Result<usize, HttpRunDriverError> {
        self.active_runs
            .lock()
            .map(|runs| runs.len())
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))
    }

    fn attached_registry(&self) -> Result<Arc<HttpSessionRunRegistry>, HttpRunDriverError> {
        self.registry
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| HttpRunDriverError::new("production driver registry is not attached"))
    }

    fn reconcile_orphaned_queued_dispatches(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<(), HttpConversationQueueDriverError> {
        self.reconcile_orphaned_queued_dispatches_with(session, |_| Ok(()))
    }

    fn reconcile_orphaned_queued_dispatches_with<F>(
        &self,
        session: &crate::HttpSessionSnapshot,
        mut before_terminal_append: F,
    ) -> Result<(), HttpConversationQueueDriverError>
    where
        F: FnMut(&JsonlSessionStore) -> Result<(), HttpConversationQueueDriverError>,
    {
        for _ in 0..=crate::HTTP_MAX_CONVERSATION_QUEUE_ITEMS {
            let records = JsonlSessionStore::read_event_records(&session.session_log_path)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            if records
                .iter()
                .any(|record| record.session_id() != session.durable_session_scope_id)
            {
                return Err(HttpConversationQueueDriverError::Unavailable);
            }
            let projection = ConversationQueueDurableProjection::from_records(&records)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            let Some(item) = projection
                .queue
                .items
                .iter()
                .find(|item| item.status == ConversationInputStatus::Dispatching)
            else {
                return Ok(());
            };
            let promotion = http_queued_promotion(&records, &item.queued.queue_id)
                .ok_or(HttpConversationQueueDriverError::Conflict)?;
            let (status, reason) =
                http_queued_terminal_from_attempt_evidence(&records, &promotion.dispatch_run_id)
                    .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            let expected_frontier = records
                .last()
                .map(ConversationInputTerminalFrontier::from_record)
                .ok_or(HttpConversationQueueDriverError::Unavailable)?;
            let store = JsonlSessionStore::new(&session.session_log_path)
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            before_terminal_append(&store)?;
            let appended = store
                .append_conversation_input_terminal_if_current(ConversationInputTerminalCommand {
                    expectation: ConversationInputTerminalExpectation::Promoted {
                        queue_id: promotion.queue_id.clone(),
                        dispatch_run_id: promotion.dispatch_run_id,
                        expected_frontier,
                    },
                    terminal: ConversationInputStatusEntry {
                        queue_id: promotion.queue_id.clone(),
                        status,
                        reason,
                        updated_at_ms: Some(current_unix_time_ms()),
                    },
                })
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            if appended.is_none() {
                continue;
            }
            self.exact_queue_prompts
                .lock()
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?
                .remove(&exact_queue_prompt_key(session, promotion.queue_id));
        }
        Err(HttpConversationQueueDriverError::Conflict)
    }

    fn queued_supervisor_start(
        &self,
        start: HttpQueuedRunDriverStart,
    ) -> Result<(HttpRunDriverStart, HttpQueuedRunPreparation), HttpRunDriverError> {
        if start.run.id != start.admission.dispatch_run_id
            || start.session.foreground_run_id.as_deref() != Some(start.run.id.as_str())
        {
            return Err(HttpRunDriverError::new(
                "queued run registration does not own the admitted foreground identity",
            ));
        }
        let state = read_http_durable_queue_state(&start.session)
            .map_err(|_| HttpRunDriverError::new("durable queued run state is unavailable"))?;
        let revision = state.projection.current_revision();
        if start.admission.generation != http_queue_generation(revision.clone()) {
            return Err(HttpRunDriverError::new(
                "queued run admission no longer matches the durable generation",
            ));
        }
        let queue_id = ConversationInputQueueId::new(start.admission.entry_id.clone())
            .map_err(|_| HttpRunDriverError::new("queued run entry identity is invalid"))?;
        if state.projection.queue.next_dispatchable.as_ref() != Some(&queue_id) {
            return Err(HttpRunDriverError::new(
                "queued run entry is no longer the durable dispatch frontier",
            ));
        }
        let queued = state
            .projection
            .queue
            .items
            .iter()
            .find(|item| item.queued.queue_id == queue_id)
            .ok_or_else(|| HttpRunDriverError::new("queued run entry is unavailable"))?;
        if queued.status != ConversationInputStatus::Queued
            || queued.queued.target != ConversationInputTarget::MainThread
            || queued.queued.kind != ConversationInputKind::Chat
        {
            return Err(HttpRunDriverError::new(
                "queued run entry is not a dispatchable main-thread chat",
            ));
        }
        let dispatch_run_id = stable_http_queued_dispatch_run_id(
            &start.session.durable_session_scope_id,
            &queue_id,
            &revision,
        );
        if dispatch_run_id != start.admission.dispatch_run_id {
            return Err(HttpRunDriverError::new(
                "queued run dispatch identity no longer matches durable admission",
            ));
        }

        let exact_prompt_key = exact_queue_prompt_key(&start.session, queue_id.clone());
        let (prompt_material, exact_prompt) = if queued
            .queued
            .prompt_hash
            .starts_with(CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX)
        {
            let exact_prompts = self
                .exact_queue_prompts
                .lock()
                .map_err(|_| HttpRunDriverError::new("queued exact prompt state is unavailable"))?;
            let exact = exact_prompts
                .get(&exact_prompt_key)
                .filter(|exact| exact.prompt_hash == queued.queued.prompt_hash)
                .ok_or_else(|| {
                    HttpRunDriverError::new("queued exact prompt requires user reentry")
                })?;
            (
                ApplicationQueuedPromptMaterial::AvailableProcessLocal {
                    queue_id: queue_id.clone(),
                    prompt_hash: exact.prompt_hash.clone(),
                    exact_prompt: exact.exact_prompt.clone(),
                },
                exact.exact_prompt.expose_secret().to_owned(),
            )
        } else {
            (
                ApplicationQueuedPromptMaterial::PersistedSafe,
                queued.queued.prompt.clone(),
            )
        };
        let prompt_projection = project_conversation_prompt_for_persistence(&exact_prompt);
        if prompt_projection.prompt_hash != queued.queued.prompt_hash
            || prompt_projection.safe_prompt != queued.queued.prompt
        {
            return Err(HttpRunDriverError::new(
                "queued exact prompt no longer matches its durable projection",
            ));
        }

        let promotion_seed = stable_http_identity_seed(&[
            &start.session.durable_session_scope_id,
            queue_id.as_str(),
            &revision.stream_sequence.to_string(),
            &revision.event_id,
        ]);
        let durable_message_id = stable_event_uuid(
            "sigil-http-conversation-queue-user-message",
            &promotion_seed,
        );
        let promoted_at_ms = current_unix_time_ms();
        let capability_projection = project_user_message_for_persistence_with_nonce_and_issued_at(
            durable_message_id.clone(),
            exact_prompt,
            Some(&dispatch_run_id),
            promoted_at_ms,
            None,
        )
        .map_err(|_| HttpRunDriverError::new("queued URL capability projection failed"))?;
        let mut capability_registrations = capability_projection.capability_registrations;
        capability_registrations.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        let capability_descriptors = capability_registrations
            .iter()
            .map(|registration| {
                registration.durable_descriptor(&start.session.durable_session_scope_id)
            })
            .collect::<Vec<_>>();
        let capability_digest =
            conversation_promotion_capability_digest(&capability_descriptors)
                .map_err(|_| HttpRunDriverError::new("queued capability digest failed"))?;
        let mut durable_user_message = ModelMessage::user(queued.queued.prompt.clone());
        durable_user_message.id = durable_message_id;
        let promotion = ConversationInputPromotedEntry {
            queue_id,
            expected_queue_revision: revision,
            prompt_hash: queued.queued.prompt_hash.clone(),
            exact_prompt_required: prompt_projection.exact_prompt_required,
            durable_user_message,
            capability_descriptors,
            capability_digest,
            dispatch_run_id,
            promoted_at_ms,
        };
        promotion
            .validate_for_session(&start.session.durable_session_scope_id)
            .map_err(|_| HttpRunDriverError::new("queued promotion candidate is invalid"))?;

        let standard_start = HttpRunDriverStart {
            session: start.session,
            run: start.run,
            prompt: queued.queued.prompt.clone(),
            model_name: None,
            model_selection_binding: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
        };
        Ok((
            standard_start,
            HttpQueuedRunPreparation {
                durable_queue: state.projection,
                promotion,
                prompt_material,
                capability_registrations,
                exact_prompt_key,
            },
        ))
    }

    fn start_supervised_run(
        &self,
        start: HttpRunDriverStart,
        queued: Option<HttpQueuedRunPreparation>,
    ) -> Result<(), HttpRunDriverError> {
        let registry = self.attached_registry()?;
        let broker = Arc::new(HttpApprovalBroker::default());
        let (cancel_sender, cancel_receiver) = mpsc::unbounded_channel();
        let active = Arc::new(HttpProductionActiveRun {
            session_id: start.session.id.clone(),
            broker: Arc::clone(&broker),
            cancel_sender,
        });
        {
            let mut runs = self
                .active_runs
                .lock()
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            if runs.contains_key(&start.run.id) {
                return Err(HttpRunDriverError::new(format!(
                    "production run already active: {}",
                    start.run.id
                )));
            }
            runs.insert(start.run.id.clone(), active);
        }

        let queued_terminal = queued.as_ref().map(|queued| HttpQueuedRunTerminalContext {
            queue_id: queued.promotion.queue_id.clone(),
            dispatch_run_id: queued.promotion.dispatch_run_id.clone(),
            expected_queue_revision: queued.promotion.expected_queue_revision.clone(),
            prompt_hash: queued.promotion.prompt_hash.clone(),
            exact_prompt_key: queued.exact_prompt_key.clone(),
        });
        let queued_session = start.session.clone();
        let terminal_exact_queue_prompts = Arc::clone(&self.exact_queue_prompts);

        let supervisor = HttpRunSupervisor {
            options: self.options.clone(),
            services: self.services.clone(),
            preparer: Arc::clone(&self.preparer),
            event_bus: Arc::clone(&self.event_bus),
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&broker),
            start: start.clone(),
            queued,
            exact_queue_prompts: Arc::clone(&self.exact_queue_prompts),
            cancel_receiver,
        };
        let task = self.runtime.spawn(supervisor.run());
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_ready = Arc::clone(&self.active_runs_ready);
        let registry = Arc::downgrade(&registry);
        let run_id = start.run.id;
        self.runtime.spawn(async move {
            let mut uncertain = match task.await {
                Ok(Ok(())) => false,
                Ok(Err(_)) | Err(_) => true,
            };
            if let Some(queued_terminal) = queued_terminal {
                let unpromoted_terminal = registry
                    .upgrade()
                    .and_then(|registry| registry.get_run(&run_id).ok())
                    .map_or(HttpQueuedUnpromotedTerminal::Rejected, |run| {
                        match run.status {
                            crate::HttpRunStatus::Cancelled | crate::HttpRunStatus::Interrupted => {
                                HttpQueuedUnpromotedTerminal::Cancelled
                            }
                            _ => HttpQueuedUnpromotedTerminal::Rejected,
                        }
                    });
                uncertain |= tokio::task::spawn_blocking(move || {
                    finalize_http_queued_terminal(
                        &queued_session,
                        &queued_terminal,
                        unpromoted_terminal,
                    )?;
                    evict_http_promoted_exact_prompt(
                        &queued_session,
                        Some(&queued_terminal),
                        &terminal_exact_queue_prompts,
                    )
                })
                .await
                .map_or(true, |result| result.is_err());
            }
            broker.cancel_all();
            if uncertain && let Some(registry) = registry.upgrade() {
                let _ = registry.record_run_execution_uncertain(&run_id);
            }
            if let Ok(mut runs) = active_runs.lock() {
                runs.remove(&run_id);
                active_runs_ready.notify_all();
            }
            if let Some(registry) = registry.upgrade() {
                let _ = registry.record_run_released(&run_id);
            }
        });
        Ok(())
    }
}

impl HttpRunDriver for HttpProductionRunDriver {
    fn requires_run_release_barrier(&self) -> bool {
        true
    }

    fn bind_session(
        &self,
        session_id: &str,
        model_name: Option<&str>,
    ) -> Result<HttpSessionBinding, HttpRunDriverError> {
        let binding = bind_application_session_with_model(
            &self.options.config_path,
            &self.options.launch_cwd,
            None,
            model_name,
        )
        .map_err(|error| {
            HttpRunDriverError::new(format!(
                "failed to bind durable session for {session_id}: {error}"
            ))
        })?;
        Ok(HttpSessionBinding {
            session_scope_id: binding.session_scope_id,
            session_log_path: binding.session_log_path.display().to_string(),
        })
    }

    fn bind_existing_session(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
    ) -> Result<HttpSessionBinding, HttpSessionOpenBindingError> {
        let lifecycle = self
            .options
            .session_lifecycle
            .as_ref()
            .ok_or(HttpSessionOpenBindingError::Unavailable)?;
        let candidate = lifecycle
            .resolve_session_for_reopen(session_ref, expected_session_id)
            .map_err(|error| match error {
                LocalSessionReopenError::NotFound => HttpSessionOpenBindingError::NotFound,
                LocalSessionReopenError::NotReady { .. } => HttpSessionOpenBindingError::NotReady,
                LocalSessionReopenError::IdentityChanged => {
                    HttpSessionOpenBindingError::IdentityChanged
                }
                LocalSessionReopenError::CatalogUnavailable { .. } => {
                    HttpSessionOpenBindingError::Unavailable
                }
            })?;
        let binding = bind_existing_application_session(
            &self.options.config_path,
            &candidate.session_log_path,
        )
        .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
        if binding.session_scope_id != candidate.session_id
            || binding.session_scope_id != expected_session_id
            || binding.session_log_path != candidate.session_log_path
        {
            return Err(HttpSessionOpenBindingError::IdentityChanged);
        }
        Ok(HttpSessionBinding {
            session_scope_id: binding.session_scope_id,
            session_log_path: binding.session_log_path.display().to_string(),
        })
    }

    fn purge_session_local_state(&self, durable_session_scope_id: &str) {
        let mut exact_prompts = self
            .exact_queue_prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        exact_prompts.retain(|key, _| key.session_scope_id != durable_session_scope_id);
    }

    fn session_frontier(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<crate::HttpDurableSessionFrontier, HttpRunDriverError> {
        let frontier = application_session_frontier_view(
            std::path::Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable session frontier is unavailable"))?;
        Ok(crate::HttpDurableSessionFrontier {
            through_stream_sequence: frontier.through_stream_sequence,
        })
    }

    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError> {
        self.start_supervised_run(start, None)
    }

    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&cancel.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", cancel.run_id))
        })?;
        if run.session_id != cancel.session_id {
            return Err(HttpRunDriverError::new(
                "production cancel session mismatch",
            ));
        }
        let (acknowledgement, acknowledged) = std_mpsc::sync_channel(1);
        run.cancel_sender
            .send(HttpProductionCancellationCommand {
                reason: cancel
                    .reason
                    .unwrap_or_else(|| "HTTP client requested cancellation".to_owned()),
                acknowledgement,
            })
            .map_err(|_| HttpRunDriverError::new("production cancellation owner is closed"))?;
        acknowledged.recv().map_err(|_| {
            HttpRunDriverError::new(
                "production cancellation owner stopped before durable acknowledgement",
            )
        })?
    }

    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError> {
        if approval.call_id != approval.decision.call_id
            || approval.run_id != approval.decision.run_id
        {
            return Err(HttpRunDriverError::new(
                "production approval decision identity mismatch",
            ));
        }
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&approval.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", approval.run_id))
        })?;
        if run.session_id != approval.session_id {
            return Err(HttpRunDriverError::new(
                "production approval session mismatch",
            ));
        }
        run.broker.resolve(&approval.call_id, approval.decision)
    }

    fn verification_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<HttpVerificationView>, HttpRunDriverError> {
        application_verification_view(Path::new(&session.session_log_path)).map_err(|error| {
            HttpRunDriverError::new(format!("failed to project verification state: {error}"))
        })
    }

    fn transcript_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        before: Option<u64>,
        limit: usize,
    ) -> Result<HttpSessionTranscriptPage, HttpRunDriverError> {
        let page = application_session_transcript_page(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            before,
            limit,
        )
        .map_err(|_| HttpRunDriverError::new("durable transcript projection failed"))?;
        Ok(HttpSessionTranscriptPage {
            session_scope_id: page.session_scope_id,
            total_messages: page.total_messages,
            messages: page
                .messages
                .into_iter()
                .map(|message| HttpSessionTranscriptMessage {
                    ordinal: message.ordinal,
                    message_id: message.message_id,
                    role: match message.role {
                        ApplicationTranscriptRole::User => HttpTranscriptRole::User,
                        ApplicationTranscriptRole::Assistant => HttpTranscriptRole::Assistant,
                        ApplicationTranscriptRole::Tool => HttpTranscriptRole::Tool,
                    },
                    content: message.content,
                    assistant_kind: message.assistant_kind.map(|kind| match kind {
                        sigil_kernel::AssistantMessageKind::ToolPreamble => {
                            HttpTranscriptAssistantKind::ToolPreamble
                        }
                        sigil_kernel::AssistantMessageKind::Progress => {
                            HttpTranscriptAssistantKind::Progress
                        }
                        sigil_kernel::AssistantMessageKind::ReasoningTrace => {
                            HttpTranscriptAssistantKind::ReasoningTrace
                        }
                        sigil_kernel::AssistantMessageKind::FinalAnswer => {
                            HttpTranscriptAssistantKind::FinalAnswer
                        }
                    }),
                    tool_name: message.tool_name,
                    image_attachment_count: message.image_attachment_count,
                    truncated: message.truncated,
                    original_content_bytes: message.original_content_bytes,
                })
                .collect(),
            next_before: page.next_before,
        })
    }

    fn conversation_display_page(
        &self,
        session: &crate::HttpSessionSnapshot,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<HttpConversationDisplayPage, HttpConversationDisplayDriverError> {
        let page = conversation_display_page(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
            cursor,
            limit,
        )
        .map_err(|error| match error {
            ConversationDisplayProjectionError::InvalidCursor { .. } => {
                HttpConversationDisplayDriverError::InvalidCursor
            }
            ConversationDisplayProjectionError::StaleCursor { .. } => {
                HttpConversationDisplayDriverError::StaleCursor
            }
            ConversationDisplayProjectionError::Unavailable { .. } => {
                HttpConversationDisplayDriverError::Unavailable
            }
        })?;
        let mut page = HttpConversationDisplayPage::from_runtime(&session.id, page);
        if let Some(run_id) = session.foreground_run_id.as_deref() {
            let run_sequence = self
                .event_bus
                .latest_run_sequence(&session.durable_session_scope_id, run_id)
                .map_err(|_| HttpConversationDisplayDriverError::Unavailable)?
                .unwrap_or(0);
            page.live_provisional_anchor = Some(crate::HttpConversationLiveProvisionalAnchor {
                durable_frontier: page.through_session_stream_sequence.clone(),
                run_id: run_id.to_owned(),
                run_sequence: run_sequence.to_string(),
            });
        }
        Ok(page)
    }

    fn run_context_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpRunContextView, HttpRunDriverError> {
        let view = application_run_context_view(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable run-context projection failed"))?;
        Ok(HttpRunContextView {
            provider_name: view.provider_name,
            model_name: view.model_name,
            available_models: view.available_models,
            model_options: view
                .model_options
                .into_iter()
                .map(|option| HttpApplicationModelOption {
                    model_name: option.model_name,
                    available_reasoning_efforts: option
                        .available_reasoning_efforts
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                    default_reasoning_effort: option.default_reasoning_effort.map(Into::into),
                    reasoning_effort_binding: option.reasoning_effort_binding,
                })
                .collect(),
            model_selection: HttpModelSelectionPolicy::PerRun,
            model_selection_binding: view.model_selection_binding,
            default_permission_mode: view.default_permission_mode.into(),
            available_permission_modes: vec![
                HttpPermissionMode::ReadOnly,
                HttpPermissionMode::Manual,
                HttpPermissionMode::AutoEdit,
                HttpPermissionMode::DangerFullAccess,
            ],
            available_reasoning_efforts: view
                .available_reasoning_efforts
                .into_iter()
                .map(Into::into)
                .collect(),
            default_reasoning_effort: view.default_reasoning_effort.map(Into::into),
            reasoning_effort_binding: view.reasoning_effort_binding,
            context_window_tokens: view.context_window_tokens,
            last_prompt_tokens: view.last_prompt_tokens,
            context_window_source: match view.context_window_source {
                sigil_runtime::ContextWindowSource::Provider => HttpContextWindowSource::Provider,
                sigil_runtime::ContextWindowSource::Config => HttpContextWindowSource::Config,
                sigil_runtime::ContextWindowSource::None => HttpContextWindowSource::Unavailable,
            },
            extension_catalog: HttpApplicationExtensionCatalog {
                commands: view
                    .extension_catalog
                    .commands
                    .into_iter()
                    .map(|entry| HttpApplicationCommandCatalogEntry {
                        canonical: entry.canonical,
                        aliases: entry.aliases,
                        label: entry.label,
                        description: entry.description,
                        argument_hint: entry.argument_hint,
                        completes_with_space: entry.completes_with_space,
                        client_action: entry.client_action.map(|action| match action {
                            sigil_runtime::ApplicationClientAction::NewSession => {
                                HttpApplicationClientAction::NewSession
                            }
                            sigil_runtime::ApplicationClientAction::FocusEffort => {
                                HttpApplicationClientAction::FocusEffort
                            }
                            sigil_runtime::ApplicationClientAction::FocusModel => {
                                HttpApplicationClientAction::FocusModel
                            }
                            sigil_runtime::ApplicationClientAction::OpenSessionPicker => {
                                HttpApplicationClientAction::OpenSessionPicker
                            }
                            sigil_runtime::ApplicationClientAction::OpenAgentWorkbench => {
                                HttpApplicationClientAction::OpenAgentWorkbench
                            }
                            sigil_runtime::ApplicationClientAction::OpenSettings => {
                                HttpApplicationClientAction::OpenSettings
                            }
                            sigil_runtime::ApplicationClientAction::OpenSupport => {
                                HttpApplicationClientAction::OpenSupport
                            }
                        }),
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                    })
                    .collect(),
                skills: view
                    .extension_catalog
                    .skills
                    .into_iter()
                    .map(|entry| HttpApplicationSkillCatalogEntry {
                        id: entry.id,
                        invocation_token: entry.invocation_token,
                        name: entry.name,
                        description: entry.description,
                        source: entry.source,
                        run_mode: entry.run_mode,
                        trust: entry.trust,
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                        binding: entry.binding.map(|binding| HttpApplicationSkillBinding {
                            skill_id: binding.skill_id,
                            skill_sha256: binding.skill_sha256,
                            index_fingerprint: binding.index_fingerprint,
                        }),
                    })
                    .collect(),
                agents: view
                    .extension_catalog
                    .agents
                    .into_iter()
                    .map(|entry| HttpApplicationAgentCatalogEntry {
                        id: entry.id,
                        invocation_token: entry.invocation_token,
                        description: entry.description,
                        source: entry.source,
                        kind: entry.kind,
                        trust: entry.trust,
                        enabled: entry.enabled,
                        user_invocable: entry.user_invocable,
                        available: entry.available,
                        unavailable_reason: entry.unavailable_reason,
                        snapshot_id: entry.snapshot_id,
                        binding: entry
                            .binding
                            .map(|binding| crate::HttpApplicationAgentBinding {
                                profile_id: binding.profile_id,
                                snapshot_id: binding.snapshot_id,
                            }),
                    })
                    .collect(),
            },
        })
    }

    fn agent_activity_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<HttpAgentActivityView, HttpRunDriverError> {
        let view = application_agent_activity_view(
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpRunDriverError::new("durable agent activity projection failed"))?;
        Ok(HttpAgentActivityView {
            total_agents: view.total_agents,
            active_agents: view.active_agents,
            terminal_agents: view.terminal_agents,
            items: view
                .items
                .into_iter()
                .map(|item| HttpAgentActivityItem {
                    thread_id: item.thread_id,
                    profile_id: item.profile_id,
                    display_name: item.display_name,
                    objective: item.objective,
                    status: match item.status {
                        sigil_runtime::ApplicationAgentActivityStatus::Started => {
                            HttpAgentActivityStatus::Started
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Running => {
                            HttpAgentActivityStatus::Running
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Blocked => {
                            HttpAgentActivityStatus::Blocked
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Completed => {
                            HttpAgentActivityStatus::Completed
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Failed => {
                            HttpAgentActivityStatus::Failed
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Cancelled => {
                            HttpAgentActivityStatus::Cancelled
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Interrupted => {
                            HttpAgentActivityStatus::Interrupted
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Unavailable => {
                            HttpAgentActivityStatus::Unavailable
                        }
                        sigil_runtime::ApplicationAgentActivityStatus::Unknown => {
                            HttpAgentActivityStatus::Unknown
                        }
                    },
                    reason: item.reason,
                    handoff_status: match item.handoff_status {
                        sigil_runtime::ApplicationAgentHandoffStatus::Pending => {
                            HttpAgentHandoffStatus::Pending
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::ResultReady => {
                            HttpAgentHandoffStatus::ResultReady
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::ResultRead => {
                            HttpAgentHandoffStatus::ResultRead
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::Returned => {
                            HttpAgentHandoffStatus::Returned
                        }
                        sigil_runtime::ApplicationAgentHandoffStatus::Unavailable => {
                            HttpAgentHandoffStatus::Unavailable
                        }
                    },
                    result_summary: item.result_summary,
                    result_summary_truncated: item.result_summary_truncated,
                    usage: item.usage.map(|usage| HttpAgentUsageSummary {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        total_tokens: usage.total_tokens,
                        cached_tokens: usage.cached_tokens,
                    }),
                })
                .collect(),
        })
    }

    fn conversation_queue_view(
        &self,
        session: &crate::HttpSessionSnapshot,
        foreground_owner: Option<&crate::HttpForegroundRunOwner>,
    ) -> Result<HttpConversationQueueView, HttpConversationQueueDriverError> {
        let state = read_http_durable_queue_state(session)?;
        let exact_prompts = self
            .exact_queue_prompts
            .lock()
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        Ok(http_conversation_queue_view(
            session,
            foreground_owner,
            &state,
            &exact_prompts,
        ))
    }

    fn mutate_conversation_queue(
        &self,
        session: &crate::HttpSessionSnapshot,
        foreground_owner: Option<&crate::HttpForegroundRunOwner>,
        command: &HttpConversationQueueDriverCommand,
    ) -> Result<HttpConversationQueueView, HttpConversationQueueDriverError> {
        let state = read_http_durable_queue_state(session)?;
        let current_generation = http_queue_generation(state.projection.current_revision());
        if command.request.expected_generation != current_generation {
            return Err(HttpConversationQueueDriverError::StaleGeneration);
        }
        if let HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id,
            foreground_owner_revision,
        } = &command.request.action
        {
            let owner = foreground_owner.ok_or(HttpConversationQueueDriverError::OwnerLost)?;
            if owner.run_id != *foreground_run_id
                || owner.owner_revision != *foreground_owner_revision
            {
                return Err(HttpConversationQueueDriverError::OwnerLost);
            }
            let exact_prompts = self
                .exact_queue_prompts
                .lock()
                .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            validate_http_interrupt_candidate(session, &state, &exact_prompts)?;
            return Ok(http_conversation_queue_view(
                session,
                foreground_owner,
                &state,
                &exact_prompts,
            ));
        }

        let now_ms = current_unix_time_ms();
        let expected_queue_revision = state.projection.current_revision();
        let mut cache_update = None;
        let mutation = match &command.request.action {
            HttpConversationQueueCommandAction::Enqueue {
                prompt,
                kind,
                reasoning_effort,
            } => {
                let queue_id = stable_http_queue_id(
                    &session.durable_session_scope_id,
                    &command.client_id,
                    &command.command_id,
                )?;
                let projection = project_conversation_prompt_for_persistence(prompt);
                cache_update = Some(HttpExactQueueCacheUpdate::Replace {
                    key: exact_queue_prompt_key(session, queue_id.clone()),
                    prompt_hash: projection.prompt_hash.clone(),
                    exact_prompt: projection
                        .exact_prompt_required
                        .then(|| SecretString::new(prompt.clone())),
                });
                ConversationQueueMutation::Enqueue {
                    entry: ConversationInputQueuedEntry {
                        queue_id,
                        target: ConversationInputTarget::MainThread,
                        kind: http_queue_kind_to_kernel(*kind),
                        prompt_hash: projection.prompt_hash,
                        prompt: projection.safe_prompt,
                        reasoning_effort: reasoning_effort.map(Into::into),
                        created_at_ms: Some(now_ms),
                    },
                }
            }
            HttpConversationQueueCommandAction::Edit {
                entry_id,
                prompt,
                reasoning_effort,
            } => {
                let queue_id = ConversationInputQueueId::new(entry_id.clone())
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                ensure_http_queue_item_mutable(&state.projection, &queue_id)?;
                let projection = project_conversation_prompt_for_persistence(prompt);
                cache_update = Some(HttpExactQueueCacheUpdate::Replace {
                    key: exact_queue_prompt_key(session, queue_id.clone()),
                    prompt_hash: projection.prompt_hash.clone(),
                    exact_prompt: projection
                        .exact_prompt_required
                        .then(|| SecretString::new(prompt.clone())),
                });
                ConversationQueueMutation::Edit {
                    entry: ConversationInputEditedEntry {
                        queue_id,
                        prompt_hash: projection.prompt_hash,
                        prompt: projection.safe_prompt,
                        reasoning_effort: reasoning_effort.map(Into::into),
                        updated_at_ms: Some(now_ms),
                    },
                }
            }
            HttpConversationQueueCommandAction::Remove { entry_id } => {
                let queue_id = ConversationInputQueueId::new(entry_id.clone())
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                ensure_http_queue_item_mutable(&state.projection, &queue_id)?;
                cache_update = Some(HttpExactQueueCacheUpdate::Remove(exact_queue_prompt_key(
                    session,
                    queue_id.clone(),
                )));
                ConversationQueueMutation::Remove {
                    queue_id,
                    reason: Some("removed by application queue command".to_owned()),
                    updated_at_ms: Some(now_ms),
                }
            }
            HttpConversationQueueCommandAction::Reorder {
                entry_id,
                after_entry_id,
            } => {
                let queue_id = ConversationInputQueueId::new(entry_id.clone())
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                ensure_http_queue_item_mutable(&state.projection, &queue_id)?;
                let after_queue_id = after_entry_id
                    .as_ref()
                    .map(|entry_id| ConversationInputQueueId::new(entry_id.clone()))
                    .transpose()
                    .map_err(|_| HttpConversationQueueDriverError::Conflict)?;
                if let Some(after_queue_id) = after_queue_id.as_ref() {
                    ensure_http_queue_item_mutable(&state.projection, after_queue_id)?;
                }
                ConversationQueueMutation::Reorder {
                    entry: ConversationInputReorderedEntry {
                        queue_id,
                        after_queue_id,
                        updated_at_ms: Some(now_ms),
                    },
                }
            }
            HttpConversationQueueCommandAction::Pause => ConversationQueueMutation::Pause {
                reason: Some("paused by application queue command".to_owned()),
                updated_at_ms: Some(now_ms),
            },
            HttpConversationQueueCommandAction::Resume => ConversationQueueMutation::Resume {
                reason: Some("resumed by application queue command".to_owned()),
                updated_at_ms: Some(now_ms),
            },
            HttpConversationQueueCommandAction::InterruptAndRunNext { .. } => {
                unreachable!("interrupt action returned after exact owner validation")
            }
        };

        let mut exact_prompts = self
            .exact_queue_prompts
            .lock()
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        validate_http_exact_queue_cache_capacity(&exact_prompts, cache_update.as_ref())?;
        let store = JsonlSessionStore::new(&session.session_log_path)
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        if store
            .append_conversation_queue_mutation(ConversationQueueMutationCommand {
                expected_queue_revision,
                mutation,
            })
            .is_err()
        {
            let latest = read_http_durable_queue_state(session)?;
            return if http_queue_generation(latest.projection.current_revision())
                != current_generation
            {
                Err(HttpConversationQueueDriverError::StaleGeneration)
            } else {
                Err(HttpConversationQueueDriverError::Conflict)
            };
        }
        apply_http_exact_queue_cache_update(&mut exact_prompts, cache_update);
        let state = read_http_durable_queue_state(session)?;
        Ok(http_conversation_queue_view(
            session,
            foreground_owner,
            &state,
            &exact_prompts,
        ))
    }

    fn next_queued_run_admission(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<HttpQueuedRunAdmission>, HttpConversationQueueDriverError> {
        self.reconcile_orphaned_queued_dispatches(session)?;
        let state = read_http_durable_queue_state(session)?;
        if state
            .projection
            .queue
            .items
            .iter()
            .any(|item| item.status == ConversationInputStatus::Dispatching)
        {
            return Ok(None);
        }
        let exact_prompts = self
            .exact_queue_prompts
            .lock()
            .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        let view = http_conversation_queue_view(session, None, &state, &exact_prompts);
        let Some(entry_id) = view.next_dispatchable_entry_id.as_deref() else {
            return Ok(None);
        };
        let item = state
            .projection
            .queue
            .items
            .iter()
            .find(|item| item.queued.queue_id.as_str() == entry_id)
            .ok_or(HttpConversationQueueDriverError::Conflict)?;
        if item.status != ConversationInputStatus::Queued
            || item.queued.kind != ConversationInputKind::Chat
            || item.queued.target != ConversationInputTarget::MainThread
        {
            return Ok(None);
        }
        let context = application_run_context_view(
            &self.options.config_path,
            &self.options.launch_cwd,
            Path::new(&session.session_log_path),
            &session.durable_session_scope_id,
        )
        .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
        let dispatch_run_id = stable_http_queued_dispatch_run_id(
            &session.durable_session_scope_id,
            &item.queued.queue_id,
            &state.projection.current_revision(),
        );
        let prompt_preview = view
            .items
            .iter()
            .find(|row| row.entry_id == entry_id)
            .map(|row| row.prompt_preview.clone())
            .ok_or(HttpConversationQueueDriverError::Conflict)?;
        Ok(Some(HttpQueuedRunAdmission {
            entry_id: entry_id.to_owned(),
            generation: view.generation,
            dispatch_run_id,
            prompt_preview,
            permission_mode: context.default_permission_mode.into(),
            reasoning_effort: item.queued.reasoning_effort.clone().map(Into::into),
        }))
    }

    fn start_queued_run(&self, start: HttpQueuedRunDriverStart) -> Result<(), HttpRunDriverError> {
        let (start, queued) = self.queued_supervisor_start(start)?;
        self.start_supervised_run(start, Some(queued))
    }

    fn wait_for_run_release(
        &self,
        run_id: &str,
        timeout: Duration,
    ) -> Result<(), HttpRunDriverError> {
        let deadline = Instant::now() + timeout;
        let mut runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        while runs.contains_key(run_id) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HttpRunDriverError::new(format!(
                    "production run cleanup timed out: {run_id}"
                )));
            }
            let (next, wait) = self
                .active_runs_ready
                .wait_timeout(runs, remaining)
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            runs = next;
            if wait.timed_out() && runs.contains_key(run_id) {
                return Err(HttpRunDriverError::new(format!(
                    "production run cleanup timed out: {run_id}"
                )));
            }
        }
        Ok(())
    }

    fn rerun_verification(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpVerificationRerunRequest,
    ) -> Result<HttpVerificationView, HttpRunDriverError> {
        self.runtime
            .block_on(rerun_application_verification(
                &self.options.config_path,
                &self.options.launch_cwd,
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &self.services,
                request,
            ))
            .map_err(|error| HttpRunDriverError::new(format!("verification rerun failed: {error}")))
    }

    fn wait_for_idle(&self, timeout: Duration) -> Result<(), HttpRunDriverError> {
        let deadline = Instant::now() + timeout;
        let mut runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        while !runs.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HttpRunDriverError::new(format!(
                    "production shutdown timed out with {} owned run supervisor(s)",
                    runs.len()
                )));
            }
            let (next, wait) = self
                .active_runs_ready
                .wait_timeout(runs, remaining)
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            runs = next;
            if wait.timed_out() && !runs.is_empty() {
                return Err(HttpRunDriverError::new(format!(
                    "production shutdown timed out with {} owned run supervisor(s)",
                    runs.len()
                )));
            }
        }
        Ok(())
    }
}

struct HttpDurableQueueState {
    projection: ConversationQueueDurableProjection,
    updated_at_ms: BTreeMap<ConversationInputQueueId, u64>,
}

enum HttpExactQueueCacheUpdate {
    Replace {
        key: HttpExactQueuePromptKey,
        prompt_hash: String,
        exact_prompt: Option<SecretString>,
    },
    Remove(HttpExactQueuePromptKey),
}

fn read_http_durable_queue_state(
    session: &crate::HttpSessionSnapshot,
) -> Result<HttpDurableQueueState, HttpConversationQueueDriverError> {
    let records = JsonlSessionStore::read_event_records(&session.session_log_path)
        .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
    if records
        .iter()
        .any(|record| record.session_id() != session.durable_session_scope_id)
    {
        return Err(HttpConversationQueueDriverError::Unavailable);
    }
    let projection = ConversationQueueDurableProjection::from_records(&records)
        .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
    let mut updated_at_ms = BTreeMap::new();
    for record in records {
        let Some(value) = record
            .stored_event()
            .payload
            .get("session_log_entry")
            .cloned()
        else {
            continue;
        };
        let Ok(SessionLogEntry::Control(control)) = serde_json::from_value(value) else {
            continue;
        };
        let update = match control {
            ControlEntry::ConversationInputQueued(entry) => {
                entry.created_at_ms.map(|time| (entry.queue_id, time))
            }
            ControlEntry::ConversationInputEdited(entry) => {
                entry.updated_at_ms.map(|time| (entry.queue_id, time))
            }
            ControlEntry::ConversationInputReordered(entry) => {
                entry.updated_at_ms.map(|time| (entry.queue_id, time))
            }
            ControlEntry::ConversationInputStatusChanged(entry) => {
                entry.updated_at_ms.map(|time| (entry.queue_id, time))
            }
            _ => None,
        };
        if let Some((queue_id, time)) = update {
            updated_at_ms.insert(queue_id, time);
        }
    }
    Ok(HttpDurableQueueState {
        projection,
        updated_at_ms,
    })
}

fn http_conversation_queue_view(
    session: &crate::HttpSessionSnapshot,
    foreground_owner: Option<&crate::HttpForegroundRunOwner>,
    state: &HttpDurableQueueState,
    exact_prompts: &BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
) -> HttpConversationQueueView {
    let next_dispatchable = state.projection.queue.next_dispatchable.as_ref();
    let has_dispatching_frontier = state
        .projection
        .queue
        .items
        .iter()
        .any(|item| item.status == ConversationInputStatus::Dispatching);
    let total_items = state.projection.queue.items.len();
    let items = state
        .projection
        .queue
        .items
        .iter()
        .take(crate::HTTP_MAX_CONVERSATION_QUEUE_ITEMS)
        .enumerate()
        .map(|(index, item)| {
            let key = exact_queue_prompt_key(session, item.queued.queue_id.clone());
            let prompt_material = if item
                .queued
                .prompt_hash
                .starts_with(CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX)
            {
                if exact_prompts
                    .get(&key)
                    .is_some_and(|material| material.prompt_hash == item.queued.prompt_hash)
                {
                    HttpConversationQueuePromptMaterial::AvailableProcessLocal
                } else {
                    HttpConversationQueuePromptMaterial::RequiresReentry
                }
            } else {
                HttpConversationQueuePromptMaterial::PersistedSafe
            };
            let is_supported = item.queued.target == ConversationInputTarget::MainThread
                && item.queued.kind == ConversationInputKind::Chat;
            let is_next = next_dispatchable == Some(&item.queued.queue_id);
            let dispatchable = item.status == ConversationInputStatus::Queued
                && is_supported
                && is_next
                && !has_dispatching_frontier
                && !state.projection.queue.paused
                && foreground_owner.is_none()
                && prompt_material != HttpConversationQueuePromptMaterial::RequiresReentry;
            let blocked_reason = if item.status == ConversationInputStatus::Stale {
                Some(HttpConversationQueueBlockedReason::Stale)
            } else if item.status.is_terminal() {
                Some(HttpConversationQueueBlockedReason::Terminal)
            } else if item.status == ConversationInputStatus::Dispatching {
                Some(HttpConversationQueueBlockedReason::Conflict)
            } else if !is_supported {
                Some(HttpConversationQueueBlockedReason::UnsupportedTarget)
            } else if state.projection.queue.paused {
                Some(HttpConversationQueueBlockedReason::QueuePaused)
            } else if prompt_material == HttpConversationQueuePromptMaterial::RequiresReentry {
                Some(HttpConversationQueueBlockedReason::RequiresReentry)
            } else if item.status == ConversationInputStatus::Queued
                && (has_dispatching_frontier || !is_next)
            {
                Some(HttpConversationQueueBlockedReason::WaitingForTerminalFrontier)
            } else if foreground_owner.is_some() {
                Some(HttpConversationQueueBlockedReason::ForegroundRunActive)
            } else {
                None
            };
            let (prompt_preview, prompt_preview_truncated) =
                http_queue_prompt_preview(&item.queued.prompt);
            HttpConversationQueueItem {
                entry_id: item.queued.queue_id.as_str().to_owned(),
                order: u32::try_from(index).unwrap_or(u32::MAX),
                kind: kernel_queue_kind_to_http(item.queued.kind),
                status: kernel_queue_status_to_http(item.status),
                prompt_preview,
                prompt_preview_truncated,
                prompt_material,
                dispatchable,
                blocked_reason,
                created_at_ms: item.queued.created_at_ms,
                updated_at_ms: state.updated_at_ms.get(&item.queued.queue_id).copied(),
            }
        })
        .collect::<Vec<_>>();
    let next_dispatchable_entry_id = items
        .iter()
        .find(|item| item.dispatchable)
        .map(|item| item.entry_id.clone());
    HttpConversationQueueView {
        schema_version: crate::HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION,
        session_id: session.id.clone(),
        generation: http_queue_generation(state.projection.current_revision()),
        paused: state.projection.queue.paused,
        total_items: u32::try_from(total_items).unwrap_or(u32::MAX),
        items,
        truncated: total_items > crate::HTTP_MAX_CONVERSATION_QUEUE_ITEMS,
        next_dispatchable_entry_id,
    }
}

fn exact_queue_prompt_key(
    session: &crate::HttpSessionSnapshot,
    queue_id: ConversationInputQueueId,
) -> HttpExactQueuePromptKey {
    HttpExactQueuePromptKey {
        session_scope_id: session.durable_session_scope_id.clone(),
        queue_id,
    }
}

fn stable_http_queue_id(
    session_scope_id: &str,
    client_id: &str,
    command_id: &str,
) -> Result<ConversationInputQueueId, HttpConversationQueueDriverError> {
    ConversationInputQueueId::new(stable_event_uuid(
        "sigil-http-conversation-queue-entry",
        &stable_http_identity_seed(&[session_scope_id, client_id, command_id]),
    ))
    .map_err(|_| HttpConversationQueueDriverError::Conflict)
}

fn stable_http_queued_dispatch_run_id(
    session_scope_id: &str,
    queue_id: &ConversationInputQueueId,
    revision: &ConversationQueueRevision,
) -> String {
    stable_event_uuid(
        "sigil-http-conversation-queue-dispatch",
        &stable_http_identity_seed(&[
            session_scope_id,
            queue_id.as_str(),
            &revision.stream_sequence.to_string(),
            &revision.event_id,
        ]),
    )
}

fn stable_http_identity_seed(parts: &[&str]) -> String {
    use std::fmt::Write as _;

    let mut seed = String::new();
    for part in parts {
        write!(&mut seed, "{}:{part}", part.len())
            .expect("writing a stable identity seed into String cannot fail");
    }
    seed
}

fn http_queue_generation(revision: ConversationQueueRevision) -> HttpConversationQueueGeneration {
    let mut hasher = Sha256::new();
    for part in [
        revision.stream_sequence.to_be_bytes().as_slice(),
        revision.event_id.as_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    HttpConversationQueueGeneration(format!("queue-v1:{:x}", hasher.finalize()))
}

fn http_queue_prompt_preview(prompt: &str) -> (String, bool) {
    let truncated = prompt.chars().count() > MAX_HTTP_QUEUE_PREVIEW_CHARS;
    if !truncated {
        return (prompt.to_owned(), false);
    }
    let preview = prompt
        .chars()
        .take(MAX_HTTP_QUEUE_PREVIEW_CHARS.saturating_sub(3))
        .collect::<String>();
    (format!("{preview}..."), true)
}

fn http_queue_kind_to_kernel(kind: HttpConversationQueueItemKind) -> ConversationInputKind {
    match kind {
        HttpConversationQueueItemKind::Chat => ConversationInputKind::Chat,
        HttpConversationQueueItemKind::PlanPrompt => ConversationInputKind::PlanPrompt,
        HttpConversationQueueItemKind::AgentMention => ConversationInputKind::AgentMention,
        HttpConversationQueueItemKind::AgentMessage => ConversationInputKind::AgentMessage,
        HttpConversationQueueItemKind::Unknown => ConversationInputKind::Unknown,
    }
}

fn kernel_queue_kind_to_http(kind: ConversationInputKind) -> HttpConversationQueueItemKind {
    match kind {
        ConversationInputKind::Chat => HttpConversationQueueItemKind::Chat,
        ConversationInputKind::PlanPrompt => HttpConversationQueueItemKind::PlanPrompt,
        ConversationInputKind::AgentMention => HttpConversationQueueItemKind::AgentMention,
        ConversationInputKind::AgentMessage => HttpConversationQueueItemKind::AgentMessage,
        ConversationInputKind::Unknown => HttpConversationQueueItemKind::Unknown,
    }
}

fn kernel_queue_status_to_http(status: ConversationInputStatus) -> HttpConversationQueueItemStatus {
    match status {
        ConversationInputStatus::Queued => HttpConversationQueueItemStatus::Queued,
        ConversationInputStatus::Dispatching => HttpConversationQueueItemStatus::Dispatching,
        ConversationInputStatus::Delivered => HttpConversationQueueItemStatus::Delivered,
        ConversationInputStatus::Rejected => HttpConversationQueueItemStatus::Rejected,
        ConversationInputStatus::Cancelled => HttpConversationQueueItemStatus::Cancelled,
        ConversationInputStatus::Stale => HttpConversationQueueItemStatus::Stale,
        ConversationInputStatus::Unknown => HttpConversationQueueItemStatus::Unknown,
    }
}

fn ensure_http_queue_item_mutable(
    projection: &ConversationQueueDurableProjection,
    queue_id: &ConversationInputQueueId,
) -> Result<(), HttpConversationQueueDriverError> {
    let Some(item) = projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
    else {
        return if projection.is_terminal_queue_id(queue_id) {
            Err(HttpConversationQueueDriverError::Terminal)
        } else {
            Err(HttpConversationQueueDriverError::Conflict)
        };
    };
    if item.status.is_terminal() {
        return Err(HttpConversationQueueDriverError::Terminal);
    }
    if item.status != ConversationInputStatus::Queued {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    Ok(())
}

fn validate_http_interrupt_candidate(
    session: &crate::HttpSessionSnapshot,
    state: &HttpDurableQueueState,
    exact_prompts: &BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
) -> Result<(), HttpConversationQueueDriverError> {
    if state.projection.queue.paused {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    let queue_id = state
        .projection
        .queue
        .next_dispatchable
        .as_ref()
        .ok_or(HttpConversationQueueDriverError::Conflict)?;
    let item = state
        .projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == *queue_id)
        .ok_or(HttpConversationQueueDriverError::Conflict)?;
    if item.status != ConversationInputStatus::Queued {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    if item.queued.target != ConversationInputTarget::MainThread
        || item.queued.kind != ConversationInputKind::Chat
    {
        return Err(HttpConversationQueueDriverError::Unsupported);
    }
    if item
        .queued
        .prompt_hash
        .starts_with(CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX)
    {
        let key = exact_queue_prompt_key(session, queue_id.clone());
        if exact_prompts
            .get(&key)
            .is_none_or(|material| material.prompt_hash != item.queued.prompt_hash)
        {
            return Err(HttpConversationQueueDriverError::RequiresReentry);
        }
    }
    Ok(())
}

fn validate_http_exact_queue_cache_capacity(
    cache: &BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
    update: Option<&HttpExactQueueCacheUpdate>,
) -> Result<(), HttpConversationQueueDriverError> {
    let Some(HttpExactQueueCacheUpdate::Replace {
        key,
        exact_prompt: Some(_),
        ..
    }) = update
    else {
        return Ok(());
    };
    if !cache.contains_key(key) && cache.len() >= MAX_HTTP_EXACT_QUEUE_PROMPTS {
        return Err(HttpConversationQueueDriverError::Conflict);
    }
    Ok(())
}

fn apply_http_exact_queue_cache_update(
    cache: &mut BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>,
    update: Option<HttpExactQueueCacheUpdate>,
) {
    match update {
        Some(HttpExactQueueCacheUpdate::Replace {
            key,
            prompt_hash,
            exact_prompt: Some(exact_prompt),
        }) => {
            cache.insert(
                key,
                HttpExactQueuePrompt {
                    prompt_hash,
                    exact_prompt,
                },
            );
        }
        Some(HttpExactQueueCacheUpdate::Replace {
            key,
            exact_prompt: None,
            ..
        })
        | Some(HttpExactQueueCacheUpdate::Remove(key)) => {
            cache.remove(&key);
        }
        None => {}
    }
}

fn evict_http_promoted_exact_prompt(
    session: &crate::HttpSessionSnapshot,
    queued: Option<&HttpQueuedRunTerminalContext>,
    exact_queue_prompts: &Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>,
) -> Result<(), HttpRunDriverError> {
    let Some(queued) = queued else {
        return Ok(());
    };
    let state = read_http_durable_queue_state(session)
        .map_err(|_| HttpRunDriverError::new("durable queued promotion state is unavailable"))?;
    let still_queued = state
        .projection
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == queued.queue_id)
        .is_some_and(|item| item.status == ConversationInputStatus::Queued);
    if still_queued {
        return Ok(());
    }
    exact_queue_prompts
        .lock()
        .map_err(|_| HttpRunDriverError::new("queued exact prompt state is unavailable"))?
        .remove(&queued.exact_prompt_key);
    Ok(())
}

fn finalize_http_queued_terminal(
    session: &crate::HttpSessionSnapshot,
    queued: &HttpQueuedRunTerminalContext,
    unpromoted_terminal: HttpQueuedUnpromotedTerminal,
) -> Result<(), HttpRunDriverError> {
    let records = JsonlSessionStore::read_event_records(&session.session_log_path)
        .map_err(|_| HttpRunDriverError::new("queued terminal evidence is unavailable"))?;
    if records
        .iter()
        .any(|record| record.session_id() != session.durable_session_scope_id)
    {
        return Err(HttpRunDriverError::new(
            "queued terminal evidence belongs to another durable session",
        ));
    }
    let queue = ConversationQueueDurableProjection::from_records(&records)
        .map_err(|_| HttpRunDriverError::new("durable queued terminal state is invalid"))?;
    let Some(item) = queue
        .queue
        .items
        .iter()
        .find(|item| item.queued.queue_id == queued.queue_id)
    else {
        return Ok(());
    };
    let unpromoted = item.status == ConversationInputStatus::Queued;
    if !unpromoted && item.status != ConversationInputStatus::Dispatching {
        return Ok(());
    }

    let (expectation, status, reason) = if unpromoted {
        let (status, reason) = match unpromoted_terminal {
            HttpQueuedUnpromotedTerminal::Rejected => (
                ConversationInputStatus::Rejected,
                Some("queued run preparation ended before durable promotion".to_owned()),
            ),
            HttpQueuedUnpromotedTerminal::Cancelled => (
                ConversationInputStatus::Cancelled,
                Some("queued run was cancelled before durable promotion".to_owned()),
            ),
        };
        (
            ConversationInputTerminalExpectation::Queued {
                expected_queue_revision: queued.expected_queue_revision.clone(),
                queue_id: queued.queue_id.clone(),
                expected_prompt_hash: queued.prompt_hash.clone(),
            },
            status,
            reason,
        )
    } else {
        let Some(promotion) = http_queued_promotion(&records, &queued.queue_id) else {
            return Ok(());
        };
        if promotion.dispatch_run_id != queued.dispatch_run_id {
            return Ok(());
        }
        let (status, reason) =
            http_queued_terminal_from_attempt_evidence(&records, &queued.dispatch_run_id)?;
        let expected_frontier = records
            .last()
            .map(ConversationInputTerminalFrontier::from_record)
            .ok_or_else(|| HttpRunDriverError::new("queued terminal frontier is unavailable"))?;
        (
            ConversationInputTerminalExpectation::Promoted {
                queue_id: queued.queue_id.clone(),
                dispatch_run_id: queued.dispatch_run_id.clone(),
                expected_frontier,
            },
            status,
            reason,
        )
    };
    let store = JsonlSessionStore::new(&session.session_log_path)
        .map_err(|_| HttpRunDriverError::new("queued terminal store is unavailable"))?;
    store
        .append_conversation_input_terminal_if_current(ConversationInputTerminalCommand {
            expectation,
            terminal: ConversationInputStatusEntry {
                queue_id: queued.queue_id.clone(),
                status,
                reason,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        })
        .map(|_| ())
        .map_err(|_| HttpRunDriverError::new("queued terminal status could not be persisted"))
}

fn http_queued_promotion(
    records: &[sigil_kernel::SessionStreamRecord],
    queue_id: &ConversationInputQueueId,
) -> Option<ConversationInputPromotedEntry> {
    records.iter().rev().find_map(|record| {
        let event = record.stored_event();
        if event.event_kind() != Some(sigil_kernel::DurableEventType::ConversationInputPromoted) {
            return None;
        }
        serde_json::from_value::<ConversationInputPromotedEntry>(event.payload.clone())
            .ok()
            .filter(|promotion| &promotion.queue_id == queue_id)
    })
}

fn http_queued_terminal_from_attempt_evidence(
    records: &[sigil_kernel::SessionStreamRecord],
    dispatch_run_id: &str,
) -> Result<(ConversationInputStatus, Option<String>), HttpRunDriverError> {
    let attempts = ProviderPhysicalAttemptProjection::from_records(records)
        .map_err(|_| HttpRunDriverError::new("queued provider attempt evidence is invalid"))?;
    let attempts = attempts.attempts_for_logical_run_id(dispatch_run_id);
    Ok(match attempts.as_slice() {
        [] => (
            ConversationInputStatus::Rejected,
            Some("queued promotion was not followed by a provider physical attempt".to_owned()),
        ),
        [attempt] => match attempt.terminal.as_ref().map(|entry| entry.outcome) {
            Some(
                ProviderPhysicalAttemptOutcome::Completed
                | ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect
                | ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput,
            ) => (ConversationInputStatus::Delivered, None),
            Some(ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption) => (
                ConversationInputStatus::Rejected,
                Some("queued provider attempt confirmed no model consumption".to_owned()),
            ),
            Some(
                ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain
                | ProviderPhysicalAttemptOutcome::Interrupted,
            ) => (
                ConversationInputStatus::Stale,
                Some(
                    "queued provider outcome is uncertain and will not be replayed automatically"
                        .to_owned(),
                ),
            ),
            None => (
                ConversationInputStatus::Stale,
                Some("queued provider physical attempt has no durable terminal".to_owned()),
            ),
        },
        _ => (
            ConversationInputStatus::Stale,
            Some("queued promotion has multiple provider physical attempts".to_owned()),
        ),
    })
}

struct HttpProductionActiveRun {
    session_id: String,
    broker: Arc<HttpApprovalBroker>,
    cancel_sender: mpsc::UnboundedSender<HttpProductionCancellationCommand>,
}

struct HttpProductionCancellationCommand {
    reason: String,
    acknowledgement: std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
}

struct HttpRunSupervisor {
    options: HttpProductionRunDriverOptions,
    services: ApplicationRunServices,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
    start: HttpRunDriverStart,
    queued: Option<HttpQueuedRunPreparation>,
    exact_queue_prompts: Arc<Mutex<BTreeMap<HttpExactQueuePromptKey, HttpExactQueuePrompt>>>,
    cancel_receiver: mpsc::UnboundedReceiver<HttpProductionCancellationCommand>,
}

impl HttpRunSupervisor {
    fn evict_promoted_exact_prompt(
        &self,
        queued: Option<&HttpQueuedRunTerminalContext>,
    ) -> Result<(), HttpRunDriverError> {
        evict_http_promoted_exact_prompt(&self.start.session, queued, &self.exact_queue_prompts)
    }

    async fn run(mut self) -> Result<(), HttpRunDriverError> {
        let registry = self.registry.upgrade().ok_or_else(|| {
            HttpRunDriverError::new("production registry closed before run preparation")
        })?;
        let request = ApplicationRunRequest {
            config_path: self.options.config_path.clone(),
            launch_cwd: self.options.launch_cwd.clone(),
            prompt: self.start.prompt.clone(),
            run_id: self.start.run.id.clone(),
            session_path: Some(PathBuf::from(&self.start.session.session_log_path)),
            interaction: ApplicationRunInteraction::ExternallyInteractive,
            permission_mode: Some(self.start.run.permission_mode.into()),
            model_name: self.start.model_name.clone(),
            model_selection_binding: self.start.model_selection_binding.clone(),
            reasoning_effort: self.start.run.reasoning_effort.map(Into::into),
            reasoning_effort_binding: self.start.reasoning_effort_binding.clone(),
            skill_binding: self.start.skill_binding.clone().map(|binding| {
                sigil_runtime::ApplicationSkillBinding {
                    skill_id: binding.skill_id,
                    skill_sha256: binding.skill_sha256,
                    index_fingerprint: binding.index_fingerprint,
                }
            }),
            agent_binding: self.start.agent_binding.clone().map(|binding| {
                sigil_runtime::ApplicationAgentBinding {
                    profile_id: binding.profile_id,
                    snapshot_id: binding.snapshot_id,
                }
            }),
            constraints: None,
        };
        let services = self.services.clone();
        let preparer = Arc::clone(&self.preparer);
        let queued_terminal = self
            .queued
            .as_ref()
            .map(|queued| HttpQueuedRunTerminalContext {
                queue_id: queued.promotion.queue_id.clone(),
                dispatch_run_id: queued.promotion.dispatch_run_id.clone(),
                expected_queue_revision: queued.promotion.expected_queue_revision.clone(),
                prompt_hash: queued.promotion.prompt_hash.clone(),
                exact_prompt_key: queued.exact_prompt_key.clone(),
            });
        let mut preparation = match self.queued.take() {
            Some(queued) => preparer.prepare_queued(
                ApplicationQueuedRunRequest {
                    run: request,
                    durable_queue: queued.durable_queue,
                    promotion: queued.promotion,
                    prompt_material: queued.prompt_material,
                    capability_registrations: queued.capability_registrations,
                },
                services,
            ),
            None => preparer.prepare(request, services),
        };
        let preparation_outcome = tokio::select! {
            biased;
            result = &mut preparation => Ok(result),
            cancellation = self.cancel_receiver.recv() => Err(cancellation),
        };
        let preparation_result = match preparation_outcome {
            Ok(result) => {
                drop(preparation);
                result
            }
            Err(Some(cancellation)) => {
                let deadline = cancellation_deadline(self.options.cancellation_timeout);
                let joined =
                    tokio::time::timeout(remaining_until(deadline), &mut preparation).await;
                let preparation_result = match joined {
                    Ok(result) => result,
                    Err(_) => {
                        let error = HttpRunDriverError::new(
                            "production preparation did not quiesce before the cancellation deadline",
                        );
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &cancellation.acknowledgement,
                            error,
                        );
                        let _ = preparation.await;
                        self.evict_promoted_exact_prompt(queued_terminal.as_ref())?;
                        return Err(error);
                    }
                };
                drop(preparation);
                self.evict_promoted_exact_prompt(queued_terminal.as_ref())?;
                return match preparation_result {
                    Ok(prepared) => {
                        self.cancel_prepared_before_execution(
                            &registry,
                            cancellation,
                            prepared,
                            deadline,
                        )
                        .await
                    }
                    Err(_) => {
                        self.cancel_after_failed_preparation(&registry, cancellation, deadline)
                            .await
                    }
                };
            }
            Err(None) => {
                return Err(HttpRunDriverError::new(
                    "production cancellation owner closed during run preparation",
                ));
            }
        };
        self.evict_promoted_exact_prompt(queued_terminal.as_ref())?;
        let prepared = match preparation_result {
            Ok(prepared) => prepared,
            Err(error) => {
                let event = PublicRunEvent::new(
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    1,
                    PublicRunEventKind::RunFailed {
                        error: error.to_string(),
                    },
                );
                let event_bus = Arc::clone(&self.event_bus);
                tokio::task::spawn_blocking(move || event_bus.publish_run_event(event))
                    .await
                    .map_err(|_| {
                        HttpRunDriverError::new(
                            "production preparation terminal publication worker failed",
                        )
                    })?
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                registry
                    .record_run_terminal(&self.start.run.id, HttpRunTerminalOutcome::Failed)
                    .map_err(registry_driver_error)?;
                return Ok(());
            }
        };
        if prepared.session_id() != self.start.session.durable_session_scope_id
            || prepared.session_log_path()
                != PathBuf::from(&self.start.session.session_log_path).as_path()
        {
            return Err(HttpRunDriverError::new(
                "prepared application run does not match its durable HTTP session binding",
            ));
        }
        let (execution, control) = prepared.into_parts();
        let control = Arc::new(control);
        let event_handler = HttpProductionEventHandler {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            run_id: self.start.run.id.clone(),
            approval_timeout: self.options.approval_timeout,
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&self.broker),
            event_bus: Arc::clone(&self.event_bus),
        };
        let approval_handler = HttpProductionApprovalHandler {
            run_id: self.start.run.id.clone(),
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&self.broker),
        };
        let mut execution =
            Box::pin(execution.execute_on_owned_blocking(event_handler.clone(), approval_handler));
        tokio::select! {
            biased;
            result = &mut execution => {
                let terminal_was_delivered = control
                    .terminal_was_delivered()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                if !terminal_was_delivered {
                    return Err(HttpRunDriverError::new(
                        "production execution ended without a durable protocol terminal",
                    ));
                }
                let terminal = http_terminal_from_application_result(&result);
                registry
                    .record_run_terminal(&self.start.run.id, terminal)
                    .map_err(registry_driver_error)?;
            }
            cancellation = self.cancel_receiver.recv() => {
                let Some(cancellation) = cancellation else {
                    return Err(HttpRunDriverError::new(
                        "production cancellation owner closed before run terminal",
                    ));
                };
                let acknowledgement = cancellation.acknowledgement;
                let deadline = cancellation_deadline(self.options.cancellation_timeout);
                let mut acknowledgement_sent = false;
                let request_control = Arc::clone(&control);
                let request_broker = Arc::clone(&self.broker);
                let request_timeout = remaining_until(deadline);
                let mut request_worker = tokio::task::spawn_blocking(move || {
                    request_control.request_cancellation(
                        cancellation.reason,
                        Some(request_timeout),
                        || request_broker.cancel_all(),
                    )
                });
                let request = match tokio::time::timeout(
                    remaining_until(deadline),
                    &mut request_worker,
                )
                .await
                {
                    Ok(Ok(request)) => request,
                    Ok(Err(_)) => {
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production cancellation activation worker failed",
                            ),
                        );
                        let natural_result = (&mut execution).await;
                        if record_natural_terminal_if_delivered(
                            &control,
                            &registry,
                            &self.start.run.id,
                            &natural_result,
                        )? {
                            return Ok(());
                        }
                        return Err(error);
                    }
                    Err(_) => {
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production cancellation activation missed its shared deadline",
                            ),
                        );
                        acknowledgement_sent = true;
                        match request_worker.await {
                            Ok(request) => request,
                            Err(_) => {
                                let natural_result = (&mut execution).await;
                                if record_natural_terminal_if_delivered(
                                    &control,
                                    &registry,
                                    &self.start.run.id,
                                    &natural_result,
                                )? {
                                    return Ok(());
                                }
                                return Err(error);
                            }
                        }
                    }
                };
                let ticket = match request {
                    Ok(ticket) => ticket,
                    Err(error) => match error.into_ticket() {
                        Some(ticket) => ticket,
                        None => {
                            let natural_result = match tokio::time::timeout(
                                remaining_until(deadline),
                                &mut execution,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => {
                                    let error = HttpRunDriverError::new(
                                        "natural run terminal did not join before the cancellation deadline",
                                    );
                                    let error = if acknowledgement_sent {
                                        error
                                    } else {
                                        quarantine_cancellation_failure(
                                            &registry,
                                            &self.start.run.id,
                                            &acknowledgement,
                                            error,
                                        )
                                    };
                                    let natural_result = (&mut execution).await;
                                    if record_natural_terminal_if_delivered(
                                        &control,
                                        &registry,
                                        &self.start.run.id,
                                        &natural_result,
                                    )? {
                                        return Ok(());
                                    }
                                    return Err(error);
                                }
                            };
                            let terminal_was_delivered = match control.terminal_was_delivered() {
                                Ok(delivered) => delivered,
                                Err(error) => {
                                    let error = HttpRunDriverError::new(error.to_string());
                                    let error = if acknowledgement_sent {
                                        error
                                    } else {
                                        quarantine_cancellation_failure(
                                            &registry,
                                            &self.start.run.id,
                                            &acknowledgement,
                                            error,
                                        )
                                    };
                                    return Err(error);
                                }
                            };
                            if !terminal_was_delivered {
                                let error = HttpRunDriverError::new(
                                    "natural run completion won cancellation without a durable protocol terminal",
                                );
                                let error = if acknowledgement_sent {
                                    error
                                } else {
                                    quarantine_cancellation_failure(
                                        &registry,
                                        &self.start.run.id,
                                        &acknowledgement,
                                        error,
                                    )
                                };
                                return Err(error);
                            }
                            let terminal = http_terminal_from_application_result(&natural_result);
                            if let Err(error) = registry
                                .record_run_terminal(&self.start.run.id, terminal)
                            {
                                let error = registry_driver_error(error);
                                let error = if acknowledgement_sent {
                                    error
                                } else {
                                    quarantine_cancellation_failure(
                                        &registry,
                                        &self.start.run.id,
                                        &acknowledgement,
                                        error,
                                    )
                                };
                                return Err(error);
                            }
                            self.broker.cancel_all();
                            if !acknowledgement_sent {
                                let _ = acknowledgement.send(Ok(()));
                            }
                            return Ok(());
                        }
                    },
                };
                let execution_joined = tokio::time::timeout(
                    ticket.remaining_timeout(),
                    &mut execution,
                )
                .await
                .is_ok();
                if !execution_joined && !acknowledgement_sent {
                    let _ = quarantine_cancellation_failure(
                        &registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "production execution did not join before the cancellation deadline",
                        ),
                    );
                    acknowledgement_sent = true;
                }
                let finalize_control = Arc::clone(&control);
                let runtime = tokio::runtime::Handle::current();
                let mut cancellation_events = event_handler;
                let mut finalize_worker = tokio::task::spawn_blocking(move || {
                    runtime.block_on(finalize_control.finalize_cancellation(
                        ticket,
                        execution_joined,
                        &mut cancellation_events,
                    ))
                });
                let finalized = match tokio::time::timeout(
                    remaining_until(deadline),
                    &mut finalize_worker,
                )
                .await
                {
                    Ok(Ok(finalized)) => finalized,
                    Ok(Err(_)) => Err(anyhow!(
                        "production cancellation finalization worker failed"
                    )),
                    Err(_) => {
                        if !acknowledgement_sent {
                            let _ = quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                HttpRunDriverError::new(
                                    "production cancellation finalization missed its shared deadline",
                                ),
                            );
                            acknowledgement_sent = true;
                        }
                        finalize_worker.await.map_err(|_| {
                            HttpRunDriverError::new(
                                "production cancellation finalization worker failed",
                            )
                        })?
                    }
                };
                let terminal = match finalized {
                    Ok(sigil_kernel::RunCancellationTerminalOutcome::Cancelled) => {
                        HttpRunTerminalOutcome::Cancelled
                    }
                    Ok(sigil_kernel::RunCancellationTerminalOutcome::Interrupted) => {
                        HttpRunTerminalOutcome::Interrupted
                    }
                    Err(error) => {
                        let error = HttpRunDriverError::new(format!(
                            "production cancellation terminal could not be durably proven: {error}"
                        ));
                        let error = if acknowledgement_sent {
                            error
                        } else {
                            quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                error,
                            )
                        };
                        if !execution_joined {
                            let _ = (&mut execution).await;
                        }
                        return Err(error);
                    }
                };
                if !execution_joined {
                    let _ = (&mut execution).await;
                }
                let terminal_was_delivered = match control.terminal_was_delivered() {
                    Ok(delivered) => delivered,
                    Err(error) => {
                        let error = HttpRunDriverError::new(error.to_string());
                        let error = if acknowledgement_sent {
                            error
                        } else {
                            quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                error,
                            )
                        };
                        return Err(error);
                    }
                };
                if !terminal_was_delivered {
                    let error = HttpRunDriverError::new(
                        "production cancellation ended without a durable protocol terminal",
                    );
                    let error = if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    };
                    return Err(error);
                }
                if let Err(error) = registry
                    .record_run_terminal(&self.start.run.id, terminal)
                {
                    let error = registry_driver_error(error);
                    let error = if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    };
                    return Err(error);
                }
                if !acknowledgement_sent {
                    let _ = acknowledgement.send(Ok(()));
                }
            }
        }
        self.broker.cancel_all();
        Ok(())
    }

    async fn cancel_prepared_before_execution(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        cancellation: HttpProductionCancellationCommand,
        prepared: PreparedApplicationRun,
        deadline: Instant,
    ) -> Result<(), HttpRunDriverError> {
        let acknowledgement = cancellation.acknowledgement;
        if prepared.session_id() != self.start.session.durable_session_scope_id
            || prepared.session_log_path()
                != PathBuf::from(&self.start.session.session_log_path).as_path()
        {
            let error = HttpRunDriverError::new(
                "prepared cancellation does not match its durable HTTP session binding",
            );
            return Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            ));
        }
        let (execution, control) = prepared.into_parts();
        let control = Arc::new(control);
        let request_control = Arc::clone(&control);
        let request_broker = Arc::clone(&self.broker);
        let request_timeout = remaining_until(deadline);
        let mut request_worker = tokio::task::spawn_blocking(move || {
            request_control.request_cancellation(cancellation.reason, Some(request_timeout), || {
                request_broker.cancel_all()
            })
        });
        let mut acknowledgement_sent = false;
        let request = match tokio::time::timeout(remaining_until(deadline), &mut request_worker)
            .await
        {
            Ok(Ok(request)) => request,
            Ok(Err(_)) => {
                let error =
                    HttpRunDriverError::new("pre-execution cancellation activation worker failed");
                return Err(quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                ));
            }
            Err(_) => {
                let _ = quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    HttpRunDriverError::new(
                        "pre-execution cancellation activation missed its shared deadline",
                    ),
                );
                acknowledgement_sent = true;
                request_worker.await.map_err(|_| {
                    HttpRunDriverError::new("pre-execution cancellation activation worker failed")
                })?
            }
        };
        let ticket = match request {
            Ok(ticket) => ticket,
            Err(error) => match error.into_ticket() {
                Some(ticket) => ticket,
                None => {
                    let error = HttpRunDriverError::new(
                        "pre-execution cancellation could not be durably activated",
                    );
                    return Err(if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    });
                }
            },
        };
        drop(execution);
        let finalize_control = Arc::clone(&control);
        let runtime = tokio::runtime::Handle::current();
        let mut event_handler = HttpProductionEventHandler {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            run_id: self.start.run.id.clone(),
            approval_timeout: self.options.approval_timeout,
            registry: Arc::downgrade(registry),
            broker: Arc::clone(&self.broker),
            event_bus: Arc::clone(&self.event_bus),
        };
        let mut finalize_worker = tokio::task::spawn_blocking(move || {
            runtime.block_on(finalize_control.finalize_cancellation(
                ticket,
                true,
                &mut event_handler,
            ))
        });
        let finalized = match tokio::time::timeout(remaining_until(deadline), &mut finalize_worker)
            .await
        {
            Ok(Ok(finalized)) => finalized,
            Ok(Err(_)) => Err(anyhow!(
                "pre-execution cancellation finalization worker failed"
            )),
            Err(_) => {
                if !acknowledgement_sent {
                    let _ = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "pre-execution cancellation finalization missed its shared deadline",
                        ),
                    );
                    acknowledgement_sent = true;
                }
                finalize_worker.await.map_err(|_| {
                    HttpRunDriverError::new("pre-execution cancellation finalization worker failed")
                })?
            }
        };
        let result = finalized
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "pre-execution cancellation terminal could not be durably proven: {error}"
                ))
            })
            .and_then(|terminal| {
                if !control
                    .terminal_was_delivered()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?
                {
                    return Err(HttpRunDriverError::new(
                        "pre-execution cancellation ended without a durable protocol terminal",
                    ));
                }
                let terminal = match terminal {
                    sigil_kernel::RunCancellationTerminalOutcome::Cancelled => {
                        HttpRunTerminalOutcome::Cancelled
                    }
                    sigil_kernel::RunCancellationTerminalOutcome::Interrupted => {
                        HttpRunTerminalOutcome::Interrupted
                    }
                };
                registry
                    .record_run_terminal(&self.start.run.id, terminal)
                    .map(|_| ())
                    .map_err(registry_driver_error)
            });
        match result {
            Ok(()) => {
                if !acknowledgement_sent {
                    let _ = acknowledgement.send(Ok(()));
                }
                Ok(())
            }
            Err(error) if acknowledgement_sent => Err(error),
            Err(error) => Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            )),
        }
    }

    async fn cancel_after_failed_preparation(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        cancellation: HttpProductionCancellationCommand,
        deadline: Instant,
    ) -> Result<(), HttpRunDriverError> {
        let acknowledgement = cancellation.acknowledgement;
        let config_path = self.options.config_path.clone();
        let session_path = PathBuf::from(&self.start.session.session_log_path);
        let run_id = self.start.run.id.clone();
        let reason = cancellation.reason;
        let mut binding_worker = tokio::task::spawn_blocking(move || {
            record_application_preparation_cancellation(
                &config_path,
                &session_path,
                &run_id,
                &reason,
            )
        });
        let mut acknowledgement_sent = false;
        let binding_result =
            match tokio::time::timeout(remaining_until(deadline), &mut binding_worker).await {
                Ok(joined) => match joined {
                    Ok(binding) => {
                        binding.map_err(|error| HttpRunDriverError::new(error.to_string()))
                    }
                    Err(_) => Err(HttpRunDriverError::new(
                        "production preparation cancellation worker failed",
                    )),
                },
                Err(_) => {
                    let error = HttpRunDriverError::new(
                        "preparation cancellation evidence missed its shared deadline",
                    );
                    let _ = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        error,
                    );
                    acknowledgement_sent = true;
                    Ok(binding_worker
                        .await
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?)
                }
            };
        let binding = match binding_result {
            Ok(binding) => binding,
            Err(error) if acknowledgement_sent => return Err(error),
            Err(error) => {
                return Err(quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                ));
            }
        };
        let result = async {
            if binding.session_scope_id != self.start.session.durable_session_scope_id
                || binding.session_log_path != Path::new(&self.start.session.session_log_path)
            {
                return Err(HttpRunDriverError::new(
                    "preparation cancellation does not match its durable HTTP session binding",
                ));
            }
            let event = PublicRunEvent::new(
                &self.start.session.durable_session_scope_id,
                &self.start.run.id,
                1,
                PublicRunEventKind::RunCancelled,
            );
            let event_bus = Arc::clone(&self.event_bus);
            let mut publication_worker =
                tokio::task::spawn_blocking(move || event_bus.publish_run_event(event));
            match tokio::time::timeout(remaining_until(deadline), &mut publication_worker).await {
                Ok(joined) => {
                    joined
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation publication worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                }
                Err(_) => {
                    let error = HttpRunDriverError::new(
                        "preparation cancellation publication missed its shared deadline",
                    );
                    if !acknowledgement_sent {
                        let _ = quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        );
                        acknowledgement_sent = true;
                    }
                    publication_worker
                        .await
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation publication worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                }
            };
            registry
                .record_run_terminal(&self.start.run.id, HttpRunTerminalOutcome::Cancelled)
                .map(|_| ())
                .map_err(registry_driver_error)
        }
        .await;
        if acknowledgement_sent {
            return result;
        }
        match result {
            Ok(()) => {
                let _ = acknowledgement.send(Ok(()));
                Ok(())
            }
            Err(error) => Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            )),
        }
    }
}

#[derive(Clone)]
struct HttpProductionEventHandler {
    durable_session_scope_id: String,
    run_id: String,
    approval_timeout: Duration,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
    event_bus: Arc<HttpLiveEventBus>,
}

impl ApplicationRunEventHandler for HttpProductionEventHandler {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        if event.run_id != self.run_id {
            return Err(anyhow!(
                "application event belongs to another production run"
            ));
        }
        if event.session_id != self.durable_session_scope_id {
            return Err(anyhow!(
                "application event belongs to another durable production session"
            ));
        }
        let approval_request = match &event.event {
            PublicRunEventKind::ApprovalRequested {
                call,
                spec,
                subjects,
                network_effect,
                local_policy_decision,
                network_policy_decision,
                source_policy_decision,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                ..
            } => {
                let registry = self
                    .registry
                    .upgrade()
                    .ok_or_else(|| anyhow!("production approval registry is closed"))?;
                let session_grant_available = match (
                    operation,
                    risk,
                    local_policy_decision,
                    network_policy_decision,
                    source_policy_decision,
                ) {
                    (Some(operation), Some(risk), Some(local), Some(network), Some(source)) => {
                        tool_approval_session_grant_available_for_facets(
                            spec.access,
                            *network_effect,
                            *operation,
                            *risk,
                            subjects,
                            subject_zones,
                            confirmation.as_ref(),
                            *snapshot_required,
                            *local,
                            *network,
                            *source,
                        )
                    }
                    _ => false,
                };
                let pending = self.broker.register(
                    &self.run_id,
                    call,
                    spec,
                    self.approval_timeout,
                    session_grant_available,
                )?;
                if let Err(error) =
                    registry.register_approval_request(&self.run_id, pending.clone())
                {
                    self.broker.cancel(&call.id);
                    return Err(anyhow!(error));
                }
                Some(pending)
            }
            _ => None,
        };
        if let Err(error) = self
            .event_bus
            .publish_run_event_with_approval(event, approval_request.clone())
        {
            if let Some(approval) = approval_request {
                let call_id = approval.call_id;
                self.broker.cancel(&call_id);
                if let Some(registry) = self.registry.upgrade() {
                    let _ = registry.expire_approval_request(&self.run_id, &call_id);
                }
            }
            return Err(anyhow!(error));
        }
        Ok(())
    }
}

struct HttpProductionApprovalHandler {
    run_id: String,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
}

impl ApprovalHandler for HttpProductionApprovalHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        let outcome = self.broker.wait_for_decision(&call.id)?;
        if outcome.expired
            && let Some(registry) = self.registry.upgrade()
        {
            registry
                .expire_approval_request(&self.run_id, &call.id)
                .map_err(|error| anyhow!(error))?;
        }
        match outcome.decision {
            Some(HttpApprovalDecisionRecord {
                decision: ToolApprovalUserDecision::Approved,
                ..
            }) => Ok(ToolApproval::Approve),
            Some(HttpApprovalDecisionRecord {
                decision: ToolApprovalUserDecision::Denied,
                reason,
                ..
            }) => Ok(ToolApproval::Deny {
                reason: reason.unwrap_or_else(|| "HTTP user denied tool call".to_owned()),
            }),
            Some(HttpApprovalDecisionRecord {
                decision: ToolApprovalUserDecision::ApprovedForSession,
                ..
            }) => Ok(ToolApproval::ApproveForSession),
            None => Ok(ToolApproval::Deny {
                reason: "HTTP approval request expired before a decision arrived".to_owned(),
            }),
        }
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct HttpApprovalBroker {
    pending: Mutex<BTreeMap<String, Arc<HttpApprovalSlot>>>,
}

impl HttpApprovalBroker {
    fn register(
        &self,
        run_id: &str,
        call: &ToolCall,
        spec: &ToolSpec,
        timeout: Duration,
        session_grant_available: bool,
    ) -> Result<HttpPendingApproval> {
        let now_ms = current_unix_time_ms();
        let timeout_ms = timeout.as_millis().try_into().unwrap_or(u64::MAX);
        let expires_at_ms = now_ms.saturating_add(timeout_ms);
        let tool_call_hash = tool_call_hash(call)?;
        let approval_request_id =
            approval_request_id(run_id, &call.id, &tool_call_hash, expires_at_ms);
        let slot = Arc::new(HttpApprovalSlot {
            deadline: Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
            state: Mutex::new(HttpApprovalSlotState::Waiting),
            changed: Condvar::new(),
        });
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?;
        if pending.insert(call.id.clone(), slot).is_some() {
            return Err(anyhow!("duplicate production approval call id"));
        }
        Ok(HttpPendingApproval {
            call_id: call.id.clone(),
            tool_name: spec.name.clone(),
            approval_request_id,
            tool_call_hash,
            policy_version: HTTP_APPROVAL_POLICY_VERSION.to_owned(),
            expires_at_ms,
            session_grant_available,
        })
    }

    fn resolve(
        &self,
        call_id: &str,
        decision: HttpApprovalDecisionRecord,
    ) -> Result<(), HttpRunDriverError> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| HttpRunDriverError::new("production approval broker is unavailable"))?
            .get(call_id)
            .cloned()
            .ok_or_else(|| {
                HttpRunDriverError::new(format!("production approval is not pending: {call_id}"))
            })?;
        let mut state = slot
            .state
            .lock()
            .map_err(|_| HttpRunDriverError::new("production approval slot is unavailable"))?;
        if !matches!(*state, HttpApprovalSlotState::Waiting) {
            return Err(HttpRunDriverError::new(format!(
                "production approval is no longer waiting: {call_id}"
            )));
        }
        *state = HttpApprovalSlotState::Resolved(decision);
        slot.changed.notify_all();
        Ok(())
    }

    fn wait_for_decision(&self, call_id: &str) -> Result<HttpApprovalWaitOutcome> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?
            .get(call_id)
            .cloned()
            .ok_or_else(|| anyhow!("production approval slot is missing"))?;
        let mut state = slot
            .state
            .lock()
            .map_err(|_| anyhow!("production approval slot is unavailable"))?;
        loop {
            match &*state {
                HttpApprovalSlotState::Resolved(decision) => {
                    let decision = decision.clone();
                    drop(state);
                    self.remove(call_id, &slot);
                    return Ok(HttpApprovalWaitOutcome {
                        decision: Some(decision),
                        expired: false,
                    });
                }
                HttpApprovalSlotState::Cancelled => {
                    drop(state);
                    self.remove(call_id, &slot);
                    return Err(anyhow!("production approval wait was cancelled"));
                }
                HttpApprovalSlotState::Waiting => {}
            }
            let remaining = slot.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                *state = HttpApprovalSlotState::Cancelled;
                drop(state);
                self.remove(call_id, &slot);
                return Ok(HttpApprovalWaitOutcome {
                    decision: None,
                    expired: true,
                });
            }
            let waited = slot
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| anyhow!("production approval slot is unavailable"))?;
            state = waited.0;
        }
    }

    fn cancel(&self, call_id: &str) {
        let slot = self
            .pending
            .lock()
            .ok()
            .and_then(|pending| pending.get(call_id).cloned());
        if let Some(slot) = slot
            && let Ok(mut state) = slot.state.lock()
        {
            *state = HttpApprovalSlotState::Cancelled;
            slot.changed.notify_all();
        }
    }

    fn cancel_all(&self) {
        let slots = self
            .pending
            .lock()
            .map(|pending| pending.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for slot in slots {
            if let Ok(mut state) = slot.state.lock() {
                *state = HttpApprovalSlotState::Cancelled;
                slot.changed.notify_all();
            }
        }
    }

    fn remove(&self, call_id: &str, expected: &Arc<HttpApprovalSlot>) {
        if let Ok(mut pending) = self.pending.lock()
            && pending
                .get(call_id)
                .is_some_and(|slot| Arc::ptr_eq(slot, expected))
        {
            pending.remove(call_id);
        }
    }
}

struct HttpApprovalSlot {
    deadline: Instant,
    state: Mutex<HttpApprovalSlotState>,
    changed: Condvar,
}

enum HttpApprovalSlotState {
    Waiting,
    Resolved(HttpApprovalDecisionRecord),
    Cancelled,
}

struct HttpApprovalWaitOutcome {
    decision: Option<HttpApprovalDecisionRecord>,
    expired: bool,
}

fn tool_call_hash(call: &ToolCall) -> Result<String> {
    let bytes = serde_json::to_vec(call)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn approval_request_id(
    run_id: &str,
    call_id: &str,
    tool_call_hash: &str,
    expires_at_ms: u64,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        run_id.as_bytes(),
        call_id.as_bytes(),
        tool_call_hash.as_bytes(),
        &expires_at_ms.to_be_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("http-approval-v1:{:x}", hasher.finalize())
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn cancellation_deadline(timeout: Duration) -> Instant {
    Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now)
}

fn remaining_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn quarantine_cancellation_failure(
    registry: &HttpSessionRunRegistry,
    run_id: &str,
    acknowledgement: &std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
    error: HttpRunDriverError,
) -> HttpRunDriverError {
    let error = match registry.record_run_execution_uncertain(run_id) {
        Ok(_) => error,
        Err(quarantine_error) => HttpRunDriverError::new(format!(
            "{error}; production run quarantine failed: {quarantine_error}"
        )),
    };
    let _ = acknowledgement.send(Err(error.clone()));
    error
}

fn record_natural_terminal_if_delivered(
    control: &ApplicationRunControl,
    registry: &HttpSessionRunRegistry,
    run_id: &str,
    result: &Result<ApplicationRunOutput>,
) -> Result<bool, HttpRunDriverError> {
    if !control
        .terminal_was_delivered()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?
    {
        return Ok(false);
    }
    registry
        .record_run_terminal(run_id, http_terminal_from_application_result(result))
        .map_err(registry_driver_error)?;
    Ok(true)
}

fn http_terminal_from_application_result(
    result: &Result<ApplicationRunOutput>,
) -> HttpRunTerminalOutcome {
    match result {
        Ok(output) => match output.terminal_status {
            ApplicationRunTerminalStatus::Succeeded => HttpRunTerminalOutcome::Finished,
            ApplicationRunTerminalStatus::Interrupted => HttpRunTerminalOutcome::Interrupted,
            ApplicationRunTerminalStatus::Blocked => HttpRunTerminalOutcome::Failed,
        },
        Err(_) => HttpRunTerminalOutcome::Failed,
    }
}

fn registry_driver_error(error: crate::HttpRegistryError) -> HttpRunDriverError {
    HttpRunDriverError::new(format!(
        "production registry terminal update failed: {error}"
    ))
}

#[cfg(test)]
#[path = "tests/production_driver_tests.rs"]
mod tests;
