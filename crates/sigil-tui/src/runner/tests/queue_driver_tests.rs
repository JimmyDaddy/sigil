use anyhow::Result;
use sigil_kernel::{
    DurableAuditRecord, DurableAuditWriter, DurableEventType,
    PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptPurpose, ProviderPhysicalAttemptStartedEntry,
    ProviderPhysicalAttemptTerminalEntry,
};

use super::*;

const RAW_PROMPT: &str = "inspect https://example.com/private?signature=queue-secret-value exactly";

fn append_queue_physical_attempt(
    store: &JsonlSessionStore,
    logical_run_id: &str,
    request_material_fingerprint: &str,
    outcome: Option<ProviderPhysicalAttemptOutcome>,
) -> Result<()> {
    let physical_attempt_id = "queue-provider-attempt-1";
    let started_event_id = "queue-provider-attempt-start";
    let started = ProviderPhysicalAttemptStartedEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: physical_attempt_id.to_owned(),
        logical_run_id: logical_run_id.to_owned(),
        purpose: ProviderPhysicalAttemptPurpose::ConversationGeneration,
        request_material_fingerprint: request_material_fingerprint.to_owned(),
        provider_name: "test".to_owned(),
        model_name: "model".to_owned(),
        started_at_unix_ms: 1,
    };
    let started_record = DurableAuditRecord::new(
        DurableEventType::ProviderPhysicalAttemptStarted,
        serde_json::to_value(started)?,
        physical_attempt_id,
        Some(started_event_id.to_owned()),
    )?
    .with_event_id(started_event_id)?;
    store.append_and_sync(started_record)?;

    if let Some(outcome) = outcome {
        let terminal = ProviderPhysicalAttemptTerminalEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: physical_attempt_id.to_owned(),
            request_material_fingerprint: request_material_fingerprint.to_owned(),
            outcome,
            rejection: None,
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
            Some(started_event_id.to_owned()),
        )?
        .with_event_id("queue-provider-attempt-terminal")?
        .with_causation_id(started_event_id)?;
        store.append_and_sync(terminal_record)?;
    }
    Ok(())
}

fn committed_queued_chat_candidate(
    temp: &tempfile::TempDir,
    store: &JsonlSessionStore,
) -> Result<(Session, String, String)> {
    let mut session = Some(Session::new("test", "model").with_store(store.clone()));
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
    let mut session = session.expect("store-backed session should remain available");
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
        panic!("queued chat should materialize a candidate");
    };
    let dispatch_run_id = candidate.promotion.dispatch_run_id.clone();
    let frozen_fingerprint = candidate.frozen_request.fingerprint().to_owned();
    commit_prepared_queued_conversation_candidate(store.path(), &mut session, *candidate)
        .map_err(anyhow::Error::msg)?;
    let restored = Session::load_from_store("test", "model", store.clone())?;
    Ok((restored, dispatch_run_id, frozen_fingerprint))
}

#[test]
fn sensitive_queue_prompt_is_safe_at_rest_but_exact_at_same_process_dispatch() {
    let temp = tempfile::tempdir().expect("temporary queue store should create");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))
        .expect("queue store should create");
    let mut session = Some(Session::new("test", "model").with_store(store.clone()));
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
    .expect("sensitive follow-up should queue");

    let durable_json = std::fs::read_to_string(store.path()).expect("queue stream should read");
    assert!(!durable_json.contains("queue-secret-value"));
    assert!(!durable_json.contains(RAW_PROMPT));
    assert!(durable_json.contains(EXACT_PROMPT_REQUIRED_HASH_PREFIX));

    let session = session.expect("session should remain available");
    let preparation = prepare_next_queued_conversation_candidate(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
    )
    .expect("same-process admission should retain exact material");
    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("same-process queue input should produce an exact candidate");
    };
    assert!(
        candidate
            .frozen_request
            .request()
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some(RAW_PROMPT))
    );
    assert!(exact_prompts.contains_key(&candidate.promotion.queue_id));
}

