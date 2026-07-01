use std::{collections::BTreeMap, sync::mpsc};

use sigil_kernel::{
    AgentConfig, CodeIntelligenceConfig, CompactionConfig, ContextSource, ControlEntry,
    DurableEventType, JsonlSessionStore, McpServerConfig, MemoryConfig,
    MutationArtifactCleanupRequested, MutationArtifactLifecycleRecorded,
    MutationArtifactLifecycleStatus, MutationEventRecorder, PermissionConfig, RootConfig, RunEvent,
    Session, SessionConfig, SessionStreamRecord, StorageConfig, TaskConfig, TaskId, ToolEffect,
    VerificationCheckConfig, VerificationConfig, WorkspaceConfig, WorkspaceTrust,
    WorkspaceTrustDecisionEntry, bytes_hash, config::TerminalConfig, stable_workspace_id,
};

use crate::runner::{
    event_bridge::ChannelEventHandler,
    protocol::WorkerMessage,
    worker_loop::{
        VerificationCheckPromotionKind, VerificationCheckPromotionOutcome,
        chat_agent_run_input_with_repo_context, clean_mutation_artifacts, delete_mutation_artifact,
        materialize_task_verification_config, promote_workspace_verification_check,
    },
};

#[test]
fn chat_agent_run_input_with_repo_context_attaches_repository_candidates() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("README.md"),
        "Sigil is a TUI-first Rust coding agent.",
    )
    .expect("write README");

    let input = chat_agent_run_input_with_repo_context(
        temp.path(),
        "summarize README.md".to_owned(),
        false,
        Vec::new(),
    );

    assert!(input.persisted_user_message.is_some());
    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "repo-file:README.md" && matches!(item.source, ContextSource::RepositoryFile)
    }));
}

#[test]
fn chat_agent_run_input_with_repo_context_preserves_plan_mode_transience() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("README.md"), "plan context").expect("write README");

    let input = chat_agent_run_input_with_repo_context(
        temp.path(),
        "plan from README.md".to_owned(),
        true,
        Vec::new(),
    );

    assert!(input.persisted_user_message.is_none());
    assert!(input.runtime_context.items.iter().any(|item| {
        item.id == "repo-file:README.md" && matches!(item.source, ContextSource::RepositoryFile)
    }));
}

#[test]
fn materialize_task_verification_config_records_specs_policy_and_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut root_config = root_config_with_checks(
        temp.path(),
        vec![VerificationCheckConfig {
            id: "cargo-test".to_owned(),
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
    );
    root_config.verification.scope_profile = sigil_kernel::VerificationScopeProfile::Node;
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("config materializes");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(
        projection
            .check_spec(&scope, "cargo-test")
            .is_some_and(|entry| entry.trusted_check.source
                == sigil_kernel::CheckDiscoverySource::UserExplicitConfig)
    );
    assert!(projection.latest_policy(&scope).is_some_and(|entry| {
        entry.policy.required_checks.len() == 1
            && entry.policy.workspace_trust_requirement
                == sigil_kernel::WorkspaceTrustRequirement::None
            && entry
                .policy
                .verification_scope
                .exclude
                .contains(&".next/**".to_owned())
    }));
    let controls = rx
        .try_iter()
        .filter_map(|message| match message {
            WorkerMessage::Event(event) => match *event {
                RunEvent::Control(control) => Some(control),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        controls.as_slice(),
        [
            ControlEntry::CheckSpecRecorded(_),
            ControlEntry::VerificationPolicyChanged(_)
        ]
    ));

    let (tx, _rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("idempotent config materializes");
    let control_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
                    | sigil_kernel::SessionLogEntry::Control(
                        ControlEntry::VerificationPolicyChanged(_)
                    )
            )
        })
        .count();
    assert_eq!(control_count, 2);
}

#[test]
fn materialize_task_verification_config_skips_inapplicable_user_cargo_check() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let root_config = root_config_with_checks(
        temp.path(),
        vec![VerificationCheckConfig {
            id: "kernel-verification".to_owned(),
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
                "verification".to_owned(),
            ],
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
    );
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("config materializes");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(
        projection
            .check_spec(&scope, "kernel-verification")
            .is_none()
    );
    assert!(projection.latest_policy(&scope).is_none());
    assert!(rx.try_iter().next().is_none());
}

