use anyhow::Result;
use sigil_kernel::{ProviderCapabilities, ReasoningStreamSupport, Session};

use super::{
    super::{
        WorkerCommand,
        worker_loop::{WorkerCommandDomain, WorkerLoopState, classify_worker_command},
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
