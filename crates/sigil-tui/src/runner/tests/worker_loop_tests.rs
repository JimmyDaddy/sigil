use std::{collections::BTreeMap, sync::mpsc};

use sigil_kernel::config::TerminalConfig;
use sigil_kernel::{
    AgentConfig, CodeIntelligenceConfig, CompactionConfig, ControlEntry, McpServerConfig,
    MemoryConfig, PermissionConfig, RootConfig, RunEvent, Session, SessionConfig, StorageConfig,
    TaskConfig, TaskId, ToolEffect, VerificationCheckConfig, VerificationConfig, WorkspaceConfig,
    WorkspaceTrust, WorkspaceTrustDecisionEntry, stable_workspace_id,
};

use crate::runner::{
    event_bridge::ChannelEventHandler,
    protocol::WorkerMessage,
    worker_loop::{TrustWorkspaceOutcome, materialize_task_verification_config, trust_workspace},
};

#[test]
fn materialize_task_verification_config_records_specs_policy_and_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let root_config = root_config_with_checks(
        temp.path(),
        vec![VerificationCheckConfig {
            id: "cargo-test".to_owned(),
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
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
            .check_spec(&scope, "cargo-test")
            .is_some_and(|entry| entry.trusted_check.source
                == sigil_kernel::CheckDiscoverySource::UserExplicitConfig)
    );
    assert!(
        projection
            .latest_policy(&scope)
            .is_some_and(|entry| entry.policy.required_checks.len() == 1)
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
fn materialize_task_verification_config_promotes_repo_checks_for_trusted_workspace() {
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
    let check = projection
        .check_spec(&scope, "cargo-test")
        .expect("trusted workspace should promote cargo check");
    assert_eq!(
        check.trusted_check.source,
        sigil_kernel::CheckDiscoverySource::Cargo
    );
    assert!(matches!(
        check.trusted_check.promoted_by,
        sigil_kernel::CheckPromotion::WorkspaceTrusted { .. }
    ));
    assert!(
        projection
            .latest_policy(&scope)
            .is_some_and(|entry| entry.policy.required_checks.len() == 1)
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
fn trust_workspace_records_decision_and_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace_id = stable_workspace_id(temp.path()).expect("workspace id");
    let mut current_session = Some(Session::new("deepseek", "deepseek-v4-flash"));

    let outcome = trust_workspace(temp.path(), &mut current_session).expect("trust workspace");

    let entry = match outcome {
        TrustWorkspaceOutcome::Trusted { entry } => entry,
        TrustWorkspaceOutcome::AlreadyTrusted { .. } => {
            panic!("first trust should append a decision")
        }
    };
    assert_eq!(entry.workspace_id, workspace_id);
    assert_eq!(entry.trust, WorkspaceTrust::Trusted);
    assert!(
        entry
            .workspace_trust_snapshot_id
            .starts_with("workspace-trust:")
    );
    assert_eq!(
        entry.reason.as_deref(),
        Some("trusted by user through /trust-workspace")
    );
    let session = current_session.as_ref().expect("session remains available");
    let projection = session.verification_state_projection();
    assert!(
        projection
            .workspace_trust
            .get(&workspace_id)
            .is_some_and(|entry| entry.trust == WorkspaceTrust::Trusted)
    );
    let trust_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(_))
            )
        })
        .count();
    assert_eq!(trust_count, 1);

    let second = trust_workspace(temp.path(), &mut current_session)
        .expect("trust workspace remains idempotent");

    assert!(matches!(
        second,
        TrustWorkspaceOutcome::AlreadyTrusted { workspace_id: ref id } if id == &workspace_id
    ));
    let session = current_session.as_ref().expect("session remains available");
    let trust_count = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(_))
            )
        })
        .count();
    assert_eq!(trust_count, 1);
}

#[test]
fn trust_workspace_requires_available_session() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut current_session = None;

    let error = trust_workspace(temp.path(), &mut current_session).expect_err("missing session");

    assert!(error.contains("session state is unavailable"));
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
        verification: VerificationConfig { checks },
        appearance: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::<McpServerConfig>::new(),
    }
}