#[test]
fn materialize_task_verification_config_does_not_promote_repo_checks_without_trust() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("repo discovery should not fail");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(projection.check_spec(&scope, "cargo-test").is_none());
    assert!(projection.latest_policy(&scope).is_none());
    assert!(rx.try_iter().next().is_none());
}

#[test]
fn materialize_task_verification_config_does_not_require_repo_checks_for_trusted_workspace() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let workspace_id = stable_workspace_id(temp.path()).expect("workspace id");
    session
        .append_control(ControlEntry::WorkspaceTrustDecision(
            WorkspaceTrustDecisionEntry {
                workspace_id,
                workspace_trust_snapshot_id: "trust-1".to_owned(),
                trust: WorkspaceTrust::Trusted,
                decided_by_event_id: Some("event-trust".to_owned()),
                reason: Some("test trusted workspace".to_owned()),
            },
        ))
        .expect("append trust decision");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("trusted repo checks materialize");

    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    assert!(projection.check_spec(&scope, "cargo-test").is_none());
    assert!(projection.latest_policy(&scope).is_none());
    assert!(rx.try_iter().next().is_none());
}

#[test]
fn materialize_task_verification_config_uses_workspace_check_promotion() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let mut current_session = Some(Session::new("deepseek", "deepseek-v4-flash"));

    let promoted = promote_workspace_verification_check(
        temp.path(),
        &root_config,
        &mut current_session,
        "cargo-test",
        VerificationCheckPromotionKind::Approve,
    )
    .expect("approve repo-local check");
    let entry = match promoted {
        VerificationCheckPromotionOutcome::Promoted { entry } => *entry,
        VerificationCheckPromotionOutcome::AlreadyPromoted { .. } => {
            panic!("first approval should append a check spec")
        }
    };
    assert!(matches!(
        entry.scope,
        sigil_kernel::EvidenceScope::Workspace(_)
    ));
    assert!(matches!(
        entry.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::UserApproved { .. }
    ));

    let mut session = current_session.expect("session");
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);
    let task_id = TaskId::new("task-1").expect("task id");

    materialize_task_verification_config(
        &mut session,
        &mut handler,
        &root_config,
        temp.path(),
        &task_id,
    )
    .expect("approved repo check materializes");

    let projection = session.verification_state_projection();
    let task_scope = sigil_kernel::EvidenceScope::Task("task-1".to_owned());
    let check = projection
        .check_spec(&task_scope, "cargo-test")
        .expect("approved workspace check should materialize into task");
    assert!(matches!(
        check.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::UserApproved { .. }
    ));
    assert!(
        projection
            .latest_policy(&task_scope)
            .is_some_and(|entry| entry.policy.workspace_trust_requirement
                == sigil_kernel::WorkspaceTrustRequirement::ApprovalOrSandbox)
    );
    let controls = rx
        .try_iter()
        .filter_map(|message| match message {
            WorkerMessage::Event(event) => match *event {
                RunEvent::Control(control) => Some(control),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        controls.as_slice(),
        [
            ControlEntry::CheckSpecRecorded(_),
            ControlEntry::VerificationPolicyChanged(_)
        ]
    ));
}

#[test]
fn promote_workspace_verification_check_supports_sandbox_and_idempotence() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let root_config = root_config_with_checks(temp.path(), Vec::new());
    let mut current_session = Some(Session::new("deepseek", "deepseek-v4-flash"));

    let promoted = promote_workspace_verification_check(
        temp.path(),
        &root_config,
        &mut current_session,
        "cargo-test",
        VerificationCheckPromotionKind::Sandbox,
    )
    .expect("sandbox repo-local check");
    let VerificationCheckPromotionOutcome::Promoted { entry } = promoted else {
        panic!("sandbox promotion should append a check spec");
    };
    assert!(matches!(
        entry.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::Sandboxed { .. }
    ));

    let repeated = promote_workspace_verification_check(
        temp.path(),
        &root_config,
        &mut current_session,
        "cargo-test",
        VerificationCheckPromotionKind::Sandbox,
    )
    .expect("idempotent sandbox promotion");
    assert!(matches!(
        repeated,
        VerificationCheckPromotionOutcome::AlreadyPromoted { ref check_spec_id }
            if check_spec_id == "cargo-test"
    ));
}