#[test]
fn sensitive_queue_prompt_without_process_local_exact_material_becomes_stale() {
    let temp = tempfile::tempdir().expect("temporary queue store should create");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))
        .expect("queue store should create");
    let mut original = Some(Session::new("test", "model").with_store(store.clone()));
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut original,
        &mut exact_prompts,
        RAW_PROMPT.to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .expect("sensitive follow-up should queue");

    let _ = original.expect("session should remain available");
    let restored = Session::load_from_store("test", "model", store.clone())
        .expect("queue session should reload");
    let restored_exact_prompts = ExactConversationPromptStore::new();
    let preparation = prepare_next_queued_conversation_candidate(
        &restored,
        &restored_exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
    )
    .expect("restart admission should be evaluated");
    assert!(matches!(
        preparation,
        QueuedConversationCandidatePreparation::Blocked { ref reason, .. }
            if reason == "exact sensitive follow-up was lost after restart"
    ));
    let durable_json = std::fs::read_to_string(store.path()).expect("queue stream should read");
    assert!(!durable_json.contains("queue-secret-value"));
    assert!(!durable_json.contains(RAW_PROMPT));
}

#[test]
fn queued_chat_candidate_freezes_exact_request_without_mutating_durable_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("test", "model").with_store(store.clone()));
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

    let candidate = prepare_next_queued_conversation_candidate(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        Some(ReasoningEffort::Low),
        Some("test-partition".to_owned()),
    )
    .map_err(anyhow::Error::msg)?;

    let QueuedConversationCandidatePreparation::Prepared(candidate) = candidate else {
        panic!("queued chat should produce a frozen candidate");
    };
    assert_eq!(candidate.promotion.queue_id.as_str(), "queue_1");
    assert!(candidate.promotion.exact_prompt_required);
    assert_eq!(
        candidate.promotion.durable_user_message.content.as_deref(),
        Some("inspect https://example.com/private?[redacted] exactly")
    );
    assert!(
        candidate
            .frozen_request
            .request()
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some(RAW_PROMPT))
    );
    assert_eq!(candidate.frozen_request.request().max_tokens, None);
    assert_eq!(
        candidate.frozen_request.request().reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert!(!candidate.capability_registrations.is_empty());
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(exact_prompts.contains_key(&candidate.promotion.queue_id));
    let durable_json = String::from_utf8(before_stream)?;
    assert!(!durable_json.contains("queue-secret-value"));
    assert!(!durable_json.contains(RAW_PROMPT));
    Ok(())
}

#[test]
fn queued_candidate_commit_promotes_once_and_persists_only_safe_user_material() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("test", "model").with_store(store.clone()));
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
    let mut session = session.expect("store-backed session should remain available");
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
        panic!("queued chat should materialize a candidate");
    };
    let frozen_fingerprint = candidate.frozen_request.fingerprint().to_owned();
    let promoted_message_id = candidate.promotion.durable_user_message.id.clone();
    let candidate =
        commit_prepared_queued_conversation_candidate(store.path(), &mut session, *candidate)
            .map_err(anyhow::Error::msg)?;

    assert_eq!(candidate.frozen_request.fingerprint(), frozen_fingerprint);
    let restored = Session::load_from_store("test", "model", store.clone())?;
    assert_eq!(
        restored
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::User(message) if message.id == promoted_message_id
            ))
            .count(),
        1
    );
    assert_eq!(
        restored
            .conversation_queue_projection()
            .items
            .first()
            .map(|item| item.status),
        Some(ConversationInputStatus::Dispatching)
    );
    let durable_json = std::fs::read_to_string(store.path())?;
    assert!(!durable_json.contains("queue-secret-value"));
    assert!(!durable_json.contains(RAW_PROMPT));
    assert!(durable_json.contains("conversation_input_promoted"));
    Ok(())
}

#[test]
fn queued_candidate_commit_rejects_a_stale_queue_revision_before_promotion() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut current_session = Some(Session::new("test", "model").with_store(store.clone()));
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut current_session,
        &mut exact_prompts,
        "first safe queued prompt".to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let preparation = prepare_next_queued_conversation_candidate(
        current_session
            .as_ref()
            .expect("queued fixture keeps a session"),
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
    )
    .map_err(anyhow::Error::msg)?;
    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("first queued item should produce a candidate");
    };

    queue_conversation_input(
        store.path(),
        &mut current_session,
        &mut exact_prompts,
        "second safe queued prompt".to_owned(),
        ConversationInputKind::Chat,
        ConversationInputTarget::MainThread,
        ReasoningEffort::High,
    )
    .map_err(anyhow::Error::msg)?;
    let error = commit_prepared_queued_conversation_candidate(
        store.path(),
        current_session
            .as_mut()
            .expect("queued fixture keeps a session"),
        *candidate,
    )
    .expect_err("a changed queue revision must reject the prepared candidate");
    assert!(
        error.contains("queue revision"),
        "unexpected error: {error}"
    );

    let durable_json = std::fs::read_to_string(store.path())?;
    assert!(!durable_json.contains("conversation_input_promoted"));
    assert_eq!(
        current_session
            .as_ref()
            .expect("queued fixture keeps a session")
            .conversation_queue_projection()
            .items
            .len(),
        2
    );
    Ok(())
}

