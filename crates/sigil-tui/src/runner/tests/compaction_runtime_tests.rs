use anyhow::Result;
use sigil_kernel::{
    AgentConfig, ConversationInputKind, ConversationInputQueueId, ConversationInputTarget,
    DurableAuditRecord, DurableAuditWriter, DurableEventType, MemoryConfig, ModelMessage,
    PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptPurpose, ProviderPhysicalAttemptStartedEntry,
    ProviderPhysicalAttemptTerminalEntry, ProviderRequestRejection, ReasoningEffort, RootConfig,
    SessionConfig, StorageRoot, WorkspaceConfig,
};

use crate::runner::worker_loop::queue_conversation_input;
use crate::runner::worker_loop::queue_driver::{
    QueuedConversationCandidatePreparation, prepare_next_queued_conversation_candidate,
};

use super::*;

const RAW_PROMPT: &str = "inspect https://example.com/private?signature=pre-turn-secret exactly";

fn append_context_window_rejection(
    store: &JsonlSessionStore,
    physical_attempt_id: &str,
    logical_run_id: &str,
    purpose: ProviderPhysicalAttemptPurpose,
    rejection: Option<ProviderRequestRejection>,
) -> Result<()> {
    let started_event_id = format!("{physical_attempt_id}-start");
    let fingerprint = format!("hmac-sha256:{physical_attempt_id}");
    let started = ProviderPhysicalAttemptStartedEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: physical_attempt_id.to_owned(),
        logical_run_id: logical_run_id.to_owned(),
        purpose,
        request_material_fingerprint: fingerprint.clone(),
        provider_name: "openai_responses".to_owned(),
        model_name: "gpt-4.1-2025-04-14".to_owned(),
        started_at_unix_ms: 1,
    };
    let started_record = DurableAuditRecord::new(
        DurableEventType::ProviderPhysicalAttemptStarted,
        serde_json::to_value(started)?,
        physical_attempt_id,
        Some(started_event_id.clone()),
    )?
    .with_event_id(started_event_id.clone())?;
    store.append_and_sync(started_record)?;

    let terminal = ProviderPhysicalAttemptTerminalEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: physical_attempt_id.to_owned(),
        request_material_fingerprint: fingerprint,
        outcome: ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
        rejection,
        provider_request_id: None,
        provider_response_id: None,
        durable_output_event_ids: Vec::new(),
        durable_side_effect_event_ids: Vec::new(),
        finished_at_unix_ms: 2,
    };
    let terminal_record = DurableAuditRecord::new(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        serde_json::to_value(terminal)?,
        physical_attempt_id,
        Some(started_event_id.clone()),
    )?
    .with_event_id(format!("{physical_attempt_id}-terminal"))?
    .with_causation_id(started_event_id)?;
    store.append_and_sync(terminal_record)?;
    Ok(())
}

fn root_config(workspace_root: &std::path::Path, cache_root: &std::path::Path) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: sigil_kernel::StorageConfig {
            state_root: StorageRoot::Path(workspace_root.join("state").display().to_string()),
            cache_root: StorageRoot::Path(cache_root.display().to_string()),
            mutation_artifact_retention: Default::default(),
        },
        session: SessionConfig {
            log_dir: Some(workspace_root.join("sessions").display().to_string()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: MemoryConfig { enabled: false },
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        web: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn overflow_recovery_accepts_only_one_exact_durable_context_rejection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("openai_responses", "gpt-4.1-2025-04-14").with_store(store.clone());
    append_context_window_rejection(
        &store,
        "attempt-exact",
        "foreground-run-1",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;

    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-1")?,
        Some("attempt-exact".to_owned())
    );

    append_context_window_rejection(
        &store,
        "attempt-second",
        "foreground-run-1",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;
    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-1")?,
        None,
        "multiple provider attempts are never replayed"
    );
    Ok(())
}

#[test]
fn overflow_recovery_rejects_non_context_terminal_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("openai_responses", "gpt-4.1-2025-04-14").with_store(store.clone());
    append_context_window_rejection(
        &store,
        "attempt-other",
        "foreground-run-2",
        ProviderPhysicalAttemptPurpose::ConversationGeneration,
        None,
    )?;

    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-2")?,
        None
    );
    Ok(())
}

