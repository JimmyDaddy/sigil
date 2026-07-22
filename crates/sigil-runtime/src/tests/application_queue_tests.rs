use std::{
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    Agent, AgentRunOptions, AutoApproveHandler, CompactionConfig, CompletionRequest, ControlEntry,
    ConversationInputKind, ConversationInputPromotedEntry, ConversationInputQueueId,
    ConversationInputQueuedEntry, ConversationInputReorderedEntry, ConversationInputStatus,
    ConversationInputStatusEntry, ConversationInputTarget, ConversationQueueDurableProjection,
    DurableEventType, FrozenProviderRequestMaterial, InteractionMode, JsonlSessionStore,
    MemoryConfig, ModelMessage, NoopEventHandler, PermissionConfig, PermissionEvaluationContext,
    Provider, ProviderCapabilities, ProviderChunk, ReasoningStreamSupport, SecretString, Session,
    SessionLogEntry, ToolRegistry, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
};

use super::{
    ApplicationQueuedPromptMaterial, ApplicationQueuedRunPreparationRequest,
    ApplicationQueuedRunPrepareErrorClass, ApplicationQueuedRunRequest,
    PreparedApplicationQueuedRunInput, prepare_application_queued_run,
    prepare_application_queued_run_input,
};
use crate::application_run::{
    ApplicationRunInteraction, ApplicationRunRequest, ApplicationRunServices,
    ApplicationTranscriptRole, application_session_transcript_page, bind_application_session,
    prepare_application_run_with_exact_first_request,
};

struct QueueFixture {
    session: Session,
    durable_queue: ConversationQueueDurableProjection,
    promotion: ConversationInputPromotedEntry,
    frozen_request: FrozenProviderRequestMaterial,
    prompt_material: ApplicationQueuedPromptMaterial,
    exact_prompt: String,
}

fn queue_fixture(root: &Path, exact_prompt: &str) -> Result<QueueFixture> {
    let store = JsonlSessionStore::new(root.join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    })?;
    let queue_id = ConversationInputQueueId::new("queue-1")?;
    let projection = project_conversation_prompt_for_persistence(exact_prompt);
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: projection.prompt_hash.clone(),
            prompt: projection.safe_prompt.clone(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        },
    ))?;
    let durable_queue = session
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("durable queue projection");
    let revision = durable_queue
        .revision
        .clone()
        .expect("queued entry must establish a revision");
    let mut durable_user_message = ModelMessage::user(projection.safe_prompt.clone());
    durable_user_message.id = "queued-user-message-1".to_owned();
    let promotion = ConversationInputPromotedEntry {
        queue_id: queue_id.clone(),
        expected_queue_revision: revision.clone(),
        prompt_hash: projection.prompt_hash.clone(),
        exact_prompt_required: projection.exact_prompt_required,
        durable_user_message,
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "queued-dispatch-1".to_owned(),
        promoted_at_ms: 2,
    };
    let mut exact_user_message = ModelMessage::user(exact_prompt);
    exact_user_message.id = promotion.durable_user_message.id.clone();
    let frozen_request = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        completion_request(vec![exact_user_message]),
    )?;
    let prompt_material = if projection.exact_prompt_required {
        ApplicationQueuedPromptMaterial::AvailableProcessLocal {
            queue_id,
            prompt_hash: projection.prompt_hash,
            exact_prompt: SecretString::new(exact_prompt),
        }
    } else {
        ApplicationQueuedPromptMaterial::PersistedSafe
    };
    Ok(QueueFixture {
        session,
        durable_queue,
        promotion,
        frozen_request,
        prompt_material,
        exact_prompt: exact_prompt.to_owned(),
    })
}

fn completion_request(messages: Vec<ModelMessage>) -> CompletionRequest {
    CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages,
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: false,
        hosted_tools: Vec::new(),
    }
}

fn prepare_fixture(
    fixture: QueueFixture,
) -> Result<PreparedApplicationQueuedRunInput, super::ApplicationQueuedRunPrepareError> {
    prepare_application_queued_run_input(ApplicationQueuedRunPreparationRequest {
        session_scope_id: fixture.session.session_scope_id().to_owned(),
        durable_queue: fixture.durable_queue,
        promotion: fixture.promotion,
        prompt_material: fixture.prompt_material,
        capability_registrations: Vec::new(),
        frozen_request: fixture.frozen_request,
    })
}