#[test]
fn clean_mutation_artifacts_applies_retention_policy_and_records_lifecycle() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let session_path = temp.path().join("sessions/session.jsonl");
    let store = JsonlSessionStore::new(session_path.clone())?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-cleanup", None)?;
    let new_content = b"new";
    let prepared =
        coordinator.prepare_file("note.txt", target.clone(), Some(bytes_hash(new_content)))?;
    coordinator.commit_write(&prepared, new_content)?;

    let mut root_config = root_config_with_checks(&workspace, Vec::new());
    root_config
        .storage
        .mutation_artifact_retention
        .max_artifacts = Some(0);
    root_config.storage.mutation_artifact_retention.max_bytes = None;
    root_config
        .storage
        .mutation_artifact_retention
        .expire_older_than_ms = None;
    let current_session =
        Some(Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone()));

    let report = clean_mutation_artifacts(
        &root_config,
        &session_path,
        &current_session,
        &sigil_kernel::MutationArtifactCleanupTarget::Recommended,
    )
    .expect("cleanup should apply retention");

    assert_eq!(report.scanned_artifacts, 1);
    assert_eq!(report.expired_artifacts, 1);
    assert_eq!(report.unavailable_artifacts, 0);
    assert_eq!(report.lifecycle_events.len(), 1);
    let cleanup_requests = JsonlSessionStore::read_event_records(&session_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::MutationArtifactCleanupRequested.as_str() =>
            {
                Some(
                    serde_json::from_value::<MutationArtifactCleanupRequested>(event.payload)
                        .expect("cleanup request payload should decode"),
                )
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        cleanup_requests.as_slice(),
        [MutationArtifactCleanupRequested {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
            candidate_artifacts: 1,
            ..
        }]
    ));
    let lifecycle_records = JsonlSessionStore::read_event_records(&session_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::MutationArtifactLifecycleRecorded.as_str() =>
            {
                Some(
                    serde_json::from_value::<MutationArtifactLifecycleRecorded>(event.payload)
                        .expect("lifecycle payload should decode"),
                )
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        lifecycle_records.as_slice(),
        [MutationArtifactLifecycleRecorded {
            status: MutationArtifactLifecycleStatus::Expired,
            ..
        }]
    ));
    assert_eq!(
        lifecycle_records[0].reason.as_str(),
        "retention quota limit"
    );
    Ok(())
}

#[test]
fn delete_mutation_artifact_records_user_requested_lifecycle() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let session_path = temp.path().join("sessions/session.jsonl");
    let store = JsonlSessionStore::new(session_path.clone())?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-delete", None)?;
    let new_content = b"new";
    let prepared =
        coordinator.prepare_file("note.txt", target.clone(), Some(bytes_hash(new_content)))?;
    coordinator.commit_write(&prepared, new_content)?;
    let artifact_id = recorder
        .list_mutation_artifacts()?
        .into_iter()
        .next()
        .expect("artifact should exist")
        .artifact_id;
    let current_session =
        Some(Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone()));

    let payload = delete_mutation_artifact(&session_path, &current_session, &artifact_id)
        .expect("artifact deletion should record lifecycle");

    assert_eq!(payload.artifact_id, artifact_id);
    assert_eq!(payload.status, MutationArtifactLifecycleStatus::Deleted);
    assert_eq!(payload.reason, "user requested artifact deletion");
    assert!(recorder.list_mutation_artifacts()?.is_empty());
    Ok(())
}

fn root_config_with_checks(
    workspace_root: &std::path::Path,
    checks: Vec<VerificationCheckConfig>,
) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: StorageConfig::default(),
        session: SessionConfig::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: false },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: CodeIntelligenceConfig::default(),
        terminal: TerminalConfig::default(),
        execution: Default::default(),
        verification: VerificationConfig {
            auto_run: sigil_kernel::VerificationAutoRunPolicy::Manual,
            checks,
            ..VerificationConfig::default()
        },
        appearance: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::<McpServerConfig>::new(),
    }
}