#[test]
fn overflow_recovery_rejects_non_conversation_attempt_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("openai_responses", "gpt-4.1-2025-04-14").with_store(store.clone());
    append_context_window_rejection(
        &store,
        "attempt-measurement",
        "foreground-run-3",
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
        Some(ProviderRequestRejection::ContextWindowExceeded),
    )?;

    assert_eq!(
        exact_context_window_rejection_source(&session, "foreground-run-3")?,
        None
    );
    Ok(())
}

#[test]
fn queued_pre_turn_admission_blocks_without_local_proof_and_never_mutates_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cache_root = temp.path().join("cache");
    let root_config = root_config(temp.path(), &cache_root);
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone()));
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let session = session.expect("store-backed session should remain available");
    let before_stream = std::fs::read(store.path())?;

    let admission = prepare_next_queued_conversation_pre_turn_admission(
        &root_config,
        temp.path(),
        store.path(),
        &session,
        &exact_prompts,
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
    )?;

    assert!(matches!(
        admission,
        QueuedConversationPreTurnAdmission::Blocked { ref reason, .. }
            if reason == "queued pre-turn exact admission is unavailable from the local token profile"
    ));
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(exact_prompts.contains_key(&ConversationInputQueueId::new("queue_1")?));
    let durable_json = String::from_utf8(before_stream)?;
    assert!(!durable_json.contains("pre-turn-secret"));
    assert!(!durable_json.contains(RAW_PROMPT));
    Ok(())
}

#[test]
fn queued_portable_preflight_with_no_prior_history_is_read_only_and_not_ready() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = root_config(temp.path(), &temp.path().join("cache"));
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone()));
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let session = session.expect("store-backed session should remain available");
    let before_stream = std::fs::read(store.path())?;
    let preparation = prepare_next_queued_conversation_candidate(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
    )
    .map_err(anyhow::Error::msg)?;
    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("queued chat should materialize an exact pre-turn candidate");
    };

    assert!(
        prepare_queued_portable_preflight(
            &root_config,
            temp.path(),
            store.path(),
            &session,
            &MemoryConfig { enabled: false },
            *candidate,
        )?
        .is_none()
    );
    assert_eq!(std::fs::read(store.path())?, before_stream);
    Ok(())
}

#[test]
fn queued_portable_preflight_with_foldable_history_never_starts_without_verified_target_proof()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut root_config = root_config(temp.path(), &temp.path().join("cache"));
    root_config.compaction.tail_messages = 1;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone()));
    {
        let stream = session
            .as_mut()
            .expect("store-backed session should remain available");
        stream.append_user_message(ModelMessage::user("older request"))?;
        stream.append_assistant_message(ModelMessage::assistant(
            Some("older response".to_owned()),
            Vec::new(),
        ))?;
        stream.append_user_message(ModelMessage::user("latest durable request"))?;
    }
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let session = session.expect("store-backed session should remain available");
    let before_stream = std::fs::read(store.path())?;
    let preparation = prepare_next_queued_conversation_candidate(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
    )
    .map_err(anyhow::Error::msg)?;
    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("queued chat should materialize an exact pre-turn candidate");
    };

    let error = prepare_queued_portable_preflight(
        &root_config,
        temp.path(),
        store.path(),
        &session,
        &MemoryConfig { enabled: false },
        *candidate,
    )
    .expect_err("missing local target proof must block pre-turn compaction");

    assert!(
        error
            .to_string()
            .contains("could not resolve DeepSeek transport for portable compaction"),
        "unexpected target-proof failure: {error:#}"
    );
    assert_eq!(std::fs::read(store.path())?, before_stream);
    let durable_json = String::from_utf8(before_stream)?;
    assert!(!durable_json.contains("compaction_started"));
    assert!(!durable_json.contains("pre_turn_pressure"));
    Ok(())
}