#[test]
fn persisted_safe_and_process_local_material_both_prepare_exact_frozen_inputs() -> Result<()> {
    let safe_root = tempfile::tempdir()?;
    let safe = prepare_fixture(queue_fixture(safe_root.path(), "inspect README.md")?)?;
    assert_eq!(safe.queue_id().as_str(), "queue-1");
    assert_eq!(safe.dispatch_run_id(), "queued-dispatch-1");
    assert!(
        safe.frozen_request_fingerprint()
            .starts_with("hmac-sha256:")
    );

    let exact_root = tempfile::tempdir()?;
    let exact = prepare_fixture(queue_fixture(
        exact_root.path(),
        "inspect with authorization=super-secret-value",
    )?)?;
    assert_eq!(exact.queue_id().as_str(), "queue-1");
    assert_eq!(exact.dispatch_run_id(), "queued-dispatch-1");
    Ok(())
}

#[test]
fn exact_material_is_fail_closed_after_restart_or_binding_drift() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut fixture = queue_fixture(root.path(), "inspect with authorization=super-secret-value")?;
    fixture.prompt_material = ApplicationQueuedPromptMaterial::RequiresReentry;
    let error = prepare_fixture(fixture).expect_err("lost exact prompt must require reentry");
    assert_eq!(
        error.class(),
        ApplicationQueuedRunPrepareErrorClass::RequiresReentry
    );

    let root = tempfile::tempdir()?;
    let mut fixture = queue_fixture(root.path(), "inspect with authorization=super-secret-value")?;
    let ApplicationQueuedPromptMaterial::AvailableProcessLocal { prompt_hash, .. } =
        &mut fixture.prompt_material
    else {
        panic!("secret prompt should require process-local material");
    };
    *prompt_hash = "exact-required:sha256:stale".to_owned();
    let error = prepare_fixture(fixture).expect_err("stale prompt binding must fail");
    assert_eq!(
        error.class(),
        ApplicationQueuedRunPrepareErrorClass::PromptMaterialMismatch
    );
    Ok(())
}

#[test]
fn process_local_exact_material_survives_queue_reorder_revision_changes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut fixture = queue_fixture(root.path(), "inspect with authorization=super-secret-value")?;
    let other_queue_id = ConversationInputQueueId::new("queue-2")?;
    let other_projection = project_conversation_prompt_for_persistence("inspect Cargo.toml");
    fixture
        .session
        .append_control(ControlEntry::ConversationInputQueued(
            ConversationInputQueuedEntry {
                queue_id: other_queue_id.clone(),
                target: ConversationInputTarget::MainThread,
                kind: ConversationInputKind::Chat,
                prompt_hash: other_projection.prompt_hash,
                prompt: other_projection.safe_prompt,
                reasoning_effort: None,
                created_at_ms: Some(2),
            },
        ))?;
    fixture
        .session
        .append_control(ControlEntry::ConversationInputReordered(
            ConversationInputReorderedEntry {
                queue_id: other_queue_id,
                after_queue_id: None,
                updated_at_ms: Some(3),
            },
        ))?;
    fixture
        .session
        .append_control(ControlEntry::ConversationInputReordered(
            ConversationInputReorderedEntry {
                queue_id: fixture.promotion.queue_id.clone(),
                after_queue_id: None,
                updated_at_ms: Some(4),
            },
        ))?;

    fixture.durable_queue = fixture
        .session
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("reordered durable queue projection");
    fixture.promotion.expected_queue_revision = fixture
        .durable_queue
        .revision
        .clone()
        .expect("reorder must advance the queue revision");

    let prepared = prepare_fixture(fixture)?;
    assert_eq!(prepared.queue_id().as_str(), "queue-1");
    Ok(())
}