#[test]
fn promoted_queue_without_provider_attempt_is_rejected_on_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (restored, _, _) = committed_queued_chat_candidate(&temp, &store)?;
    let attempts = restored.provider_physical_attempt_projection()?;

    assert!(matches!(
        classify_promoted_queued_conversation(
            &restored,
            &attempts,
            &ConversationInputQueueId::new("queue_1")?,
        )
        .map_err(anyhow::Error::msg)?,
        QueuedConversationTerminalClassification::Rejected { .. }
    ));
    Ok(())
}

#[test]
fn promoted_queue_recovery_classifies_physical_attempt_terminals_without_replay() -> Result<()> {
    for outcome in [
        ProviderPhysicalAttemptOutcome::Completed,
        ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect,
        ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput,
        ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
        ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
        ProviderPhysicalAttemptOutcome::Interrupted,
    ] {
        let temp = tempfile::tempdir()?;
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let (_, dispatch_run_id, frozen_fingerprint) =
            committed_queued_chat_candidate(&temp, &store)?;
        append_queue_physical_attempt(
            &store,
            &dispatch_run_id,
            &frozen_fingerprint,
            Some(outcome),
        )?;
        let restored = Session::load_from_store("test", "model", store.clone())?;
        let attempts = restored.provider_physical_attempt_projection()?;
        let classification = classify_promoted_queued_conversation(
            &restored,
            &attempts,
            &ConversationInputQueueId::new("queue_1")?,
        )
        .map_err(anyhow::Error::msg)?;
        match outcome {
            ProviderPhysicalAttemptOutcome::Completed
            | ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect
            | ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput => {
                assert!(matches!(
                    classification,
                    QueuedConversationTerminalClassification::Delivered { .. }
                ));
            }
            ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption => {
                assert!(matches!(
                    classification,
                    QueuedConversationTerminalClassification::Rejected { .. }
                ));
            }
            ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain
            | ProviderPhysicalAttemptOutcome::Interrupted => {
                assert!(matches!(
                    classification,
                    QueuedConversationTerminalClassification::Stale { .. }
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn recovery_marks_completed_promoted_queue_delivered_without_resending() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (_, dispatch_run_id, frozen_fingerprint) = committed_queued_chat_candidate(&temp, &store)?;
    append_queue_physical_attempt(
        &store,
        &dispatch_run_id,
        &frozen_fingerprint,
        Some(ProviderPhysicalAttemptOutcome::Completed),
    )?;
    let mut restored = Session::load_from_store("test", "model", store.clone())?;
    let (message_tx, message_rx) = std::sync::mpsc::channel();

    mark_stale_dispatching_conversation_queue_items(
        &mut restored,
        &ExactConversationPromptStore::new(),
        &message_tx,
    );

    let update = message_rx.recv()?;
    assert!(matches!(
        update,
        WorkerMessage::ConversationQueueUpdated { ref items, .. }
            if items.is_empty()
    ));
    let reloaded = Session::load_from_store("test", "model", store)?;
    assert!(reloaded.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.queue_id.as_str() == "queue_1"
                && status.status == ConversationInputStatus::Delivered
    )));
    Ok(())
}

#[test]
fn promoted_queue_with_unfinished_provider_attempt_stays_stale_on_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (_, dispatch_run_id, frozen_fingerprint) = committed_queued_chat_candidate(&temp, &store)?;
    append_queue_physical_attempt(&store, &dispatch_run_id, &frozen_fingerprint, None)?;
    let restored = Session::load_from_store("test", "model", store.clone())?;
    let attempts = restored.provider_physical_attempt_projection()?;

    assert!(matches!(
        classify_promoted_queued_conversation(
            &restored,
            &attempts,
            &ConversationInputQueueId::new("queue_1")?,
        )
        .map_err(anyhow::Error::msg)?,
        QueuedConversationTerminalClassification::Stale { .. }
    ));
    Ok(())
}

#[test]
fn queued_pressure_candidate_binds_explicit_output_reservation_without_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
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

    let preparation = prepare_next_queued_conversation_candidate_with_target_max_tokens(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        Some(sigil_runtime::deepseek_v4_flash_portable_target_output_tokens()),
        Some(ReasoningEffort::Low),
        None,
        |_| RuntimeContextCandidates::default(),
    )
    .map_err(anyhow::Error::msg)?;

    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("queued DeepSeek V4 chat should produce a reserved candidate");
    };
    assert_eq!(
        candidate.frozen_request.request().max_tokens,
        Some(sigil_runtime::deepseek_v4_flash_portable_target_output_tokens())
    );
    assert!(
        candidate
            .frozen_request
            .request()
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some(RAW_PROMPT))
    );
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(exact_prompts.contains_key(&candidate.promotion.queue_id));
    Ok(())
}

