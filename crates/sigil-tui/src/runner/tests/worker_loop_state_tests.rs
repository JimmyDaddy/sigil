use anyhow::Result;
use sigil_kernel::{
    ControlEntry, ConversationInputQueueId, JsonlSessionStore, ProviderCapabilities,
    ReasoningStreamSupport, SecretString, Session, SessionLogEntry,
};

use super::{
    super::{
        WorkerCommand, WorkerMessage,
        worker_loop::{
            SessionTransitionKind, WorkerCommandDomain, WorkerLoopState, classify_worker_command,
            transition_session,
        },
    },
    common::test_root_config,
};

fn provider_capabilities() -> ProviderCapabilities {
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

#[test]
fn worker_loop_state_initializes_domain_owners_from_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_log_path = temp.path().join("session.jsonl");
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let supervisor = sigil_runtime::AgentSupervisor::new(
        registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        provider_capabilities(),
    );
    let session = Session::new("planned", "planned-model");

    let state = WorkerLoopState::new(
        session_log_path.clone(),
        Some(session),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
    );

    assert_eq!(state.session.log_path, session_log_path);
    let current_session = state
        .session
        .current
        .as_ref()
        .expect("constructor should retain the supplied session");
    assert_eq!(current_session.provider_name(), "planned");
    assert_eq!(current_session.model_name(), "planned-model");
    assert!(state.run.active.is_none());
    assert_eq!(state.run.next_id, 1);
    assert!(state.run.discarded_ids.is_empty());
    assert!(state.compaction.pending.is_none());
    assert_eq!(state.compaction.next_request_id, 1);
    assert!(state.refresh.pending_mcp_servers.is_empty());
    assert!(state.processed_worker_command_ids.is_empty());
    Ok(())
}

#[test]
fn worker_commands_are_routed_to_explicit_domains() {
    let cases = [
        (WorkerCommand::CancelRun, WorkerCommandDomain::RunPlan),
        (
            WorkerCommand::StartNewSession {
                session_log_path: "new-session.jsonl".into(),
            },
            WorkerCommandDomain::Session,
        ),
        (
            WorkerCommand::PreviewV2Compaction,
            WorkerCommandDomain::QueueCompaction,
        ),
        (
            WorkerCommand::BackgroundActiveAgent,
            WorkerCommandDomain::AgentTask,
        ),
        (
            WorkerCommand::CheckChangedFilesDiagnostics,
            WorkerCommandDomain::VerificationCheckpoint,
        ),
        (
            WorkerCommand::CancelProviderModelsRefresh { request_id: 7 },
            WorkerCommandDomain::ProviderMcp,
        ),
        (WorkerCommand::Shutdown, WorkerCommandDomain::Maintenance),
    ];

    for (command, expected) in cases {
        assert_eq!(classify_worker_command(&command), expected);
    }
}

#[test]
fn detached_background_runs_block_session_transitions() {
    assert_eq!(
        SessionTransitionKind::Switch.block_reason(false, true),
        Some("cannot switch sessions while a background agent is running")
    );
    assert_eq!(
        SessionTransitionKind::StartNew.block_reason(false, true),
        Some("cannot start a new session while a background agent is running")
    );
}

#[test]
fn session_transition_rebuilds_session_scoped_worker_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let current_path = temp.path().join("current.jsonl");
    let target_path = temp.path().join("target.jsonl");
    let current_store = JsonlSessionStore::new(&current_path)?;
    let target_store = JsonlSessionStore::new(&target_path)?;
    target_store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "planned".to_owned(),
        model_name: "planned-model".to_owned(),
    }))?;
    let current_session = Session::new("planned", "planned-model").with_store(current_store);
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let supervisor = sigil_runtime::AgentSupervisor::new(
        registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        provider_capabilities(),
    );
    let mut state = WorkerLoopState::new(
        current_path,
        Some(current_session),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
    );
    let queue_id = ConversationInputQueueId::new("queue_1")?;
    state
        .session
        .exact_prompts
        .insert(queue_id.clone(), SecretString::new("private prompt"));
    state.session.last_queued_pre_turn_block = Some((queue_id, "blocked".to_owned()));
    state
        .session
        .pending_agent_result_continuations
        .push(sigil_kernel::AgentThreadId::new("agent_pending")?);
    state
        .compaction
        .idle_auto
        .request_after_successful_chat_run();
    let (message_tx, _message_rx) = std::sync::mpsc::channel();

    let message = transition_session(
        SessionTransitionKind::Switch,
        target_path.clone(),
        &root_config,
        temp.path(),
        &mut state,
        &message_tx,
    )
    .map_err(anyhow::Error::msg)?;

    assert!(matches!(
        message,
        WorkerMessage::SessionSwitched {
            session_log_path,
            ..
        } if session_log_path == target_path
    ));
    assert_eq!(state.session.log_path, target_path);
    assert!(state.session.exact_prompts.is_empty());
    assert!(state.session.last_queued_pre_turn_block.is_none());
    assert!(state.session.pending_agent_result_continuations.is_empty());
    assert!(!state.compaction.idle_auto.is_requested());

    let same_scope_queue_id = ConversationInputQueueId::new("queue_same_scope")?;
    state.session.exact_prompts.insert(
        same_scope_queue_id.clone(),
        SecretString::new("same scope prompt"),
    );
    transition_session(
        SessionTransitionKind::Switch,
        target_path.clone(),
        &root_config,
        temp.path(),
        &mut state,
        &message_tx,
    )
    .map_err(anyhow::Error::msg)?;
    assert!(
        state
            .session
            .exact_prompts
            .contains_key(&same_scope_queue_id)
    );

    let retained_block = Some((same_scope_queue_id, "retain on failure".to_owned()));
    state.session.last_queued_pre_turn_block = retained_block.clone();
    let invalid_path = temp.path().join("invalid-target");
    std::fs::create_dir(&invalid_path)?;
    assert!(
        transition_session(
            SessionTransitionKind::Switch,
            invalid_path,
            &root_config,
            temp.path(),
            &mut state,
            &message_tx,
        )
        .is_err()
    );
    assert_eq!(state.session.log_path, target_path);
    assert_eq!(state.session.last_queued_pre_turn_block, retained_block);
    Ok(())
}