#[test]
fn stale_queue_revision_and_duplicate_frozen_turn_are_rejected() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut fixture = queue_fixture(root.path(), "inspect README.md")?;
    fixture.promotion.expected_queue_revision.event_id = "stale-event-id".to_owned();
    let error = prepare_fixture(fixture).expect_err("stale queue revision must fail");
    assert_eq!(
        error.class(),
        ApplicationQueuedRunPrepareErrorClass::QueueConflict
    );

    let root = tempfile::tempdir()?;
    let mut fixture = queue_fixture(root.path(), "inspect README.md")?;
    let duplicate = fixture.frozen_request.request().messages[0].clone();
    fixture.frozen_request = FrozenProviderRequestMaterial::freeze(
        fixture.session.session_scope_id(),
        completion_request(vec![duplicate.clone(), duplicate]),
    )?;
    let error = prepare_fixture(fixture).expect_err("duplicate exact turn must fail");
    assert_eq!(
        error.class(),
        ApplicationQueuedRunPrepareErrorClass::FrozenRequestMismatch
    );
    Ok(())
}

#[derive(Clone)]
struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: true,
            reports_cache_tokens: true,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: true,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: true,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: true,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests
            .lock()
            .expect("recording provider lock")
            .push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
async fn queued_input_sends_the_exact_turn_once_without_persisting_a_second_user_message()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = queue_fixture(root.path(), "inspect with authorization=super-secret-value")?;
    let expected_exact_prompt = fixture.exact_prompt.clone();
    let promotion = fixture.promotion.clone();
    let mut session = fixture.session;
    let prepared = prepare_application_queued_run_input(ApplicationQueuedRunPreparationRequest {
        session_scope_id: session.session_scope_id().to_owned(),
        durable_queue: fixture.durable_queue,
        promotion: promotion.clone(),
        prompt_material: fixture.prompt_material,
        capability_registrations: Vec::new(),
        frozen_request: fixture.frozen_request,
    })?;
    JsonlSessionStore::new(root.path().join("session.jsonl"))?
        .append_conversation_input_promoted(promotion.clone())?;
    session.record_durably_appended_conversation_input_promotion(promotion.clone())?;

    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        RecordingProvider {
            requests: Arc::clone(&requests),
        },
        ToolRegistry::new(),
    );
    let mut handler = NoopEventHandler;
    let mut approval = AutoApproveHandler;
    agent
        .run_with_approval_input(
            &mut session,
            prepared.input,
            test_run_options(root.path()),
            &mut handler,
            &mut approval,
        )
        .await?;

    let user_messages = session
        .entries()
        .iter()
        .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
        .count();
    assert_eq!(
        user_messages, 1,
        "active session must project the promoted user once"
    );
    assert_eq!(
        JsonlSessionStore::read_event_records(root.path().join("session.jsonl"))?
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0,
        "promotion must remain the unique durable user event"
    );
    let requests = requests.lock().expect("recorded requests");
    assert_eq!(requests.len(), 1);
    let exact_turns = requests[0]
        .messages
        .iter()
        .filter(|message| {
            message.id == promotion.durable_user_message.id
                && message.content.as_deref() == Some(expected_exact_prompt.as_str())
        })
        .count();
    assert_eq!(exact_turns, 1, "provider exact turn must appear once");
    Ok(())
}

fn test_run_options(workspace_root: &Path) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: workspace_root.to_path_buf(),
        max_turns: Some(2),
        tool_timeout_secs: 30,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    }
}

struct RejectingDisclosurePresenter;

#[async_trait]
impl sigil_kernel::EgressDisclosurePresenter for RejectingDisclosurePresenter {
    async fn present(
        &self,
        _disclosure: sigil_kernel::PreEgressDisclosure,
    ) -> std::result::Result<
        sigil_kernel::DisclosurePresentationReceipt,
        sigil_kernel::DisclosurePresentationError,
    > {
        Err(sigil_kernel::DisclosurePresentationError::SinkClosed)
    }
}

struct ApplicationOwnedQueueFixture {
    config_path: PathBuf,
    session_log_path: PathBuf,
    session_scope_id: String,
    session: Session,
    durable_queue: ConversationQueueDurableProjection,
    promotion: ConversationInputPromotedEntry,
    prompt_material: ApplicationQueuedPromptMaterial,
    exact_prompt: String,
    safe_prompt: String,
}