#[test]
fn queued_candidate_freezes_context_v1_for_the_exact_prompt_without_durable_leak() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("test", "model").with_store(store.clone()));
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
    let body = "export function queuedContextV1() { return true; }";
    let item = sigil_kernel::ContextItem {
        id: "repo-file:src/context.ts".to_owned(),
        source: sigil_kernel::ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: sigil_kernel::ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: sigil_kernel::ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: Some("snapshot-queued".to_owned()),
        token_cost: 12,
        score: Some(100.0),
        score_breakdown: Vec::new(),
        inclusion_reason: sigil_kernel::ContextInclusionReason::RetrievalHit,
        body_ref: sigil_kernel::ContextBodyRef::inline(body),
    };
    let mut expected_context = RuntimeContextCandidates::new();
    expected_context
        .snippets
        .insert(item.id.clone(), body.to_owned());
    expected_context.items.push(item);
    let resolved_context = expected_context.clone();

    let preparation = prepare_next_queued_conversation_candidate_with_target_max_tokens(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
        move |query| {
            assert_eq!(query, RAW_PROMPT);
            resolved_context
        },
    )
    .map_err(anyhow::Error::msg)?;

    let QueuedConversationCandidatePreparation::Prepared(candidate) = preparation else {
        panic!("queued chat should produce a frozen candidate");
    };
    assert_eq!(candidate.runtime_context, expected_context);
    let request_text = candidate
        .frozen_request
        .request()
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(request_text.contains("sigil_context_v1"));
    assert!(request_text.contains("warm_lsp_then_request_local_tree_sitter"));
    assert!(request_text.contains(body));
    assert!(!format!("{candidate:?}").contains(body));
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(!String::from_utf8(before_stream)?.contains(body));
    Ok(())
}

#[test]
fn queued_pressure_admission_blocks_without_verified_local_tokenizer_without_mutation() -> Result<()>
{
    let temp = tempfile::tempdir()?;
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

    let admission = prepare_next_queued_conversation_pressure_admission(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        temp.path(),
    )
    .map_err(anyhow::Error::msg)?;

    assert!(matches!(
        admission,
        QueuedConversationPressureAdmission::Blocked {
            ref reason,
            candidate: Some(_),
            ..
        }
            if reason == "queued pre-turn exact admission is unavailable from the local token profile"
    ));
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(exact_prompts.contains_key(&ConversationInputQueueId::new("queue_1")?));
    let durable_json = String::from_utf8(before_stream)?;
    assert!(!durable_json.contains("queue-secret-value"));
    assert!(!durable_json.contains(RAW_PROMPT));
    Ok(())
}

#[test]
fn queued_pressure_admission_blocks_unadmitted_profile_without_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("openai_compat", "gpt-test").with_store(store.clone()));
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

    let admission = prepare_next_queued_conversation_pressure_admission(
        &session,
        &exact_prompts,
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        temp.path(),
    )
    .map_err(anyhow::Error::msg)?;

    assert!(matches!(
        admission,
        QueuedConversationPressureAdmission::Blocked {
            ref reason,
            candidate: Some(_),
            ..
        }
            if reason == "queued pre-turn exact admission is unavailable for this provider/model"
    ));
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert!(exact_prompts.contains_key(&ConversationInputQueueId::new("queue_1")?));
    Ok(())
}

#[test]
fn queued_plan_candidate_is_blocked_without_changing_queue_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Some(Session::new("test", "model").with_store(store.clone()));
    let mut exact_prompts = ExactConversationPromptStore::new();
    queue_conversation_input(
        store.path(),
        &mut session,
        &mut exact_prompts,
        "prepare a plan".to_owned(),
        ConversationInputKind::PlanPrompt,
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

    assert!(matches!(
        preparation,
        QueuedConversationCandidatePreparation::Blocked { ref reason, .. }
            if reason == "queued pre-turn admission is not available for this follow-up kind"
    ));
    assert_eq!(std::fs::read(store.path())?, before_stream);
    assert_eq!(
        session.conversation_queue_projection().next_dispatchable,
        Some(ConversationInputQueueId::new("queue_1")?)
    );
    Ok(())
}