impl ApplicationOwnedQueueFixture {
    fn run_request(&self) -> ApplicationRunRequest {
        let mut request = ApplicationRunRequest::non_interactive(
            &self.config_path,
            self.config_path.parent().expect("config parent"),
            self.safe_prompt.clone(),
            self.promotion.dispatch_run_id.clone(),
        );
        request.session_path = Some(self.session_log_path.clone());
        request.interaction = ApplicationRunInteraction::NonInteractive;
        request
    }
}

fn application_owned_queue_fixture(
    root: &Path,
    exact_prompt: &str,
) -> Result<ApplicationOwnedQueueFixture> {
    let config_path = root.join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;
    let session_path = root.join("state/sessions/queued.jsonl");
    let binding = bind_application_session(&config_path, root, Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let queue_id = ConversationInputQueueId::new("queue-hook")?;
    let projection = project_conversation_prompt_for_persistence(exact_prompt);
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: projection.prompt_hash.clone(),
            prompt: projection.safe_prompt.clone(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        },
    ))?;
    let durable_queue = session
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("durable queue projection");
    let revision = durable_queue.revision.clone().expect("queue revision");
    let mut durable_user_message = ModelMessage::user(projection.safe_prompt.clone());
    durable_user_message.id = "queued-hook-user".to_owned();
    let promotion = ConversationInputPromotedEntry {
        queue_id: queue_id.clone(),
        expected_queue_revision: revision,
        prompt_hash: projection.prompt_hash.clone(),
        exact_prompt_required: projection.exact_prompt_required,
        durable_user_message,
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "queued-hook-run".to_owned(),
        promoted_at_ms: 2,
    };
    let prompt_material = if projection.exact_prompt_required {
        ApplicationQueuedPromptMaterial::AvailableProcessLocal {
            queue_id,
            prompt_hash: projection.prompt_hash,
            exact_prompt: SecretString::new(exact_prompt),
        }
    } else {
        ApplicationQueuedPromptMaterial::PersistedSafe
    };
    Ok(ApplicationOwnedQueueFixture {
        config_path,
        session_log_path: binding.session_log_path,
        session_scope_id: binding.session_scope_id,
        session,
        durable_queue,
        promotion,
        prompt_material,
        exact_prompt: exact_prompt.to_owned(),
        safe_prompt: projection.safe_prompt,
    })
}

#[tokio::test]
async fn application_assembly_freezes_exact_first_request_without_persisting_it() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = application_owned_queue_fixture(
        root.path(),
        "inspect with authorization=super-secret-value",
    )?;
    let run_request = fixture.run_request();
    let durable_message_id = fixture.promotion.durable_user_message.id.clone();
    let session_scope_id = fixture.session_scope_id.clone();
    let session_log_path = fixture.session_log_path.clone();
    let exact_prompt = fixture.exact_prompt.clone();
    drop(fixture.session);

    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let (prepared, assembly) = prepare_application_run_with_exact_first_request(
        run_request,
        &services,
        SecretString::new(exact_prompt.clone()),
        durable_message_id.clone(),
    )
    .await?;
    let matching_exact_turns = assembly
        .frozen_request
        .request()
        .messages
        .iter()
        .filter(|message| {
            message.id == durable_message_id
                && message.content.as_deref() == Some(exact_prompt.as_str())
        })
        .count();
    assert_eq!(matching_exact_turns, 1);
    let run_input_debug = format!("{:?}", assembly.run_input);
    assert!(run_input_debug.contains("cancellation: Some"));
    assert!(run_input_debug.contains("initial_frozen_provider_request: Some"));
    drop(prepared);

    let restored = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&session_log_path)?,
    )?;
    assert_eq!(restored.session_scope_id(), session_scope_id);
    assert!(!restored.entries().iter().any(
        |entry| matches!(entry, SessionLogEntry::User(message) if message.id == durable_message_id)
    ));
    assert!(!std::fs::read_to_string(session_log_path)?.contains(&exact_prompt));
    Ok(())
}

#[tokio::test]
async fn application_owned_queued_run_commits_one_promoted_user_event_and_no_exact_prompt()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture = application_owned_queue_fixture(
        root.path(),
        "inspect with authorization=super-secret-value",
    )?;
    let run = fixture.run_request();
    let durable_message_id = fixture.promotion.durable_user_message.id.clone();
    let queue_id = fixture.promotion.queue_id.clone();
    let safe_prompt = fixture.safe_prompt.clone();
    let exact_prompt = fixture.exact_prompt.clone();
    let session_log_path = fixture.session_log_path.clone();
    drop(fixture.session);

    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let committed = prepare_application_queued_run(
        ApplicationQueuedRunRequest {
            run,
            durable_queue: fixture.durable_queue,
            promotion: fixture.promotion,
            prompt_material: fixture.prompt_material,
            capability_registrations: Vec::new(),
        },
        &services,
    )
    .await?;
    assert!(committed.has_in_memory_queued_promotion(&queue_id));

    let store = JsonlSessionStore::new(committed.session_log_path())?;
    let restored = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    let matching_user_messages = restored
        .entries()
        .iter()
        .filter(|entry| {
            matches!(entry, SessionLogEntry::User(message) if message.id == durable_message_id)
        })
        .count();
    assert_eq!(matching_user_messages, 0);
    assert!(restored.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion))
                if promotion.durable_user_message.id == durable_message_id
                    && promotion.durable_user_message.content.as_deref()
                        == Some(safe_prompt.as_str())
        )
    }));
    let records = JsonlSessionStore::read_event_records(committed.session_log_path())?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind()
                    == Some(DurableEventType::ConversationInputPromoted)
            })
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0
    );
    assert!(
        !restored
            .context_projection()
            .model_messages()
            .iter()
            .any(|message| message.id == durable_message_id)
    );
    let transcript = application_session_transcript_page(
        committed.session_log_path(),
        restored.session_scope_id(),
        None,
        20,
    )?;
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|message| {
                message.role == ApplicationTranscriptRole::User
                    && message.content.as_deref() == Some(safe_prompt.as_str())
            })
            .count(),
        1
    );

    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id: queue_id.clone(),
            status: ConversationInputStatus::Delivered,
            reason: None,
            updated_at_ms: Some(3),
        }),
    ))?;
    let delivered = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    assert_eq!(
        delivered
            .context_projection()
            .model_messages()
            .iter()
            .filter(|message| {
                message.id == durable_message_id
                    && message.content.as_deref() == Some(safe_prompt.as_str())
            })
            .count(),
        1
    );
    assert!(!std::fs::read_to_string(session_log_path)?.contains(&exact_prompt));
    assert!(
        restored
            .conversation_queue_projection()
            .next_dispatchable
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn stale_promotion_returns_no_executable_run_and_persists_no_user_message() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut fixture = application_owned_queue_fixture(root.path(), "inspect README.md")?;
    let run = fixture.run_request();
    let durable_message_id = fixture.promotion.durable_user_message.id.clone();
    let session_log_path = fixture.session_log_path.clone();
    let other_projection = project_conversation_prompt_for_persistence("inspect Cargo.toml");
    fixture
        .session
        .append_control(ControlEntry::ConversationInputQueued(
            ConversationInputQueuedEntry {
                queue_id: ConversationInputQueueId::new("queue-after-snapshot")?,
                target: ConversationInputTarget::MainThread,
                kind: ConversationInputKind::Chat,
                prompt_hash: other_projection.prompt_hash,
                prompt: other_projection.safe_prompt,
                reasoning_effort: None,
                created_at_ms: Some(3),
            },
        ))?;
    drop(fixture.session);

    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));
    let error = match prepare_application_queued_run(
        ApplicationQueuedRunRequest {
            run,
            durable_queue: fixture.durable_queue,
            promotion: fixture.promotion,
            prompt_material: fixture.prompt_material,
            capability_registrations: Vec::new(),
        },
        &services,
    )
    .await
    {
        Ok(_) => panic!("stale writer-lock promotion returned an executable run"),
        Err(error) => error,
    };
    assert_eq!(
        error.class(),
        ApplicationQueuedRunPrepareErrorClass::PromotionCommit
    );

    let restored = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(session_log_path)?,
    )?;
    assert!(!restored.entries().iter().any(
        |entry| matches!(entry, SessionLogEntry::User(message) if message.id == durable_message_id)
    ));
    Ok(())
}
