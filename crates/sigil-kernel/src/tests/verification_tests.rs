use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, anyhow};

use crate::{
    CandidateCheck, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, ChildVerificationReceiptLinked, CompletionCriteria, DurableEventType,
    EvidenceReceipt, EvidenceScope, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionFuture, ExecutionNetworkPolicy, ExecutionNetworkReceipt,
    ExecutionReceipt, ExecutionRequest, FileMetadataPlatform, FileType, JsonlSessionStore,
    MAX_WORKSPACE_SNAPSHOT_FILE_BYTES, PluginHookExecutionFinishedEntry,
    PluginHookExecutionStartedEntry, PluginHookExecutionStatus, PluginHookKind,
    PluginHookOutputArtifactRef, PluginHookOutputEnvelope, PluginHookOutputStream,
    PluginVerificationHookReceiptRequest, ReadinessInput, ReadinessProjectionMode, ReadinessReason,
    ReceiptStatus, RedactionState, RequiredAction, RunStatus, SandboxProfileRequirement, Session,
    SessionLogEntry, SessionStreamRecord, SnapshotEntryState, ToolEffect,
    VerificationAutoRunPolicy, VerificationBinding, VerificationCheckConfig,
    VerificationCheckRunEntry, VerificationCheckRunRequest, VerificationCheckRunStatus,
    VerificationConfig, VerificationPolicy, VerificationReceipt, VerificationScope,
    VerificationSkipDecision, VerificationStaleCause, VerificationStaleReason,
    VerificationStateProjection, VerificationVerdict, VisibleCompletionState, WorkspaceKnowledge,
    WorkspaceMutationDetected, WorkspaceMutationDetectionReason, WorkspaceMutationEvidence,
    WorkspaceSnapshotEntry, WorkspaceSnapshotManifestV1, WorkspaceTrust, WorkspaceTrustRequirement,
    build_workspace_snapshot, build_workspace_snapshot_for_event, check_specs_from_user_config,
    discover_candidate_checks, discover_candidate_checks_with_user_config, evaluate_readiness,
    record_plugin_verification_hook_receipt, run_verification_check, session::ControlEntry,
};

#[derive(Debug, Default)]
struct FakeVerificationBackend;

impl ExecutionBackend for FakeVerificationBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        Box::pin(async move {
            if request.program.contains("definitely-missing")
                || request.cwd.ends_with("missing-cwd")
            {
                return Err(anyhow!("fake spawn failed for {}", request.program));
            }
            let joined_args = request.args.join(" ");
            let timed_out = request.timeout_ms == Some(1) && joined_args.contains("sleep");
            let failed = joined_args.contains("--definitely-not-a-real-rustc-flag");
            Ok(ExecutionReceipt {
                backend: ExecutionBackendKind::Local,
                capabilities: ExecutionBackendCapabilities::default(),
                network: ExecutionNetworkReceipt::unknown(
                    "fake local backend does not report network enforcement",
                ),
                resources: Default::default(),
                exit_code: if timed_out {
                    None
                } else if failed {
                    Some(1)
                } else {
                    Some(0)
                },
                stdout: format!("fake backend executed {}\n", request.program).into_bytes(),
                stderr: if failed {
                    b"fake verification failure\n".to_vec()
                } else {
                    Vec::new()
                },
                timed_out,
            })
        })
    }
}

#[derive(Debug, Default)]
struct FakeSandboxVerificationBackend;

impl ExecutionBackend for FakeSandboxVerificationBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::MacosSeatbelt
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities {
            filesystem_isolation: true,
            network_isolation: true,
            process_isolation: true,
            resource_limits: false,
            persistent_pty: false,
            workspace_snapshot: false,
        }
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        let capabilities = self.capabilities();
        Box::pin(async move {
            Ok(ExecutionReceipt {
                backend: ExecutionBackendKind::MacosSeatbelt,
                capabilities,
                network: ExecutionNetworkReceipt::denied("fake sandbox denied network"),
                resources: Default::default(),
                exit_code: Some(0),
                stdout: format!("fake sandbox executed {}\n", request.program).into_bytes(),
                stderr: Vec::new(),
                timed_out: false,
            })
        })
    }
}

fn run_verification_check_with_fake_backend(
    session: &mut Session,
    request: VerificationCheckRunRequest,
) -> Result<crate::VerificationRecordedEntry> {
    let backend = FakeVerificationBackend;
    futures::executor::block_on(run_verification_check(session, &backend, request))
}

fn run_verification_check_with_sandbox_backend(
    session: &mut Session,
    request: VerificationCheckRunRequest,
) -> Result<crate::VerificationRecordedEntry> {
    let backend = FakeSandboxVerificationBackend;
    futures::executor::block_on(run_verification_check(session, &backend, request))
}

#[test]
fn visible_state_preserves_run_status_and_verification_verdict() {
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Completed, VerificationVerdict::Passed),
        VisibleCompletionState::Verified
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Completed, VerificationVerdict::Stale),
        VisibleCompletionState::CompletedUnverified
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Blocked, VerificationVerdict::Missing),
        VisibleCompletionState::NeedsUser
    );
    assert!(RunStatus::Completed.is_terminal());
    assert!(RunStatus::Blocked.is_terminal());
    assert!(RunStatus::Failed.is_terminal());
    assert!(RunStatus::Cancelled.is_terminal());
    assert!(RunStatus::Interrupted.is_terminal());
    assert!(!RunStatus::Running.is_terminal());
    assert!(!RunStatus::Paused.is_terminal());
    assert!(VerificationVerdict::Passed.is_terminal());
    assert!(!VerificationVerdict::Pending.is_terminal());
    assert!(!VerificationVerdict::NotEvaluated.is_terminal());
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Completed, VerificationVerdict::NotApplicable),
        VisibleCompletionState::Completed
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Completed, VerificationVerdict::Pending),
        VisibleCompletionState::CompletedUnverified
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Failed, VerificationVerdict::Failed),
        VisibleCompletionState::FailedVerification
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Cancelled, VerificationVerdict::Passed),
        VisibleCompletionState::Cancelled
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Paused, VerificationVerdict::Missing),
        VisibleCompletionState::Paused
    );
    assert_eq!(
        VisibleCompletionState::derive(RunStatus::Interrupted, VerificationVerdict::Passed),
        VisibleCompletionState::Interrupted
    );
    for verdict in [
        VerificationVerdict::Missing,
        VerificationVerdict::Skipped,
        VerificationVerdict::Inconclusive,
        VerificationVerdict::Failed,
    ] {
        assert_eq!(
            VisibleCompletionState::derive(RunStatus::Completed, verdict),
            VisibleCompletionState::CompletedUnverified
        );
    }
    for verdict in [
        VerificationVerdict::Stale,
        VerificationVerdict::Inconclusive,
        VerificationVerdict::NotEvaluated,
        VerificationVerdict::Pending,
    ] {
        assert_eq!(
            VisibleCompletionState::derive(RunStatus::Blocked, verdict),
            VisibleCompletionState::NeedsUser
        );
    }
}

#[test]
fn verification_policy_scope_trust_and_effect_helpers_cover_edges() -> Result<()> {
    assert!(super::default_scope_excludes().contains(&"target/**".to_owned()));
    assert!(!super::default_scope_excludes().contains(&".sigil/**".to_owned()));
    assert!(super::default_scope_excludes().contains(&".sigil/sessions/**".to_owned()));
    assert!(super::default_scope_excludes().contains(&".next/**".to_owned()));
    assert!(super::default_scope_excludes().contains(&"__pycache__/**".to_owned()));
    let repo_scope = VerificationScope::all_tracked("scope-main");
    let src_scope = VerificationScope {
        scope_hash: "scope-src".to_owned(),
        include: vec!["src/**".to_owned()],
        exclude: super::default_scope_excludes(),
        tracked_files_only: true,
        max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
        generated_roots: Vec::new(),
    };
    let wide_scope = VerificationScope {
        scope_hash: "scope-wide".to_owned(),
        include: vec!["**/*".to_owned()],
        exclude: repo_scope.exclude.clone(),
        tracked_files_only: false,
        max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
        generated_roots: Vec::new(),
    };
    assert!(wide_scope.covers(&src_scope));
    assert!(!src_scope.covers(&repo_scope));
    assert!(!repo_scope.covers(&wide_scope));
    let low_file_limit_scope = VerificationScope {
        max_file_bytes: repo_scope.max_file_bytes.saturating_sub(1),
        ..repo_scope.clone()
    };
    assert!(!low_file_limit_scope.covers(&repo_scope));

    assert_eq!(
        SandboxProfileRequirement::None.stricter(SandboxProfileRequirement::Sandboxed)?,
        SandboxProfileRequirement::Sandboxed
    );
    assert_eq!(
        WorkspaceTrustRequirement::None.stricter(WorkspaceTrustRequirement::Trusted),
        WorkspaceTrustRequirement::Trusted
    );
    assert!(WorkspaceTrustRequirement::None.is_satisfied(WorkspaceTrust::Denied, None, None));
    assert!(!WorkspaceTrustRequirement::ApprovalOrSandbox.is_satisfied(
        WorkspaceTrust::Unknown,
        None,
        None
    ));
    assert!(WorkspaceTrustRequirement::ApprovalOrSandbox.is_satisfied(
        WorkspaceTrust::Unknown,
        Some(&"approval".to_owned()),
        None
    ));
    assert!(WorkspaceTrustRequirement::ApprovalOrSandbox.is_satisfied(
        WorkspaceTrust::Unknown,
        None,
        Some(&"sandbox".to_owned())
    ));
    assert!(WorkspaceTrustRequirement::ApprovalOrSandbox.is_satisfied(
        WorkspaceTrust::Trusted,
        None,
        None
    ));
    assert!(WorkspaceTrustRequirement::Trusted.is_satisfied(WorkspaceTrust::Trusted, None, None));

    for (effect, label, mutates) in [
        (ToolEffect::ReadOnly, "read_only", false),
        (ToolEffect::WorkspaceWrite, "workspace_write", true),
        (ToolEffect::ExternalWrite, "external_write", true),
        (ToolEffect::Network, "network", false),
        (ToolEffect::Unknown, "unknown", true),
    ] {
        assert_eq!(effect.as_str(), label);
        assert_eq!(effect.may_mutate_workspace(), mutates);
    }
    assert!(VerificationConfig::default().is_empty());
    assert_eq!(CheckCommand::shell("cargo test").command, "cargo test");
    assert_eq!(
        super::CompletionCriteria::NoChecksRequired
            .stricter(super::CompletionCriteria::AllRequiredChecks),
        super::CompletionCriteria::AllRequiredChecks
    );
    let decoded: VerificationCheckConfig = toml::from_str(
        r#"
            id = "default-effect"
            command = "cargo"
        "#,
    )?;
    assert_eq!(decoded.effect, ToolEffect::ReadOnly);
    Ok(())
}

#[test]
fn verification_scope_profiles_apply_language_cache_excludes_without_hiding_skills() {
    let node_scope =
        VerificationScope::profiled("scope-node", crate::VerificationScopeProfile::Node);
    assert!(node_scope.exclude.contains(&"node_modules/**".to_owned()));
    assert!(node_scope.exclude.contains(&".next/**".to_owned()));
    assert!(
        !node_scope
            .exclude
            .iter()
            .any(|pattern| pattern == ".sigil/**" || pattern == ".sigil/skills/**")
    );

    let python_scope =
        VerificationScope::profiled("scope-python", crate::VerificationScopeProfile::Python);
    assert!(python_scope.exclude.contains(&"__pycache__/**".to_owned()));
    assert!(python_scope.exclude.contains(&".venv/**".to_owned()));

    let docs_scope =
        VerificationScope::profiled("scope-docs", crate::VerificationScopeProfile::Docs);
    assert!(docs_scope.generated_roots.contains(&PathBuf::from("site")));
}

#[test]
fn check_promotion_receipt_and_projection_helpers_cover_edges() -> Result<()> {
    for (source, requires_promotion) in [
        (CheckDiscoverySource::SigilVerificationFile, true),
        (CheckDiscoverySource::UserExplicitConfig, false),
        (CheckDiscoverySource::CiConfig, true),
        (CheckDiscoverySource::PackageScript, true),
        (CheckDiscoverySource::Cargo, true),
        (CheckDiscoverySource::Makefile, true),
        (CheckDiscoverySource::ModelSuggested, true),
        (CheckDiscoverySource::UserConfirmed, false),
    ] {
        assert_eq!(source.requires_trust_promotion(), requires_promotion);
    }

    let repo_candidate = CandidateCheck {
        source: CheckDiscoverySource::Cargo,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-unknown".to_owned(),
    };
    let error = repo_candidate
        .clone()
        .promote(
            "cargo-test",
            "scope-main",
            ToolEffect::ReadOnly,
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "config".to_owned(),
            },
        )
        .expect_err("repo-discovered check cannot be promoted as user config");
    assert!(error.to_string().contains("requires approval"));

    let approved = repo_candidate.clone().promote(
        "cargo-test",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::UserApproved {
            approval_event_id: "approval-1".to_owned(),
        },
    )?;
    assert_eq!(approved.approval_event_id.as_deref(), Some("approval-1"));
    assert!(approved.sandbox_decision_id.is_none());
    let sandboxed = repo_candidate.promote(
        "cargo-test",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::Sandboxed {
            sandbox_decision_id: "sandbox-1".to_owned(),
        },
    )?;
    assert_eq!(sandboxed.sandbox_decision_id.as_deref(), Some("sandbox-1"));
    let workspace_trusted_error = CandidateCheck {
        source: CheckDiscoverySource::Makefile,
        command: CheckCommand::shell("make test"),
        source_event_id: "event-make".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "make-test",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::WorkspaceTrusted {
            trust_event_id: "event-trust".to_owned(),
        },
    )
    .expect_err("workspace trust alone must not promote repo-local checks");
    assert!(
        workspace_trusted_error
            .to_string()
            .contains("requires approval")
    );
    let global_policy = CandidateCheck {
        source: CheckDiscoverySource::CiConfig,
        command: CheckCommand::shell("cargo test"),
        source_event_id: "event-ci".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "ci-cargo-test",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::GlobalPolicy {
            policy_event_id: "event-global-policy".to_owned(),
        },
    )?;
    assert_eq!(global_policy.source, CheckDiscoverySource::CiConfig);
    let legacy_default_source: super::TrustedCheckSpec =
        serde_json::from_value(serde_json::json!({
            "check_spec": global_policy.check_spec,
            "promoted_by": { "kind": "global_policy", "policy_event_id": "event-global-policy" },
            "approval_event_id": null,
            "sandbox_decision_id": null
        }))?;
    assert_eq!(
        legacy_default_source.source,
        CheckDiscoverySource::UserConfirmed
    );

    let check = check_spec("cargo-test");
    let receipt = verification_receipt(
        "receipt-pass",
        &check,
        "snapshot-a",
        10,
        ReceiptStatus::Succeeded,
        false,
    );
    let scope = VerificationScope::all_tracked("scope-main");
    assert!(receipt.is_applicable_to(
        &check,
        &"snapshot-a".to_owned(),
        &scope,
        WorkspaceTrustRequirement::None,
        WorkspaceTrust::Unknown,
        SandboxProfileRequirement::None,
    ));
    assert!(!receipt.is_applicable_to(
        &check,
        &"snapshot-b".to_owned(),
        &scope,
        WorkspaceTrustRequirement::None,
        WorkspaceTrust::Unknown,
        SandboxProfileRequirement::None,
    ));
    assert!(!receipt.is_applicable_to(
        &check,
        &"snapshot-a".to_owned(),
        &scope,
        WorkspaceTrustRequirement::Trusted,
        WorkspaceTrust::Unknown,
        SandboxProfileRequirement::None,
    ));
    assert!(receipt.is_applicable_to(
        &check,
        &"snapshot-a".to_owned(),
        &scope,
        WorkspaceTrustRequirement::Trusted,
        WorkspaceTrust::Trusted,
        SandboxProfileRequirement::None,
    ));

    let mut projection = VerificationStateProjection::default();
    let entry = CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task-a".to_owned()),
        approved,
        "event-discovery",
    );
    projection.apply_control(&ControlEntry::CheckSpecRecorded(entry.clone()));
    assert!(
        projection
            .check_spec(&EvidenceScope::Task("task-a".to_owned()), "cargo-test")
            .is_some()
    );
    assert_eq!(
        projection
            .check_specs_for_scopes(&[
                EvidenceScope::Run("run-a".to_owned()),
                EvidenceScope::Task("task-a".to_owned()),
            ])
            .len(),
        1
    );
    Ok(())
}

#[test]
fn receipt_identity_snapshot_and_child_link_validation_cover_edges() {
    let complete_file = WorkspaceSnapshotEntry {
        normalized_path: PathBuf::from("src/lib.rs"),
        file_type: FileType::File,
        content_hash: Some("sha256:file".to_owned()),
        mode: Some(0o100644),
        file_metadata: None,
        symlink_target: None,
        state: SnapshotEntryState::Present,
    };
    assert!(complete_file.is_complete());
    assert!(SnapshotEntryState::Present.is_clean());
    assert!(SnapshotEntryState::Missing.is_clean());
    let incomplete_file = WorkspaceSnapshotEntry {
        content_hash: None,
        ..complete_file.clone()
    };
    assert!(!incomplete_file.is_complete());
    let complete_symlink = WorkspaceSnapshotEntry {
        normalized_path: PathBuf::from("link"),
        file_type: FileType::Symlink,
        content_hash: None,
        mode: None,
        file_metadata: None,
        symlink_target: Some(PathBuf::from("src/lib.rs")),
        state: SnapshotEntryState::Present,
    };
    assert!(complete_symlink.is_complete());
    let permission_denied = WorkspaceSnapshotEntry {
        state: SnapshotEntryState::PermissionDenied,
        ..complete_file
    };
    assert!(!permission_denied.is_complete());

    let mut receipt = EvidenceReceipt {
        receipt_id: "receipt-a".to_owned(),
        source_session_id: "session-a".to_owned(),
        source_event_id: "event-a".to_owned(),
        source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
        scope: EvidenceScope::Step("task:step".to_owned()),
        producer_tool_call: None,
        workspace_revision: Some(1),
        workspace_snapshot_id: Some("snapshot-a".to_owned()),
        policy_hash: Some("policy-a".to_owned()),
        changeset_id: None,
        status: ReceiptStatus::Succeeded,
        artifact_refs: Vec::new(),
        redaction_state: RedactionState::ContainsSensitiveMetadata,
        recorded_at_stream_sequence: 1,
    };
    receipt.validate_source_identity().expect("valid receipt");
    receipt.source_session_id.clear();
    assert!(receipt.validate_source_identity().is_err());
    receipt.source_session_id = "session-a".to_owned();
    receipt.source_event_id.clear();
    assert!(receipt.validate_source_identity().is_err());
    receipt.source_event_id = "event-a".to_owned();
    receipt.source_event_type.clear();
    assert!(receipt.validate_source_identity().is_err());
    receipt.source_event_type = DurableEventType::CheckFinished.as_str().to_owned();
    receipt.recorded_at_stream_sequence = 0;
    assert!(receipt.validate_source_identity().is_err());

    let valid_link = ChildVerificationReceiptLinked {
        parent_session_id: "parent".to_owned(),
        child_session_id: "child".to_owned(),
        child_receipt_id: "receipt".to_owned(),
        child_event_id: "event".to_owned(),
        child_workspace_id: "workspace".to_owned(),
        child_workspace_snapshot_id: "snapshot".to_owned(),
        policy_hash: "policy".to_owned(),
        changeset_id: None,
        merge_event_id: None,
    };
    assert!(valid_link.validate().is_ok());
    let mut invalid_link = valid_link;
    invalid_link.child_event_id.clear();
    assert!(invalid_link.validate().is_err());
}

#[test]
fn readiness_reducer_terminal_and_trust_edges_are_explicit() {
    let check = check_spec("cargo-test");
    let mut policy = policy_with_checks(vec![check.clone()]);
    policy.workspace_trust_requirement = WorkspaceTrustRequirement::Trusted;
    let input = ReadinessInput::new_run(RunStatus::Completed, policy.clone());
    let evaluation = evaluate_readiness(&input);
    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::TrustWorkspace)
    );

    let mut running = ReadinessInput::new_run(RunStatus::Running, policy_with_checks(vec![check]));
    running.pending_checks.push("cargo-test".to_owned());
    let pending = evaluate_readiness(&running);
    assert_eq!(pending.verification_verdict, VerificationVerdict::Pending);
    assert_eq!(pending.visible_state, VisibleCompletionState::Running);

    let mut terminal = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check_spec("cargo-test")]),
    );
    terminal.pending_checks.push("cargo-test".to_owned());
    let inconclusive = evaluate_readiness(&terminal);
    assert_eq!(
        inconclusive.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert!(inconclusive.reasons.iter().any(|reason| matches!(
        reason,
        ReadinessReason::PendingCheckReducedForTerminalRun { .. }
    )));

    let mut legacy = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check_spec("cargo-test")]),
    );
    legacy.projection_mode = ReadinessProjectionMode::LegacyProjection;
    let legacy_eval = evaluate_readiness(&legacy);
    assert_eq!(
        legacy_eval.verification_verdict,
        VerificationVerdict::NotEvaluated
    );
    assert!(
        legacy_eval
            .reasons
            .contains(&ReadinessReason::LegacyEvidenceUnavailable)
    );
}

#[test]
fn pure_question_maps_to_not_applicable() {
    let input = ReadinessInput::new_run(
        RunStatus::Completed,
        VerificationPolicy::no_checks_required("scope-main"),
    );

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::NotApplicable
    );
    assert_eq!(evaluation.visible_state, VisibleCompletionState::Completed);
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::NoVerificationRequired)
    );
}

#[test]
fn code_write_without_check_maps_to_missing() {
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        VerificationPolicy::no_checks_required("scope-main"),
    );
    input.workspace_knowledge = WorkspaceKnowledge::Dirty(1);
    input
        .mutations
        .push(workspace_mutation("event-write-1", 10));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::ProvideVerificationConfig)
    );
}

#[test]
fn successful_check_after_write_maps_to_passed() {
    let check = check_spec("cargo-test");
    let policy = policy_with_checks(vec![check.clone()]);
    let snapshot = "snapshot-after-write".to_owned();
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id = Some(snapshot.clone());
    input.workspace_knowledge = WorkspaceKnowledge::Clean(1);
    input.verification_receipts.push(verification_receipt(
        "receipt-pass",
        &check,
        &snapshot,
        12,
        ReceiptStatus::Succeeded,
        false,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Passed);
    assert_eq!(evaluation.visible_state, VisibleCompletionState::Verified);
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::VerificationPassed {
                receipt_id: "receipt-pass".to_owned()
            })
    );
}

#[test]
fn verification_check_runner_records_durable_check_and_passed_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let store_path = temp.path().join("state/session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);

    let recorded = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check,
            policy,
            policy_hash: Some("policy-hash".to_owned()),
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;

    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Succeeded);
    assert!(!recorded.receipt.mutates_verification_scope);
    assert_eq!(
        recorded.receipt.receipt.source_event_type,
        DurableEventType::CheckFinished.as_str()
    );
    assert!(recorded.receipt.receipt.workspace_snapshot_id.is_some());

    let stored_events = JsonlSessionStore::read_event_records(&store_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event) => Some(event),
            SessionStreamRecord::Legacy { .. } => None,
        })
        .collect::<Vec<_>>();
    let event_types = stored_events
        .iter()
        .map(|event| event.event_type.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        vec![
            DurableEventType::CommandFinished.as_str().to_owned(),
            DurableEventType::CheckFinished.as_str().to_owned(),
        ]
    );
    assert_eq!(
        stored_events[0].payload["execution_network"]["policy"],
        serde_json::json!("unknown")
    );
    Ok(())
}

#[test]
fn verification_check_runner_binds_receipt_to_actual_sandbox_backend() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let mut policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);
    policy.sandbox_profile = SandboxProfileRequirement::Sandboxed;

    let recorded = run_verification_check_with_sandbox_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check,
            policy,
            policy_hash: Some("policy-hash".to_owned()),
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;

    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Succeeded);
    assert_eq!(
        recorded.receipt.binding.execution_backend,
        Some(ExecutionBackendKind::MacosSeatbelt)
    );
    assert_eq!(
        recorded.receipt.binding.execution_network.policy,
        ExecutionNetworkPolicy::Denied
    );
    let capabilities = recorded
        .receipt
        .binding
        .execution_backend_capabilities
        .expect("new receipt should bind backend capabilities");
    assert!(capabilities.supports_required_sandbox());
    assert_eq!(
        recorded.receipt.binding.sandbox_profile_hash,
        super::sandbox_profile_hash_for_execution(
            SandboxProfileRequirement::Sandboxed,
            ExecutionBackendKind::MacosSeatbelt,
            capabilities,
            &recorded.receipt.binding.execution_network,
        )
    );
    assert_ne!(
        recorded.receipt.binding.sandbox_profile_hash,
        super::sandbox_profile_hash(SandboxProfileRequirement::Sandboxed)
    );
    Ok(())
}

#[test]
fn plugin_verification_hook_receipt_binds_snapshot_backend_and_check_spec() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let store_path = temp.path().join("state/session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let trusted_check = trusted_plugin_check("plugin-verification", ToolEffect::ReadOnly)?;
    let mut policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);
    policy.sandbox_profile = SandboxProfileRequirement::Sandboxed;
    let started = plugin_verification_hook_started(ToolEffect::ReadOnly);
    let finished =
        plugin_verification_hook_finished(&started, PluginHookExecutionStatus::Succeeded);
    let mut output = plugin_verification_hook_output(&started);
    output.artifact_refs.push(PluginHookOutputArtifactRef {
        artifact_id: "artifact-hook-log".to_owned(),
        label: "hook log".to_owned(),
        media_type: Some("text/plain".to_owned()),
        size_bytes: Some(42),
        redaction_state: RedactionState::Redacted,
    });
    output.redaction_state = RedactionState::Redacted;

    let recorded = record_plugin_verification_hook_receipt(
        &mut session,
        PluginVerificationHookReceiptRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: trusted_check.clone(),
            policy: policy.clone(),
            policy_hash: Some("policy-hash".to_owned()),
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            started: started.clone(),
            finished: finished.clone(),
            output,
            workspace_mutation_event_id: None,
        },
    )?;

    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Succeeded);
    assert!(!recorded.receipt.mutates_verification_scope);
    assert_eq!(
        recorded.receipt.receipt.producer_tool_call.as_deref(),
        Some(started.execution_id.as_str())
    );
    assert_eq!(
        recorded.receipt.receipt.artifact_refs,
        vec!["artifact-hook-log".to_owned()]
    );
    assert_eq!(
        recorded.receipt.receipt.redaction_state,
        RedactionState::Redacted
    );
    assert_eq!(
        recorded.receipt.binding.execution_backend,
        Some(finished.backend)
    );
    assert_eq!(
        recorded.receipt.binding.execution_network.policy,
        ExecutionNetworkPolicy::Denied
    );
    assert_eq!(
        recorded.receipt.binding.check_spec_hash,
        trusted_check.check_spec.check_spec_hash
    );
    assert!(recorded.receipt.is_applicable_to(
        &trusted_check.check_spec,
        &recorded.receipt.binding.workspace_snapshot_id,
        &policy.verification_scope,
        policy.workspace_trust_requirement,
        WorkspaceTrust::Unknown,
        policy.sandbox_profile,
    ));
    assert!(!recorded.receipt.is_applicable_to(
        &trusted_check.check_spec,
        &"snapshot-different".to_owned(),
        &policy.verification_scope,
        policy.workspace_trust_requirement,
        WorkspaceTrust::Unknown,
        policy.sandbox_profile,
    ));

    let stored_events = JsonlSessionStore::read_event_records(&store_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::CheckFinished.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stored_events.len(), 1);
    assert_eq!(stored_events[0].payload["source"], "plugin_hook");
    assert_eq!(stored_events[0].payload["plugin_id"], started.plugin_id);
    assert_eq!(
        stored_events[0].payload["hook_execution_id"],
        started.execution_id
    );
    assert_eq!(
        stored_events[0].payload["execution_backend"],
        serde_json::json!("macos_seatbelt")
    );
    Ok(())
}

#[test]
fn mutating_plugin_verification_hook_receipt_is_inconclusive_and_not_applicable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let trusted_check = trusted_plugin_check("plugin-verification", ToolEffect::ReadOnly)?;
    let policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);
    let started = plugin_verification_hook_started(ToolEffect::WorkspaceWrite);
    let finished =
        plugin_verification_hook_finished(&started, PluginHookExecutionStatus::Succeeded);
    let output = plugin_verification_hook_output(&started);

    let recorded = record_plugin_verification_hook_receipt(
        &mut session,
        PluginVerificationHookReceiptRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: trusted_check.clone(),
            policy: policy.clone(),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            started,
            finished,
            output,
            workspace_mutation_event_id: Some("event-workspace-mutation".to_owned()),
        },
    )?;

    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Inconclusive);
    assert!(recorded.receipt.mutates_verification_scope);
    assert!(!recorded.receipt.is_applicable_to(
        &trusted_check.check_spec,
        &recorded.receipt.binding.workspace_snapshot_id,
        &policy.verification_scope,
        policy.workspace_trust_requirement,
        WorkspaceTrust::Unknown,
        policy.sandbox_profile,
    ));

    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id =
        Some(recorded.receipt.binding.workspace_snapshot_id.clone());
    input.verification_receipts.push(recorded.receipt);
    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert_eq!(
        evaluation.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert!(
        evaluation
            .required_actions
            .iter()
            .any(|action| matches!(action, RequiredAction::RunCheck { .. }))
    );
    Ok(())
}

#[test]
fn plugin_verification_hook_receipt_rejects_non_verification_or_mismatched_evidence() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let trusted_check = trusted_plugin_check("plugin-verification", ToolEffect::ReadOnly)?;
    let policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);

    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let started = PluginHookExecutionStartedEntry {
        hook_kind: PluginHookKind::Context,
        ..plugin_verification_hook_started(ToolEffect::ReadOnly)
    };
    let finished =
        plugin_verification_hook_finished(&started, PluginHookExecutionStatus::Succeeded);
    let output = plugin_verification_hook_output(&started);
    let error = record_plugin_verification_hook_receipt(
        &mut session,
        PluginVerificationHookReceiptRequest {
            workspace_root: workspace.clone(),
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: trusted_check.clone(),
            policy: policy.clone(),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            started,
            finished,
            output,
            workspace_mutation_event_id: None,
        },
    )
    .expect_err("non-verification hooks must be rejected");
    assert!(error.to_string().contains("not verification"));

    let started = plugin_verification_hook_started(ToolEffect::ReadOnly);
    let finished =
        plugin_verification_hook_finished(&started, PluginHookExecutionStatus::Succeeded);
    let mut output = plugin_verification_hook_output(&started);
    output.execution_id = "different-execution".to_owned();
    let error = record_plugin_verification_hook_receipt(
        &mut session,
        PluginVerificationHookReceiptRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check,
            policy,
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            started,
            finished,
            output,
            workspace_mutation_event_id: None,
        },
    )
    .expect_err("mismatched hook output must be rejected");
    assert!(
        error
            .to_string()
            .contains("plugin verification hook output evidence mismatch")
    );
    Ok(())
}

#[test]
fn sandbox_required_policy_rejects_receipt_without_matching_backend_binding() {
    let check = check_spec("cargo-test");
    let mut policy = policy_with_checks(vec![check.clone()]);
    policy.sandbox_profile = SandboxProfileRequirement::Sandboxed;
    let snapshot = "snapshot-current".to_owned();
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id = Some(snapshot.clone());
    input.workspace_knowledge = WorkspaceKnowledge::Clean(1);
    input.verification_receipts.push(verification_receipt(
        "receipt-legacy",
        &check,
        &snapshot,
        12,
        ReceiptStatus::Succeeded,
        false,
    ));

    let legacy_evaluation = evaluate_readiness(&input);

    assert_eq!(
        legacy_evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        legacy_evaluation
            .required_actions
            .contains(&RequiredAction::RunCheck {
                check_spec_id: "cargo-test".to_owned()
            })
    );

    input.verification_receipts[0].binding.execution_backend = Some(ExecutionBackendKind::Local);
    input.verification_receipts[0]
        .binding
        .execution_backend_capabilities = Some(ExecutionBackendCapabilities::default());
    input.verification_receipts[0].binding.execution_network =
        ExecutionNetworkReceipt::unknown("legacy local receipt");
    input.verification_receipts[0].binding.sandbox_profile_hash =
        super::sandbox_profile_hash_for_execution(
            SandboxProfileRequirement::Sandboxed,
            ExecutionBackendKind::Local,
            ExecutionBackendCapabilities::default(),
            &input.verification_receipts[0].binding.execution_network,
        );

    let local_evaluation = evaluate_readiness(&input);

    assert_eq!(
        local_evaluation.verification_verdict,
        VerificationVerdict::Missing
    );

    let inconsistent_network_capabilities = ExecutionBackendCapabilities {
        filesystem_isolation: true,
        network_isolation: true,
        process_isolation: true,
        resource_limits: false,
        persistent_pty: false,
        workspace_snapshot: false,
    };
    input.verification_receipts[0].binding.execution_backend =
        Some(ExecutionBackendKind::MacosSeatbelt);
    input.verification_receipts[0]
        .binding
        .execution_backend_capabilities = Some(inconsistent_network_capabilities);
    input.verification_receipts[0].binding.execution_network =
        ExecutionNetworkReceipt::unsupported("backend cannot enforce network denial");
    input.verification_receipts[0].binding.sandbox_profile_hash =
        super::sandbox_profile_hash_for_execution(
            SandboxProfileRequirement::Sandboxed,
            ExecutionBackendKind::MacosSeatbelt,
            inconsistent_network_capabilities,
            &input.verification_receipts[0].binding.execution_network,
        );

    let unsupported_network_evaluation = evaluate_readiness(&input);

    assert_eq!(
        unsupported_network_evaluation.verification_verdict,
        VerificationVerdict::Missing
    );

    let capabilities = ExecutionBackendCapabilities {
        filesystem_isolation: true,
        network_isolation: true,
        process_isolation: true,
        resource_limits: false,
        persistent_pty: false,
        workspace_snapshot: false,
    };
    input.verification_receipts[0].binding.execution_backend =
        Some(ExecutionBackendKind::MacosSeatbelt);
    input.verification_receipts[0]
        .binding
        .execution_backend_capabilities = Some(capabilities);
    input.verification_receipts[0].binding.execution_network =
        ExecutionNetworkReceipt::denied("fake sandbox denied network");
    input.verification_receipts[0].binding.sandbox_profile_hash =
        super::sandbox_profile_hash_for_execution(
            SandboxProfileRequirement::Sandboxed,
            ExecutionBackendKind::MacosSeatbelt,
            capabilities,
            &input.verification_receipts[0].binding.execution_network,
        );

    let sandbox_evaluation = evaluate_readiness(&input);

    assert_eq!(
        sandbox_evaluation.verification_verdict,
        VerificationVerdict::Passed
    );
}

#[test]
fn verification_check_runner_accepts_trusted_check_promotion_approval() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::Cargo,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-unknown".to_owned(),
    }
    .promote(
        "rustc-version",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::UserApproved {
            approval_event_id: "event-approval".to_owned(),
        },
    )?;
    let mut policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);
    policy.workspace_trust_requirement = WorkspaceTrustRequirement::ApprovalOrSandbox;

    let recorded = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check,
            policy,
            policy_hash: Some("policy-hash".to_owned()),
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "trust-unknown".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;

    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Succeeded);
    assert_eq!(
        recorded.receipt.binding.approval_event_id.as_deref(),
        Some("event-approval")
    );
    Ok(())
}

#[test]
fn verification_check_runner_covers_missing_workspace_and_in_memory_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);
    let mut missing_session = Session::new("deepseek", "deepseek-v4-flash");
    let error = run_verification_check_with_fake_backend(
        &mut missing_session,
        VerificationCheckRunRequest {
            workspace_root: temp.path().join("missing-workspace"),
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: trusted_check.clone(),
            policy: policy.clone(),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )
    .expect_err("missing workspace should fail before command execution");
    assert!(error.to_string().contains("failed to canonicalize"));

    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let recorded = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check,
            policy,
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;

    assert_eq!(
        recorded.receipt.receipt.source_session_id,
        "session:in-memory"
    );
    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Succeeded);
    Ok(())
}

#[test]
fn verification_check_runner_records_failed_and_mutating_checks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let store_path = store.path().to_path_buf();
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let failing_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--definitely-not-a-real-rustc-flag".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-fails",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let failed = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace.clone(),
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: failing_check.clone(),
            policy: policy_with_checks(vec![failing_check.check_spec.clone()]),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;
    assert_eq!(failed.receipt.check_status, ReceiptStatus::Failed);
    assert!(!failed.receipt.mutates_verification_scope);

    let mutating_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-mutating-effect",
        "scope-main",
        ToolEffect::WorkspaceWrite,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let mutating = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: mutating_check.clone(),
            policy: policy_with_checks(vec![mutating_check.check_spec.clone()]),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;
    assert_eq!(mutating.receipt.check_status, ReceiptStatus::Inconclusive);
    assert!(mutating.receipt.mutates_verification_scope);
    let mutating_terminal_run = VerificationCheckRunEntry::new(
        "run-mutating".to_owned(),
        EvidenceScope::Step("task_1:step_1".to_owned()),
        &mutating_check.check_spec,
        VerificationCheckRunStatus::Running,
    )
    .with_terminal_receipt(&mutating.receipt);
    assert_eq!(
        mutating_terminal_run.status,
        VerificationCheckRunStatus::Inconclusive
    );
    assert_eq!(
        mutating_terminal_run.reason.as_deref(),
        Some("check mutated verification scope")
    );
    let mutation_events = JsonlSessionStore::read_event_records(&store_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str() =>
            {
                Some(event.payload)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(mutation_events.len(), 1);
    assert_eq!(mutation_events[0]["reason"], "declared_write_effect");
    assert_eq!(mutation_events[0]["unknown_dirty"], true);
    Ok(())
}

#[test]
fn verification_check_runner_records_timeout_and_spawn_failure_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::create_dir(workspace.join("checks"))?;
    fs::write(workspace.join("note.txt"), "stable\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let timeout_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), "sleep 0.2".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "timeout-check",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let mut timeout_policy = policy_with_checks(vec![timeout_check.check_spec.clone()]);
    timeout_policy.timeout_ms = Some(1);
    let timed_out = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace.clone(),
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: timeout_check.clone(),
            policy: timeout_policy,
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;
    assert_eq!(timed_out.receipt.check_status, ReceiptStatus::Failed);
    assert_eq!(
        timed_out.receipt.failure_reason.as_deref(),
        Some("check timed out after 1 ms")
    );
    let terminal_run = VerificationCheckRunEntry::new(
        "run-timeout".to_owned(),
        EvidenceScope::Step("task_1:step_1".to_owned()),
        &timeout_check.check_spec,
        VerificationCheckRunStatus::Running,
    )
    .with_terminal_receipt(&timed_out.receipt);
    assert_eq!(terminal_run.status, VerificationCheckRunStatus::Failed);
    assert_eq!(
        terminal_run.reason.as_deref(),
        Some("check timed out after 1 ms")
    );

    let missing_command = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "sigil-definitely-missing-verification-command".to_owned(),
            args: Vec::new(),
            cwd: Some(PathBuf::from("checks")),
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "missing-command",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let error = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: missing_command.clone(),
            policy: policy_with_checks(vec![missing_command.check_spec]),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )
    .expect_err("missing verification command should fail to spawn");
    assert!(
        error
            .to_string()
            .contains("failed to spawn verification check")
    );
    Ok(())
}

#[test]
fn verification_check_run_entry_covers_terminal_status_and_error_edges() {
    let check = CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "scope-main",
    );
    let skipped = verification_receipt(
        "receipt-skipped",
        &check,
        "snapshot-1",
        1,
        ReceiptStatus::Skipped,
        false,
    );

    let skipped_run = VerificationCheckRunEntry::new(
        "run-skipped".to_owned(),
        EvidenceScope::Step("task_1:step_1".to_owned()),
        &check,
        VerificationCheckRunStatus::Running,
    )
    .with_terminal_receipt(&skipped);
    assert_eq!(skipped_run.status, VerificationCheckRunStatus::Skipped);
    assert!(skipped_run.reason.is_none());

    let errored_run = VerificationCheckRunEntry::new(
        "run-error".to_owned(),
        EvidenceScope::Step("task_1:step_1".to_owned()),
        &check,
        VerificationCheckRunStatus::Queued,
    )
    .with_error("spawn failed");
    assert_eq!(errored_run.status, VerificationCheckRunStatus::Errored);
    assert_eq!(errored_run.reason.as_deref(), Some("spawn failed"));
}

#[test]
fn check_failure_reason_covers_timeout_and_signal_edges() {
    let timeout_without_configured_ms = super::CheckCommandOutput {
        backend: ExecutionBackendKind::Local,
        backend_capabilities: ExecutionBackendCapabilities::default(),
        network: Default::default(),
        resources: Default::default(),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: true,
    };
    assert_eq!(
        super::check_failure_reason(&timeout_without_configured_ms, None).as_deref(),
        Some("check timed out")
    );

    let terminated_without_exit_code = super::CheckCommandOutput {
        backend: ExecutionBackendKind::Local,
        backend_capabilities: ExecutionBackendCapabilities::default(),
        network: Default::default(),
        resources: Default::default(),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
    };
    assert_eq!(
        super::check_failure_reason(&terminated_without_exit_code, None).as_deref(),
        Some("check terminated without exit code")
    );
}

#[test]
fn verification_check_runner_rejects_untrusted_workspace_policy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let mut policy = policy_with_checks(vec![trusted_check.check_spec.clone()]);
    policy.workspace_trust_requirement = WorkspaceTrustRequirement::Trusted;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    let error = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: workspace,
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check,
            policy,
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Restricted,
            workspace_trust_snapshot_id: "trust-restricted".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )
    .expect_err("trusted policy should reject restricted workspace");

    assert!(error.to_string().contains("workspace trust requirement"));
    Ok(())
}

#[test]
fn check_spec_hash_includes_args_and_cwd() {
    let base = CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: Some(PathBuf::from(".")),
        },
        ToolEffect::ReadOnly,
        "scope-main",
    );
    let changed_args = CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned(), "--workspace".to_owned()],
            cwd: Some(PathBuf::from(".")),
        },
        ToolEffect::ReadOnly,
        "scope-main",
    );
    let changed_cwd = CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: Some(PathBuf::from("crates/sigil-kernel")),
        },
        ToolEffect::ReadOnly,
        "scope-main",
    );

    assert_ne!(base.check_spec_hash, changed_args.check_spec_hash);
    assert_ne!(base.check_spec_hash, changed_cwd.check_spec_hash);
}

#[test]
fn verification_policy_hash_is_content_bound() {
    let base = policy_with_checks(vec![check_spec("cargo-test")]);
    let same = policy_with_checks(vec![check_spec("cargo-test")]);
    let changed = policy_with_checks(vec![check_spec("cargo-check")]);

    assert_eq!(
        base.stable_hash().expect("base policy hashes"),
        same.stable_hash().expect("same policy hashes")
    );
    assert_ne!(
        base.stable_hash().expect("base policy hashes again"),
        changed.stable_hash().expect("changed policy hashes")
    );
}

#[test]
fn any_required_check_can_pass_after_an_earlier_missing_check() {
    let missing_check = check_spec("cargo-test");
    let passing_check = check_spec("cargo-check");
    let mut policy = policy_with_checks(vec![missing_check, passing_check.clone()]);
    policy.completion_criteria = CompletionCriteria::AnyRequiredCheck;
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-check",
        &passing_check,
        "snapshot-current",
        12,
        ReceiptStatus::Succeeded,
        false,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Passed);
}

#[test]
fn any_required_check_reports_failure_when_no_check_passes() {
    let failing = check_spec("cargo-test");
    let missing = check_spec("cargo-clippy");
    let mut policy = policy_with_checks(vec![failing.clone(), missing]);
    policy.completion_criteria = CompletionCriteria::AnyRequiredCheck;
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-failed",
        &failing,
        "snapshot-current",
        12,
        ReceiptStatus::Failed,
        false,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Failed);
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::ReviewVerificationFailure {
                receipt_id: "receipt-failed".to_owned()
            })
    );
}

#[test]
fn skipped_and_inconclusive_receipts_require_rerun() {
    for (status, expected) in [
        (ReceiptStatus::Skipped, VerificationVerdict::Missing),
        (
            ReceiptStatus::Inconclusive,
            VerificationVerdict::Inconclusive,
        ),
    ] {
        let check = check_spec("cargo-test");
        let mut input = ReadinessInput::new_run(
            RunStatus::Completed,
            policy_with_checks(vec![check.clone()]),
        );
        input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
        input.verification_receipts.push(verification_receipt(
            "receipt-nonfinal",
            &check,
            "snapshot-current",
            12,
            status,
            false,
        ));

        let evaluation = evaluate_readiness(&input);

        assert_eq!(evaluation.verification_verdict, expected);
        assert!(
            evaluation
                .required_actions
                .contains(&RequiredAction::RunCheck {
                    check_spec_id: "cargo-test".to_owned()
                })
        );
    }
}

#[test]
fn missing_snapshot_and_snapshot_mismatch_require_check_rerun() {
    let check = check_spec("cargo-test");
    let input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    let evaluation = evaluate_readiness(&input);
    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::RunCheck {
                check_spec_id: "cargo-test".to_owned()
            })
    );

    let mut mismatch = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    mismatch.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    mismatch.verification_receipts.push(verification_receipt(
        "receipt-old-snapshot",
        &check,
        "snapshot-old",
        12,
        ReceiptStatus::Succeeded,
        false,
    ));
    let evaluation = evaluate_readiness(&mismatch);
    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            ReadinessReason::ReceiptSnapshotMismatch { receipt_id }
                if receipt_id == "receipt-old-snapshot"
        )
    }));
}

#[test]
fn no_checks_required_criteria_stays_not_applicable_even_with_checks() {
    let mut policy = policy_with_checks(vec![check_spec("cargo-test")]);
    policy.completion_criteria = CompletionCriteria::NoChecksRequired;
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::NotApplicable
    );
}

#[test]
fn unknown_dirty_without_prior_pass_is_inconclusive_and_without_event_stays_inconclusive() {
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check_spec("cargo-test")]),
    );
    input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::ResolveUnknownDirty)
    );

    let mut no_required_checks = ReadinessInput::new_run(
        RunStatus::Completed,
        VerificationPolicy::no_checks_required("scope-main"),
    );
    no_required_checks.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
    let evaluation = evaluate_readiness(&no_required_checks);
    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
}

#[test]
fn failed_then_successful_rerun_uses_latest_current_receipt() {
    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-failed-first",
        &check,
        "snapshot-current",
        12,
        ReceiptStatus::Failed,
        false,
    ));
    input.verification_receipts.push(verification_receipt(
        "receipt-passed-later",
        &check,
        "snapshot-current",
        13,
        ReceiptStatus::Succeeded,
        false,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Passed);
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::VerificationPassed {
                receipt_id: "receipt-passed-later".to_owned()
            })
    );
}

#[test]
fn mutating_check_then_non_writing_rerun_can_pass() {
    let check = check_spec("cargo-fmt");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.current_workspace_snapshot_id = Some("snapshot-after-fmt".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-fmt-mutated",
        &check,
        "snapshot-after-fmt",
        20,
        ReceiptStatus::Succeeded,
        true,
    ));
    input.verification_receipts.push(verification_receipt(
        "receipt-fmt-check",
        &check,
        "snapshot-after-fmt",
        21,
        ReceiptStatus::Succeeded,
        false,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Passed);
}

#[test]
fn write_after_successful_check_maps_to_stale() {
    let check = check_spec("cargo-test");
    let policy = policy_with_checks(vec![check.clone()]);
    let snapshot = "snapshot-before-second-write".to_owned();
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.current_workspace_snapshot_id = Some("snapshot-after-second-write".to_owned());
    input.workspace_knowledge = WorkspaceKnowledge::Dirty(2);
    input.verification_receipts.push(verification_receipt(
        "receipt-pass",
        &check,
        &snapshot,
        12,
        ReceiptStatus::Succeeded,
        false,
    ));
    input.mutations.push(WorkspaceMutationEvidence {
        event_id: "event-write-2".to_owned(),
        source_event_type: "mutation_committed".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: "scope-main".to_owned(),
        recorded_at_stream_sequence: 13,
        from_workspace_snapshot_id: Some(snapshot),
        to_workspace_snapshot_id: Some("snapshot-after-second-write".to_owned()),
        tool_effect: ToolEffect::WorkspaceWrite,
        unknown_dirty: false,
    });

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Stale);
    assert!(evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            ReadinessReason::VerificationStale(VerificationStaleCause {
                reason: VerificationStaleReason::WorkspaceChanged(event_id),
                ..
            }) if event_id == "event-write-2"
        )
    }));
}

#[test]
fn terminal_run_never_persists_pending_or_not_evaluated_for_new_runs() {
    let check = check_spec("cargo-test");
    let mut pending = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    pending.pending_checks.push(check.check_spec_id.clone());

    let pending_eval = evaluate_readiness(&pending);

    assert_ne!(
        pending_eval.verification_verdict,
        VerificationVerdict::Pending
    );
    assert_eq!(
        pending_eval.verification_verdict,
        VerificationVerdict::Inconclusive
    );

    let legacy = ReadinessInput {
        projection_mode: ReadinessProjectionMode::LegacyProjection,
        ..ReadinessInput::new_run(RunStatus::Completed, policy_with_checks(vec![check]))
    };
    let legacy_eval = evaluate_readiness(&legacy);

    assert_eq!(
        legacy_eval.verification_verdict,
        VerificationVerdict::NotEvaluated
    );
}

#[test]
fn receipt_scope_mismatch_cannot_pass_current_policy() {
    let check = check_spec("cargo-test");
    let mut receipt = verification_receipt(
        "receipt-wrong-scope",
        &check,
        "snapshot-current",
        12,
        ReceiptStatus::Succeeded,
        false,
    );
    receipt.binding.verification_scope_hash = "other-scope".to_owned();
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy_with_checks(vec![check]));
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input.verification_receipts.push(receipt);

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            ReadinessReason::ReceiptScopeMismatch { receipt_id }
                if receipt_id == "receipt-wrong-scope"
        )
    }));
}

#[test]
fn child_receipt_with_only_local_sequence_is_rejected() {
    let mut receipt = base_evidence_receipt("receipt-child", 7, ReceiptStatus::Succeeded);
    receipt.source_session_id.clear();

    let error = receipt
        .validate_source_identity()
        .expect_err("missing source session should fail");

    assert!(error.to_string().contains("source_session_id"));
}

#[test]
fn formatter_mutation_cannot_produce_final_passed_evidence() {
    let check = check_spec("cargo-fmt");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.current_workspace_snapshot_id = Some("snapshot-after-fmt".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-fmt",
        &check,
        "snapshot-after-fmt",
        20,
        ReceiptStatus::Succeeded,
        true,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::ReRunNonWritingCheck {
                check_spec_id: "cargo-fmt".to_owned()
            })
    );
}

#[test]
fn user_skip_requires_policy_support_and_maps_to_skipped() {
    let check = check_spec("cargo-test");
    let mut policy = policy_with_checks(vec![check]);
    policy.allow_unverified_completion = true;
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.skip_decision = Some(VerificationSkipDecision {
        event_id: "event-skip".to_owned(),
        reason: "user explicitly skipped".to_owned(),
    });

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Skipped
    );
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::VerificationSkipped {
                event_id: "event-skip".to_owned()
            })
    );
}

#[test]
fn required_check_failure_maps_to_failed() {
    let check = check_spec("cargo-test");
    let mut input =
        ReadinessInput::new_run(RunStatus::Failed, policy_with_checks(vec![check.clone()]));
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-fail",
        &check,
        "snapshot-current",
        30,
        ReceiptStatus::Failed,
        false,
    ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Failed);
    assert_eq!(
        evaluation.visible_state,
        VisibleCompletionState::FailedVerification
    );
}

#[test]
fn untrusted_workspace_discovers_but_does_not_auto_promote_repo_checks() {
    let candidate = CandidateCheck {
        source: CheckDiscoverySource::Makefile,
        command: CheckCommand::shell("make test"),
        source_event_id: "event-discover".to_owned(),
        workspace_trust_snapshot_id: "trust-unknown".to_owned(),
    };

    let error = candidate
        .clone()
        .promote(
            "make-test",
            "scope-main",
            ToolEffect::ReadOnly,
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config".to_owned(),
            },
        )
        .expect_err("untrusted repository check needs approval or sandbox");
    assert!(error.to_string().contains("requires approval"));

    let trusted = candidate
        .promote(
            "make-test",
            "scope-main",
            ToolEffect::ReadOnly,
            CheckPromotion::UserApproved {
                approval_event_id: "event-approval".to_owned(),
            },
        )
        .expect("approval promotes untrusted candidate");
    assert_eq!(trusted.approval_event_id.as_deref(), Some("event-approval"));
}

#[test]
fn check_discovery_reads_repo_sources_without_auto_promotion() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".sigil")).expect("sigil dir");
    fs::write(
        temp.path().join(".sigil/verification.toml"),
        r#"
            [[checks]]
            id = "docs-check"
            command = "cargo"
            args = ["test", "-p", "sigil-kernel"]
        "#,
    )
    .expect("verification file");
    fs::create_dir_all(temp.path().join(".github/workflows")).expect("workflow dir");
    fs::write(
        temp.path().join(".github/workflows/ci.yml"),
        "jobs:\n  test:\n    steps:\n      - run: \"cargo test --workspace\"\n      - run: 'npm test -- --runInBand'\n      - run: make test\n",
    )
    .expect("ci file");
    fs::write(
        temp.path().join("package.json"),
        r#"{"scripts":{"test":"vitest","check":"tsc --noEmit","lint":"eslint .","build":"vite build","empty":""}}"#,
    )
    .expect("package file");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .expect("cargo file");
    fs::write(temp.path().join("Makefile"), "test:\n\tcargo test\n").expect("makefile");

    let checks = discover_candidate_checks(temp.path(), "trust-unknown", "event-discovery")
        .expect("discovery succeeds");

    let ids = checks
        .iter()
        .map(|check| check.suggested_check_spec_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "docs-check",
            "cargo-test-ci",
            "npm-test-ci",
            "make-test-ci",
            "npm-test",
            "npm-check",
            "npm-lint",
            "npm-build",
            "cargo-test-workspace",
            "make-test",
        ]
    );
    assert!(checks.iter().all(|check| {
        check.candidate.source_event_id == "event-discovery"
            && check.candidate.workspace_trust_snapshot_id == "trust-unknown"
    }));
    let error = checks[0]
        .clone()
        .promote(
            "scope-main",
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config".to_owned(),
            },
        )
        .expect_err("repo-local discovery still requires trust promotion");
    assert!(error.to_string().contains("requires approval"));
}

#[test]
fn check_discovery_covers_empty_workspace_and_non_workspace_cargo() {
    let empty = tempfile::tempdir().expect("tempdir");
    let checks = discover_candidate_checks(empty.path(), "trust-unknown", "event-discovery")
        .expect("empty workspace discovery succeeds");
    assert!(checks.is_empty());

    fs::write(empty.path().join("package.json"), r#"{"dependencies":{}}"#)
        .expect("package file without scripts");
    let checks = discover_candidate_checks(empty.path(), "trust-unknown", "event-discovery")
        .expect("package without scripts should not produce checks");
    assert!(checks.is_empty());
    fs::remove_file(empty.path().join("package.json")).expect("remove package");

    fs::write(
        empty.path().join("Cargo.toml"),
        "[package]\nname = 'demo'\nversion = '0.1.0'\nedition = '2024'\n",
    )
    .expect("cargo file");
    let checks = discover_candidate_checks(empty.path(), "trust-unknown", "event-discovery")
        .expect("cargo discovery succeeds");
    assert_eq!(checks[0].suggested_check_spec_id, "cargo-test");
    assert_eq!(checks[0].candidate.command.args, vec!["test".to_owned()]);
}

#[test]
fn check_discovery_reports_malformed_repo_manifests_and_empty_checks() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".sigil")).expect("sigil dir");
    fs::write(temp.path().join(".sigil/verification.toml"), "checks = [").expect("bad toml");
    let error = discover_candidate_checks(temp.path(), "trust-unknown", "event-discovery")
        .expect_err("malformed verification file should fail");
    assert!(error.to_string().contains("failed to parse"));

    fs::write(
        temp.path().join(".sigil/verification.toml"),
        r#"
            [[checks]]
            id = ""
            command = "cargo"
        "#,
    )
    .expect("empty check config");
    let error = discover_candidate_checks(temp.path(), "trust-unknown", "event-discovery")
        .expect_err("empty check should fail");
    assert!(error.to_string().contains("empty id or command"));

    fs::remove_file(temp.path().join(".sigil/verification.toml")).expect("remove sigil config");
    fs::write(temp.path().join("package.json"), "{").expect("bad package");
    let error = discover_candidate_checks(temp.path(), "trust-unknown", "event-discovery")
        .expect_err("malformed package should fail");
    assert!(error.to_string().contains("failed to parse"));
    fs::remove_file(temp.path().join("package.json")).expect("remove package");

    fs::write(temp.path().join("Cargo.toml"), "[workspace").expect("bad cargo");
    let error = discover_candidate_checks(temp.path(), "trust-unknown", "event-discovery")
        .expect_err("malformed Cargo.toml should fail");
    assert!(error.to_string().contains("failed to parse"));
}

#[test]
fn ci_discovery_ignores_verification_commands_outside_run_steps() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".github/workflows")).expect("workflow dir");
    fs::write(
        temp.path().join(".github/workflows/ci.yml"),
        "jobs:\n  test:\n    steps:\n      # cargo test should not become a check\n      - name: docs mention npm test\n      - run: make test\n",
    )
    .expect("ci file");

    let checks = discover_candidate_checks(temp.path(), "trust-unknown", "event-discovery")
        .expect("discovery succeeds");
    let ids = checks
        .iter()
        .map(|check| check.suggested_check_spec_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, vec!["make-test-ci"]);
}

#[test]
fn user_configured_checks_are_discovered_before_repo_checks_and_can_promote_explicitly() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname = 'demo'\n").expect("cargo file");
    let user_config = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![VerificationCheckConfig {
            id: "user-check".to_owned(),
            command: "cargo".to_owned(),
            args: vec!["check".to_owned()],
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
        ..VerificationConfig::default()
    };

    let checks = discover_candidate_checks_with_user_config(
        temp.path(),
        "trust-unknown",
        "event-discovery",
        &user_config,
    )
    .expect("discovery succeeds");

    assert_eq!(checks[0].suggested_check_spec_id, "user-check");
    assert_eq!(
        checks[0].candidate.source,
        CheckDiscoverySource::UserExplicitConfig
    );
    assert_eq!(checks[1].candidate.source, CheckDiscoverySource::Cargo);
    let trusted = checks[0]
        .clone()
        .promote(
            "scope-main",
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config".to_owned(),
            },
        )
        .expect("explicit user config can promote without workspace trust");
    assert_eq!(trusted.source, CheckDiscoverySource::UserExplicitConfig);
}

#[test]
fn user_config_discovery_validates_empty_checks_and_normalizes_cwd_edges() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir(temp.path().join("crates")).expect("cwd dir");
    let empty = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![VerificationCheckConfig {
            id: " ".to_owned(),
            command: "cargo".to_owned(),
            args: Vec::new(),
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
        ..VerificationConfig::default()
    };
    let error =
        discover_candidate_checks_with_user_config(temp.path(), "trust-1", "event-config", &empty)
            .expect_err("empty explicit user check should fail closed");
    assert!(error.to_string().contains("empty id or command"));

    let cwd_edges = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![
            VerificationCheckConfig {
                id: "empty-cwd".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: Some(PathBuf::new()),
                effect: ToolEffect::ReadOnly,
            },
            VerificationCheckConfig {
                id: "curdir-cwd".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: Some(PathBuf::from(".")),
                effect: ToolEffect::ReadOnly,
            },
            VerificationCheckConfig {
                id: "normal-cwd".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: Some(PathBuf::from("./crates")),
                effect: ToolEffect::ReadOnly,
            },
        ],
        ..VerificationConfig::default()
    };
    let checks = discover_candidate_checks_with_user_config(
        temp.path(),
        "trust-1",
        "event-config",
        &cwd_edges,
    )
    .expect("cwd edges should normalize");

    assert_eq!(checks[0].candidate.command.cwd, None);
    assert_eq!(checks[1].candidate.command.cwd, None);
    assert_eq!(
        checks[2].candidate.command.cwd.as_deref(),
        Some(Path::new("crates"))
    );
}

#[test]
fn user_configured_check_specs_are_trusted_and_deduplicate_ids() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let user_config = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![
            VerificationCheckConfig {
                id: "check".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: None,
                effect: ToolEffect::ReadOnly,
            },
            VerificationCheckConfig {
                id: "check".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["check".to_owned()],
                cwd: None,
                effect: ToolEffect::ReadOnly,
            },
        ],
        ..VerificationConfig::default()
    };

    let entries = check_specs_from_user_config(
        temp.path(),
        &user_config,
        EvidenceScope::Task("task-1".to_owned()),
        "scope-main",
        "event-config",
    )?;

    assert_eq!(entries[0].trusted_check.check_spec.check_spec_id, "check");
    assert_eq!(entries[1].trusted_check.check_spec.check_spec_id, "check-2");
    assert!(entries.iter().all(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::ExplicitUserConfig { .. }
        )
    }));

    let invalid = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![VerificationCheckConfig {
            id: " ".to_owned(),
            command: "cargo".to_owned(),
            args: Vec::new(),
            cwd: None,
            effect: ToolEffect::ReadOnly,
        }],
        ..VerificationConfig::default()
    };
    let error = check_specs_from_user_config(
        temp.path(),
        &invalid,
        EvidenceScope::Task("task-1".to_owned()),
        "scope-main",
        "event-config",
    )
    .expect_err("empty check id should fail");
    assert!(error.to_string().contains("empty id or command"));
    Ok(())
}

#[test]
fn check_spec_recorded_entry_replays_into_projection() {
    let candidate = CandidateCheck {
        source: CheckDiscoverySource::Cargo,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    };
    let trusted = candidate
        .promote(
            "cargo-test",
            "scope-main",
            ToolEffect::ReadOnly,
            CheckPromotion::UserApproved {
                approval_event_id: "event-approval".to_owned(),
            },
        )
        .expect("approved check promotes");
    let entry = CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        trusted,
        "event-discovery",
    );
    let projection = VerificationStateProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::CheckSpecRecorded(entry.clone()),
    )]);

    assert_eq!(
        projection.check_spec(&EvidenceScope::Task("task-1".to_owned()), "cargo-test"),
        Some(&entry)
    );
    assert_eq!(
        projection
            .check_spec(&EvidenceScope::Task("task-1".to_owned()), "cargo-test")
            .map(|entry| entry.trusted_check.source),
        Some(CheckDiscoverySource::Cargo)
    );
}

#[test]
fn check_spec_projection_is_scoped_by_evidence_scope() {
    let candidate = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand::shell("cargo test"),
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    };
    let task_one = candidate
        .clone()
        .promote(
            "cargo-test",
            "scope-one",
            ToolEffect::ReadOnly,
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config-1".to_owned(),
            },
        )
        .expect("task one check promotes");
    let task_two = candidate
        .promote(
            "cargo-test",
            "scope-two",
            ToolEffect::ReadOnly,
            CheckPromotion::ExplicitUserConfig {
                config_event_id: "event-config-2".to_owned(),
            },
        )
        .expect("task two check promotes");
    let scope_one = EvidenceScope::Task("task-1".to_owned());
    let scope_two = EvidenceScope::Task("task-2".to_owned());
    let entry_one = CheckSpecRecordedEntry::new(scope_one.clone(), task_one, "event-config-1");
    let entry_two = CheckSpecRecordedEntry::new(scope_two.clone(), task_two, "event-config-2");
    let projection = VerificationStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(entry_one.clone())),
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(entry_two.clone())),
    ]);

    assert_eq!(
        projection.check_spec(&scope_one, "cargo-test"),
        Some(&entry_one)
    );
    assert_eq!(
        projection.check_spec(&scope_two, "cargo-test"),
        Some(&entry_two)
    );
    assert_ne!(
        projection
            .check_spec(&scope_one, "cargo-test")
            .map(|entry| entry.trusted_check.check_spec.check_spec_hash.clone()),
        projection
            .check_spec(&scope_two, "cargo-test")
            .map(|entry| entry.trusted_check.check_spec.check_spec_hash.clone())
    );
}

#[test]
fn verification_config_cwd_must_stay_workspace_relative() {
    let temp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let absolute = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![VerificationCheckConfig {
            id: "bad".to_owned(),
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: Some(outside.path().to_path_buf()),
            effect: ToolEffect::ReadOnly,
        }],
        ..VerificationConfig::default()
    };
    let error = discover_candidate_checks_with_user_config(
        temp.path(),
        "trust-1",
        "event-config",
        &absolute,
    )
    .expect_err("absolute cwd should be rejected");
    assert!(error.to_string().contains("workspace-relative"));

    let parent = VerificationConfig {
        auto_run: VerificationAutoRunPolicy::Manual,
        checks: vec![VerificationCheckConfig {
            id: "bad".to_owned(),
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: Some(PathBuf::from("../outside")),
            effect: ToolEffect::ReadOnly,
        }],
        ..VerificationConfig::default()
    };
    let error =
        discover_candidate_checks_with_user_config(temp.path(), "trust-1", "event-config", &parent)
            .expect_err("parent cwd should be rejected");
    assert!(error.to_string().contains("parent components"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        symlink(outside.path(), temp.path().join("linked-outside")).expect("symlink");
        let symlinked = VerificationConfig {
            auto_run: VerificationAutoRunPolicy::Manual,
            checks: vec![VerificationCheckConfig {
                id: "bad".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["test".to_owned()],
                cwd: Some(PathBuf::from("linked-outside")),
                effect: ToolEffect::ReadOnly,
            }],
            ..VerificationConfig::default()
        };
        let error = discover_candidate_checks_with_user_config(
            temp.path(),
            "trust-1",
            "event-config",
            &symlinked,
        )
        .expect_err("external symlink cwd should be rejected");
        assert!(error.to_string().contains("outside workspace"));
    }
}

#[test]
fn untrusted_check_execution_requires_approval_or_sandbox_decision() {
    let check = check_spec("make-test");
    let mut policy = policy_with_checks(vec![check]);
    policy.workspace_trust_requirement = WorkspaceTrustRequirement::ApprovalOrSandbox;
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.workspace_trust = WorkspaceTrust::Unknown;

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::TrustWorkspace)
    );

    let mut approved = input;
    approved.workspace_trust_approval_event_id = Some("event-approval".to_owned());
    approved.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    let check = approved.policy.required_checks[0].clone();
    approved.verification_receipts.push(verification_receipt(
        "receipt-approved",
        &check,
        "snapshot-current",
        40,
        ReceiptStatus::Succeeded,
        false,
    ));

    let approved_evaluation = evaluate_readiness(&approved);

    assert_eq!(
        approved_evaluation.verification_verdict,
        VerificationVerdict::Missing
    );

    let mut receipt_bound = approved;
    receipt_bound.verification_receipts[0]
        .binding
        .approval_event_id = Some("event-approval".to_owned());
    let approved_evaluation = evaluate_readiness(&receipt_bound);

    assert_eq!(
        approved_evaluation.verification_verdict,
        VerificationVerdict::Passed
    );
}

#[test]
fn verification_private_helpers_cover_terminal_reduction_and_output_edges() {
    let pending = super::finalize_new_run(
        RunStatus::Completed,
        VerificationVerdict::Pending,
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        pending.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert!(pending.reasons.iter().any(|reason| {
        matches!(
            reason,
            ReadinessReason::PendingCheckReducedForTerminalRun { check_spec_id }
                if check_spec_id == "unknown"
        )
    }));

    let not_evaluated = super::finalize_new_run(
        RunStatus::Completed,
        VerificationVerdict::NotEvaluated,
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        not_evaluated.verification_verdict,
        VerificationVerdict::Missing
    );

    let approval_or_sandbox =
        super::sandbox_profile_hash(SandboxProfileRequirement::ApprovalOrSandbox);
    assert_ne!(
        approval_or_sandbox,
        super::sandbox_profile_hash(SandboxProfileRequirement::None)
    );

    let mut long = vec![b'a'; 5000];
    long.extend_from_slice("tail".as_bytes());
    let truncated = super::truncated_lossy(&long);
    assert!(truncated.ends_with("\n[truncated]"));
    assert!(truncated.len() < 5000);
}

#[test]
fn unknown_shell_mutation_maps_to_unknown_dirty_and_stale_when_prior_passed_exists() {
    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
    input.current_workspace_snapshot_id = Some("snapshot-old".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-pass",
        &check,
        "snapshot-old",
        8,
        ReceiptStatus::Succeeded,
        false,
    ));
    input.mutations.push(WorkspaceMutationEvidence {
        event_id: "event-shell".to_owned(),
        source_event_type: "workspace_mutation_detected".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: "scope-main".to_owned(),
        recorded_at_stream_sequence: 9,
        from_workspace_snapshot_id: Some("snapshot-old".to_owned()),
        to_workspace_snapshot_id: None,
        tool_effect: ToolEffect::Unknown,
        unknown_dirty: true,
    });

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Stale);
    assert!(evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            ReadinessReason::VerificationStale(VerificationStaleCause {
                reason: VerificationStaleReason::UnknownDirty(event_id),
                ..
            }) if event_id == "event-shell"
        )
    }));
}

#[test]
fn unknown_dirty_without_prior_pass_is_inconclusive() {
    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input.mutations.push(WorkspaceMutationEvidence {
        event_id: "event-shell".to_owned(),
        source_event_type: "workspace_mutation_detected".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: "scope-main".to_owned(),
        recorded_at_stream_sequence: 9,
        from_workspace_snapshot_id: None,
        to_workspace_snapshot_id: None,
        tool_effect: ToolEffect::Unknown,
        unknown_dirty: true,
    });

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::ResolveUnknownDirty)
    );
}

#[test]
fn mcp_unknown_dirty_adds_user_visible_source_reason() {
    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
    input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
    input
        .mutations
        .push(WorkspaceMutationEvidence::from_detected_event(
            "event-mcp".to_owned(),
            9,
            WorkspaceMutationDetected {
                operation_id: "operation-mcp".to_owned(),
                tool_call_id: None,
                tool_name: "mcp_server:docs".to_owned(),
                tool_effect: ToolEffect::Unknown,
                workspace_id: "workspace-main".to_owned(),
                scope_hash: "scope-main".to_owned(),
                from_workspace_snapshot_id: None,
                to_workspace_snapshot_id: None,
                base_workspace_revision: 0,
                workspace_revision: 1,
                reason: WorkspaceMutationDetectionReason::DeclaredWriteEffect,
                unknown_dirty: true,
                metadata: Default::default(),
            },
        ));

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert!(evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            ReadinessReason::WorkspaceMutationSource {
                event_id,
                source_label,
                recovery_hint: Some(hint),
            } if event_id == "event-mcp"
                && source_label == "MCP server docs"
                && hint == "refresh MCP or run check"
        )
    }));
    assert!(
        evaluation
            .required_actions
            .contains(&RequiredAction::ResolveUnknownDirty)
    );
}

#[test]
fn explicit_stale_causes_invalidate_old_receipts() {
    for cause in [
        VerificationStaleReason::CheckSpecChanged("event-check-spec".to_owned()),
        VerificationStaleReason::PolicyChanged("event-policy".to_owned()),
        VerificationStaleReason::EnvironmentChanged("event-env".to_owned()),
        VerificationStaleReason::SandboxChanged("event-sandbox".to_owned()),
        VerificationStaleReason::TrustChanged("event-trust".to_owned()),
    ] {
        let check = check_spec("cargo-test");
        let mut input = ReadinessInput::new_run(
            RunStatus::Completed,
            policy_with_checks(vec![check.clone()]),
        );
        input.current_workspace_snapshot_id = Some("snapshot-current".to_owned());
        input.verification_receipts.push(verification_receipt(
            "receipt-pass",
            &check,
            "snapshot-current",
            8,
            ReceiptStatus::Succeeded,
            false,
        ));
        input.stale_causes.push(VerificationStaleCause {
            reason: cause,
            from_workspace_snapshot_id: Some("snapshot-current".to_owned()),
            to_workspace_snapshot_id: Some("snapshot-current".to_owned()),
        });

        let evaluation = evaluate_readiness(&input);

        assert_eq!(evaluation.verification_verdict, VerificationVerdict::Stale);
    }
}

#[test]
fn restore_invalidates_previous_passed_verification() {
    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.current_workspace_snapshot_id = Some("snapshot-restored".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-pass",
        &check,
        "snapshot-before-restore",
        8,
        ReceiptStatus::Succeeded,
        false,
    ));
    input.stale_causes.push(VerificationStaleCause {
        reason: VerificationStaleReason::WorkspaceChanged("event-restore".to_owned()),
        from_workspace_snapshot_id: Some("snapshot-before-restore".to_owned()),
        to_workspace_snapshot_id: Some("snapshot-restored".to_owned()),
    });

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Stale);
}

#[test]
fn verification_child_worktree_passed_evidence_does_not_transfer_to_parent_after_merge() {
    let link = ChildVerificationReceiptLinked {
        parent_session_id: "parent-session".to_owned(),
        child_session_id: "child-session".to_owned(),
        child_receipt_id: "child-receipt".to_owned(),
        child_event_id: "child-event".to_owned(),
        child_workspace_id: "child-workspace".to_owned(),
        child_workspace_snapshot_id: "child-snapshot".to_owned(),
        policy_hash: "policy-hash".to_owned(),
        changeset_id: Some("changeset-1".to_owned()),
        merge_event_id: Some("event-merge".to_owned()),
    };
    link.validate().expect("complete child link is valid");

    let check = check_spec("cargo-test");
    let mut child_receipt = verification_receipt(
        "child-receipt",
        &check,
        "child-snapshot",
        8,
        ReceiptStatus::Succeeded,
        false,
    );
    child_receipt.binding.workspace_id = "child-workspace".to_owned();
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy_with_checks(vec![check]));
    input.current_workspace_snapshot_id = Some("parent-snapshot-after-merge".to_owned());
    input.verification_receipts.push(child_receipt);
    input.stale_causes.push(VerificationStaleCause {
        reason: VerificationStaleReason::WorkspaceChanged("event-merge".to_owned()),
        from_workspace_snapshot_id: Some("parent-snapshot-before-merge".to_owned()),
        to_workspace_snapshot_id: Some("parent-snapshot-after-merge".to_owned()),
    });

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Stale);
}

#[test]
fn child_receipt_link_survives_parent_session_restore() {
    let link = ChildVerificationReceiptLinked {
        parent_session_id: "parent-session".to_owned(),
        child_session_id: "child-session".to_owned(),
        child_receipt_id: "child-receipt".to_owned(),
        child_event_id: "child-event".to_owned(),
        child_workspace_id: "child-workspace".to_owned(),
        child_workspace_snapshot_id: "child-snapshot".to_owned(),
        policy_hash: "policy-hash".to_owned(),
        changeset_id: Some("changeset-1".to_owned()),
        merge_event_id: Some("event-merge".to_owned()),
    };

    let encoded = serde_json::to_string(&link).expect("child link serializes");
    let restored: ChildVerificationReceiptLinked =
        serde_json::from_str(&encoded).expect("child link deserializes");

    assert_eq!(restored, link);
    restored
        .validate()
        .expect("restored child receipt link remains traceable");
}

#[test]
fn final_text_cannot_force_passed() {
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check_spec("cargo-test")]),
    );
    input.final_assistant_event_id = Some("assistant-final".to_owned());

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::FinalAssistantTextIgnored {
                event_id: "assistant-final".to_owned()
            })
    );
}

#[test]
fn task_step_with_recovered_tool_error_is_not_silently_verified() {
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check_spec("cargo-test")]),
    );
    input.final_assistant_event_id = Some("assistant-final".to_owned());
    input
        .recovered_tool_error_event_ids
        .push("tool-error-recovered".to_owned());

    let evaluation = evaluate_readiness(&input);

    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert_eq!(
        evaluation.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::RecoveredToolError {
                event_id: "tool-error-recovered".to_owned()
            })
    );
    assert!(
        evaluation
            .reasons
            .contains(&ReadinessReason::FinalAssistantTextIgnored {
                event_id: "assistant-final".to_owned()
            })
    );
}

#[test]
fn cancellation_preserves_independent_verification_verdict() {
    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(RunStatus::Cancelled, policy_with_checks(vec![check]));
    input.skip_decision = Some(VerificationSkipDecision {
        event_id: "event-cancel-skip".to_owned(),
        reason: "user cancelled".to_owned(),
    });
    input.policy.allow_unverified_completion = true;

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.run_status, RunStatus::Cancelled);
    assert_eq!(
        evaluation.verification_verdict,
        VerificationVerdict::Skipped
    );
    assert_eq!(evaluation.visible_state, VisibleCompletionState::Cancelled);
}

#[test]
fn policy_merge_covers_duplicate_child_ids_and_timeout_edges() {
    let parent = VerificationPolicy {
        required_checks: Vec::new(),
        completion_criteria: CompletionCriteria::NoChecksRequired,
        verification_scope: VerificationScope::all_tracked("scope-main"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: true,
        timeout_ms: None,
        auto_run: VerificationAutoRunPolicy::Manual,
    };
    let child = VerificationPolicy {
        required_checks: vec![
            check_spec("cargo-test"),
            CheckSpec::new(
                "cargo-test",
                CheckCommand::shell("cargo test --doc"),
                ToolEffect::ReadOnly,
                "scope-main",
            ),
        ],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked("scope-main"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: true,
        timeout_ms: Some(10_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    };

    let merged = parent.merge_child(&child).expect("child tightens parent");

    assert_eq!(merged.required_checks.len(), 1);
    assert_eq!(merged.timeout_ms, Some(10_000));
    assert_eq!(merged.auto_run, VerificationAutoRunPolicy::Manual);

    let trusted_child = VerificationPolicy {
        auto_run: VerificationAutoRunPolicy::TrustedOnly,
        ..merged.clone()
    };
    assert_eq!(
        merged
            .merge_child(&trusted_child)
            .expect("trusted-only cannot relax manual auto-run")
            .auto_run,
        VerificationAutoRunPolicy::Manual
    );
    let never_parent = VerificationPolicy {
        auto_run: VerificationAutoRunPolicy::Never,
        ..merged.clone()
    };
    assert_eq!(
        never_parent
            .merge_child(&trusted_child)
            .expect("manual child cannot relax never auto-run")
            .auto_run,
        VerificationAutoRunPolicy::Never
    );

    let no_timeout = VerificationPolicy {
        timeout_ms: None,
        ..merged.clone()
    };
    assert_eq!(
        no_timeout
            .merge_child(&no_timeout)
            .expect("same policy merges")
            .timeout_ms,
        None
    );
}

#[test]
fn policy_inheritance_cannot_relax_parent_requirements() {
    let parent_check = check_spec("cargo-test");
    let parent = VerificationPolicy {
        required_checks: vec![parent_check.clone()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope {
            scope_hash: "scope-main".to_owned(),
            include: vec!["src/**".to_owned()],
            exclude: vec!["target/**".to_owned()],
            tracked_files_only: true,
            max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
            generated_roots: Vec::new(),
        },
        sandbox_profile: SandboxProfileRequirement::Sandboxed,
        workspace_trust_requirement: WorkspaceTrustRequirement::Trusted,
        allow_unverified_completion: false,
        timeout_ms: Some(60_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    };
    let relaxed_scope = VerificationPolicy {
        verification_scope: VerificationScope {
            scope_hash: "scope-child".to_owned(),
            include: vec!["tests/**".to_owned()],
            exclude: vec!["target/**".to_owned()],
            tracked_files_only: true,
            max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
            generated_roots: Vec::new(),
        },
        allow_unverified_completion: true,
        timeout_ms: Some(120_000),
        ..parent.clone()
    };

    let error = parent
        .merge_child(&relaxed_scope)
        .expect_err("child scope cannot drop parent include paths");

    assert!(error.to_string().contains("scope"));

    let all_tracked_parent = VerificationPolicy {
        verification_scope: VerificationScope::all_tracked("scope-main"),
        ..parent.clone()
    };
    let narrowed_from_all_tracked = VerificationPolicy {
        verification_scope: VerificationScope {
            scope_hash: "scope-child".to_owned(),
            include: vec!["src/**".to_owned()],
            exclude: all_tracked_parent.verification_scope.exclude.clone(),
            tracked_files_only: true,
            max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
            generated_roots: Vec::new(),
        },
        ..all_tracked_parent.clone()
    };
    let error = all_tracked_parent
        .merge_child(&narrowed_from_all_tracked)
        .expect_err("child cannot narrow parent all-tracked scope");
    assert!(error.to_string().contains("scope"));

    let tightened = VerificationPolicy {
        required_checks: vec![check_spec("cargo-clippy")],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: parent.verification_scope.clone(),
        sandbox_profile: SandboxProfileRequirement::Sandboxed,
        workspace_trust_requirement: WorkspaceTrustRequirement::Trusted,
        allow_unverified_completion: false,
        timeout_ms: Some(30_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    };
    let merged = parent
        .merge_child(&tightened)
        .expect("tightening policy should merge");

    assert_eq!(merged.required_checks.len(), 2);
    assert!(!merged.allow_unverified_completion);
    assert_eq!(merged.timeout_ms, Some(30_000));
    assert_eq!(
        merged.workspace_trust_requirement,
        WorkspaceTrustRequirement::Trusted
    );
    let same_check_child = VerificationPolicy {
        required_checks: vec![parent_check],
        verification_scope: parent.verification_scope.clone(),
        sandbox_profile: SandboxProfileRequirement::Sandboxed,
        workspace_trust_requirement: WorkspaceTrustRequirement::Trusted,
        allow_unverified_completion: false,
        timeout_ms: None,
        ..parent.clone()
    };
    let same_check_merged = parent
        .merge_child(&same_check_child)
        .expect("same check id and hash should not duplicate");
    assert_eq!(same_check_merged.required_checks.len(), 1);

    let redefined = VerificationPolicy {
        required_checks: vec![CheckSpec::new(
            "cargo-test",
            CheckCommand::shell("cargo test --doc"),
            ToolEffect::ReadOnly,
            "scope-main",
        )],
        ..tightened
    };
    let error = parent
        .merge_child(&redefined)
        .expect_err("child cannot redefine a parent check id");
    assert!(error.to_string().contains("redefines required check"));
}

#[test]
fn workspace_snapshot_id_is_content_bound_and_rejects_incomplete_manifest() {
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        entries: vec![
            WorkspaceSnapshotEntry {
                normalized_path: PathBuf::from("b.rs"),
                file_type: FileType::File,
                content_hash: Some("sha256:b".to_owned()),
                mode: Some(0o644),
                file_metadata: None,
                symlink_target: None,
                state: SnapshotEntryState::Present,
            },
            WorkspaceSnapshotEntry {
                normalized_path: PathBuf::from("a.rs"),
                file_type: FileType::File,
                content_hash: Some("sha256:a".to_owned()),
                mode: Some(0o644),
                file_metadata: None,
                symlink_target: None,
                state: SnapshotEntryState::Present,
            },
        ],
    };
    let reordered = WorkspaceSnapshotManifestV1 {
        entries: manifest.entries.iter().cloned().rev().collect(),
        ..manifest.clone()
    };

    assert_eq!(
        manifest
            .workspace_snapshot_id()
            .expect("clean manifest hashes"),
        reordered
            .workspace_snapshot_id()
            .expect("reordered clean manifest hashes")
    );

    let incomplete = WorkspaceSnapshotManifestV1 {
        entries: vec![WorkspaceSnapshotEntry {
            normalized_path: PathBuf::from("secret.env"),
            file_type: FileType::File,
            content_hash: None,
            mode: None,
            file_metadata: None,
            symlink_target: None,
            state: SnapshotEntryState::PermissionDenied,
        }],
        ..manifest
    };
    assert!(incomplete.workspace_snapshot_id().is_err());
}

#[test]
fn workspace_snapshot_builder_hashes_scope_and_excludes_build_outputs() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("src")).expect("src dir");
    fs::create_dir_all(temp.path().join("target/debug")).expect("target dir");
    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source file");
    fs::write(temp.path().join("target/debug/generated"), "ignored").expect("target file");
    let scope = VerificationScope::all_tracked("scope-main");

    let first =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert!(first.workspace_snapshot_id.is_some());
    assert_eq!(first.workspace_knowledge, WorkspaceKnowledge::Clean(1));
    assert!(
        first
            .manifest
            .entries
            .iter()
            .any(|entry| entry.normalized_path == Path::new("src/lib.rs"))
    );
    assert!(
        first
            .manifest
            .entries
            .iter()
            .all(|entry| !entry.normalized_path.starts_with("target"))
    );

    fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 2 }\n",
    )
    .expect("source rewrite");
    let second =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 2).expect("snapshot rebuilds");

    assert_ne!(first.workspace_snapshot_id, second.workspace_snapshot_id);
}

#[test]
fn workspace_snapshot_builder_includes_repo_local_skill_files() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join(".sigil/skills/review")).expect("skill dir");
    fs::create_dir_all(temp.path().join(".sigil/sessions")).expect("session dir");
    fs::write(
        temp.path().join(".sigil/skills/review/SKILL.md"),
        "name: review\n",
    )
    .expect("skill file");
    fs::write(temp.path().join(".sigil/sessions/session.jsonl"), "{}\n").expect("session file");
    let scope = VerificationScope::all_tracked("scope-main");

    let first =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert!(first.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new(".sigil/skills/review/SKILL.md")
            && entry.state == SnapshotEntryState::Present
    }));
    assert!(first.manifest.entries.iter().all(|entry| {
        !entry
            .normalized_path
            .starts_with(Path::new(".sigil/sessions"))
    }));

    fs::write(
        temp.path().join(".sigil/skills/review/SKILL.md"),
        "name: review\nversion: 2\n",
    )
    .expect("skill rewrite");
    let second =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 2).expect("snapshot rebuilds");

    assert_ne!(first.workspace_snapshot_id, second.workspace_snapshot_id);
}

#[test]
fn workspace_snapshot_builder_marks_large_files_unsupported() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = fs::File::create(temp.path().join("large.bin")).expect("large file");
    file.set_len(MAX_WORKSPACE_SNAPSHOT_FILE_BYTES + 1)
        .expect("sparse file length");
    let scope = VerificationScope::all_tracked("scope-main");

    let snapshot =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert_eq!(
        snapshot.workspace_knowledge,
        WorkspaceKnowledge::UnknownDirty
    );
    assert!(snapshot.workspace_snapshot_id.is_none());
    assert!(snapshot.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("large.bin")
            && entry.file_type == FileType::File
            && entry.content_hash.is_none()
            && entry.state == SnapshotEntryState::Unsupported
    }));
}

#[test]
fn workspace_snapshot_builder_respects_scope_max_file_bytes() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("note.txt"), b"abcd").expect("small file");
    let low_limit_scope = VerificationScope {
        max_file_bytes: 3,
        ..VerificationScope::all_tracked("scope-main")
    };

    let unsupported = build_workspace_snapshot(temp.path(), "workspace-1", &low_limit_scope, 1)
        .expect("snapshot builds");

    assert_eq!(
        unsupported.workspace_knowledge,
        WorkspaceKnowledge::UnknownDirty
    );
    assert!(unsupported.workspace_snapshot_id.is_none());
    assert!(unsupported.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("note.txt")
            && entry.content_hash.is_none()
            && entry.state == SnapshotEntryState::Unsupported
    }));

    let capturing_scope = VerificationScope {
        max_file_bytes: 4,
        ..low_limit_scope
    };
    let captured = build_workspace_snapshot(temp.path(), "workspace-1", &capturing_scope, 2)
        .expect("snapshot rebuilds");

    assert_eq!(captured.workspace_knowledge, WorkspaceKnowledge::Clean(2));
    assert!(captured.workspace_snapshot_id.is_some());
    assert!(captured.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("note.txt")
            && entry.content_hash.is_some()
            && entry.state == SnapshotEntryState::Present
    }));
}

#[test]
fn workspace_snapshot_builder_records_comparable_file_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("note.txt");
    fs::write(&path, b"metadata").expect("source");
    let scope = VerificationScope::all_tracked("scope-main");

    let first =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");
    let entry = first
        .manifest
        .entries
        .iter()
        .find(|entry| entry.normalized_path == Path::new("note.txt"))
        .expect("snapshot entry");
    let file_metadata = entry.file_metadata.as_ref().expect("file metadata");
    assert_eq!(
        file_metadata.readonly,
        fs::metadata(&path)
            .expect("file metadata")
            .permissions()
            .readonly()
    );
    #[cfg(unix)]
    {
        assert_eq!(file_metadata.platform, FileMetadataPlatform::Unix);
        assert_eq!(file_metadata.unix_mode, entry.mode);
        assert!(entry.mode.is_some());
    }
    #[cfg(windows)]
    {
        assert_eq!(file_metadata.platform, FileMetadataPlatform::Windows);
        assert_eq!(entry.mode, None);
        assert_eq!(file_metadata.unix_mode, None);
    }
    #[cfg(not(any(unix, windows)))]
    {
        assert_eq!(file_metadata.platform, FileMetadataPlatform::Other);
        assert_eq!(entry.mode, None);
        assert_eq!(file_metadata.unix_mode, None);
    }

    let original_permissions = fs::metadata(&path).expect("file metadata").permissions();
    let mut readonly_permissions = original_permissions.clone();
    readonly_permissions.set_readonly(true);
    fs::set_permissions(&path, readonly_permissions).expect("set readonly");
    let second =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 2).expect("snapshot rebuilds");
    assert_ne!(first.workspace_snapshot_id, second.workspace_snapshot_id);

    fs::set_permissions(&path, original_permissions).expect("restore writable");
}

#[test]
fn workspace_snapshot_builder_records_missing_literal_includes_as_clean() {
    let temp = tempfile::tempdir().expect("tempdir");
    let scope = VerificationScope {
        scope_hash: "scope-main".to_owned(),
        include: vec!["src/missing.rs".to_owned()],
        exclude: Vec::new(),
        tracked_files_only: true,
        max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
        generated_roots: Vec::new(),
    };

    let snapshot =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert!(snapshot.workspace_snapshot_id.is_some());
    assert_eq!(snapshot.manifest.entries.len(), 1);
    assert_eq!(
        snapshot.manifest.entries[0].state,
        SnapshotEntryState::Missing
    );
}

#[test]
fn workspace_snapshot_builder_uses_git_tracked_and_unignored_file_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    run_git(temp.path(), &["init"]);
    fs::write(temp.path().join(".gitignore"), "*.log\ngenerated/\n").expect("gitignore");
    fs::write(temp.path().join("tracked_ignored.log"), "tracked ignored").expect("tracked file");
    fs::write(temp.path().join("ignored.log"), "ignored").expect("ignored file");
    fs::create_dir_all(temp.path().join("src")).expect("src dir");
    fs::write(temp.path().join("src/new.rs"), "pub fn new() {}\n").expect("new source");
    fs::create_dir_all(temp.path().join("generated")).expect("generated dir");
    fs::write(
        temp.path().join("generated/out.rs"),
        "pub fn generated() {}\n",
    )
    .expect("generated file");
    run_git(temp.path(), &["add", ".gitignore"]);
    run_git(temp.path(), &["add", "-f", "tracked_ignored.log"]);
    let scope = VerificationScope {
        generated_roots: vec![PathBuf::from("generated")],
        ..VerificationScope::all_tracked("scope-main")
    };

    let snapshot =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");
    let paths = snapshot
        .manifest
        .entries
        .iter()
        .map(|entry| entry.normalized_path.clone())
        .collect::<Vec<_>>();

    assert!(paths.contains(&PathBuf::from("tracked_ignored.log")));
    assert!(paths.contains(&PathBuf::from("src/new.rs")));
    assert!(!paths.contains(&PathBuf::from("ignored.log")));
    assert!(!paths.contains(&PathBuf::from("generated/out.rs")));
}

#[test]
fn workspace_snapshot_builder_records_deleted_git_paths_as_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    run_git(temp.path(), &["init"]);
    fs::write(temp.path().join("deleted.rs"), "pub fn deleted() {}\n").expect("deleted source");
    run_git(temp.path(), &["add", "deleted.rs"]);
    fs::remove_file(temp.path().join("deleted.rs")).expect("delete tracked file");
    let scope = VerificationScope::all_tracked("scope-main");

    let snapshot =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert!(snapshot.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("deleted.rs")
            && entry.file_type == FileType::File
            && entry.state == SnapshotEntryState::Missing
    }));
}

#[test]
fn workspace_snapshot_builder_filters_non_included_files_and_direct_unsupported_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("src")).expect("src dir");
    fs::write(temp.path().join("src/lib.rs"), "pub fn value() {}\n").expect("source");
    fs::write(temp.path().join("README.md"), "ignored\n").expect("readme");
    let scope = VerificationScope {
        scope_hash: "scope-main".to_owned(),
        include: vec!["src/lib.rs".to_owned()],
        exclude: Vec::new(),
        tracked_files_only: false,
        max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
        generated_roots: Vec::new(),
    };

    let snapshot =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert_eq!(snapshot.manifest.entries.len(), 1);
    assert_eq!(
        snapshot.manifest.entries[0].normalized_path,
        PathBuf::from("src/lib.rs")
    );

    let metadata = fs::metadata(temp.path().join("src")).expect("dir metadata");
    let direct = super::snapshot_entry_for_path(
        temp.path(),
        &temp.path().join("src"),
        PathBuf::from("src"),
        &metadata,
        MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
    );
    assert_eq!(direct.file_type, FileType::Other);
    assert_eq!(direct.state, SnapshotEntryState::Unsupported);
}

#[cfg(unix)]
#[test]
fn workspace_snapshot_builder_records_internal_and_broken_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("src")).expect("src dir");
    fs::write(temp.path().join("src/lib.rs"), "pub fn value() {}\n").expect("source");
    symlink("src/lib.rs", temp.path().join("inside-link")).expect("internal symlink");
    symlink("missing-target", temp.path().join("broken-link")).expect("broken symlink");
    let scope = VerificationScope {
        scope_hash: "scope-main".to_owned(),
        include: vec!["**/*".to_owned()],
        exclude: super::default_scope_excludes(),
        tracked_files_only: false,
        max_file_bytes: MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
        generated_roots: Vec::new(),
    };

    let snapshot =
        build_workspace_snapshot(temp.path(), "workspace-1", &scope, 1).expect("snapshot builds");

    assert!(snapshot.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("inside-link")
            && entry.file_type == FileType::Symlink
            && entry.state == SnapshotEntryState::Present
            && entry.symlink_target.is_some()
    }));
    assert!(snapshot.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("broken-link")
            && entry.file_type == FileType::Symlink
            && entry.state == SnapshotEntryState::Unsupported
    }));
    assert_eq!(
        snapshot.workspace_knowledge,
        WorkspaceKnowledge::UnknownDirty
    );
}

#[test]
fn snapshot_entry_completeness_covers_directory_and_unsupported_states() {
    let directory = WorkspaceSnapshotEntry {
        normalized_path: PathBuf::from("src"),
        file_type: FileType::Directory,
        content_hash: None,
        mode: Some(0o755),
        file_metadata: None,
        symlink_target: None,
        state: SnapshotEntryState::Present,
    };
    assert!(directory.is_complete());
    let other = WorkspaceSnapshotEntry {
        file_type: FileType::Other,
        ..directory.clone()
    };
    assert!(other.is_complete());
    for state in [
        SnapshotEntryState::PermissionDenied,
        SnapshotEntryState::External,
        SnapshotEntryState::Unsupported,
    ] {
        assert!(
            !WorkspaceSnapshotEntry {
                state,
                ..directory.clone()
            }
            .is_complete()
        );
    }
}

#[cfg(unix)]
#[test]
fn workspace_snapshot_builder_marks_external_symlink_unknown_dirty() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    fs::write(outside.path().join("secret.txt"), "secret").expect("outside file");
    symlink(outside.path().join("secret.txt"), temp.path().join("leak")).expect("symlink");
    let scope = VerificationScope::all_tracked("scope-main");

    let snapshot = build_workspace_snapshot_for_event(
        temp.path(),
        "workspace-1",
        &scope,
        1,
        "event-snapshot",
        9,
    )
    .expect("snapshot builds");

    assert_eq!(snapshot.workspace_snapshot_id, None);
    assert_eq!(
        snapshot.workspace_knowledge,
        WorkspaceKnowledge::UnknownDirty
    );
    assert_eq!(
        snapshot
            .unknown_dirty_evidence
            .as_ref()
            .map(|evidence| evidence.event_id.as_str()),
        Some("event-snapshot")
    );
    assert!(snapshot.manifest.entries.iter().any(|entry| {
        entry.normalized_path == Path::new("leak") && entry.state == SnapshotEntryState::External
    }));

    let check = check_spec("cargo-test");
    let mut input = ReadinessInput::new_run(
        RunStatus::Completed,
        policy_with_checks(vec![check.clone()]),
    );
    input.workspace_knowledge = snapshot.workspace_knowledge;
    input.current_workspace_snapshot_id = Some("snapshot-before".to_owned());
    input.verification_receipts.push(verification_receipt(
        "receipt-pass",
        &check,
        "snapshot-before",
        8,
        ReceiptStatus::Succeeded,
        false,
    ));
    input.mutations.push(
        snapshot
            .unknown_dirty_evidence
            .expect("unknown dirty evidence"),
    );

    let evaluation = evaluate_readiness(&input);

    assert_eq!(evaluation.verification_verdict, VerificationVerdict::Stale);
}

#[cfg(unix)]
#[test]
fn verification_check_runner_uses_synthetic_snapshot_id_for_incomplete_scope() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path().join("secret.txt"), temp.path().join("leak"))?;
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    let recorded = run_verification_check_with_fake_backend(
        &mut session,
        VerificationCheckRunRequest {
            workspace_root: temp.path().to_path_buf(),
            scope: EvidenceScope::Step("task_1:step_1".to_owned()),
            trusted_check: trusted_check.clone(),
            policy: policy_with_checks(vec![trusted_check.check_spec.clone()]),
            policy_hash: None,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_snapshot_id: "user-config".to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )?;

    assert!(recorded.receipt.mutates_verification_scope);
    assert_eq!(recorded.receipt.check_status, ReceiptStatus::Inconclusive);
    assert_eq!(
        recorded.receipt.receipt.workspace_snapshot_id.as_deref(),
        Some(recorded.receipt.binding.workspace_snapshot_id.as_str())
    );
    assert_eq!(
        recorded
            .receipt
            .binding
            .workspace_snapshot_id
            .chars()
            .filter(|character| *character == '-')
            .count(),
        4
    );
    Ok(())
}

#[test]
fn workspace_snapshot_for_event_requires_nonzero_source_sequence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let error = build_workspace_snapshot_for_event(
        temp.path(),
        "workspace-1",
        &VerificationScope::all_tracked("scope-main"),
        1,
        "event-snapshot",
        0,
    )
    .expect_err("zero stream sequence should fail");

    assert!(
        error
            .to_string()
            .contains("source stream sequence must be non-zero")
    );
}

fn run_git(workspace: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn policy_with_checks(required_checks: Vec<CheckSpec>) -> VerificationPolicy {
    VerificationPolicy {
        required_checks,
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked("scope-main"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: VerificationAutoRunPolicy::Manual,
    }
}

fn check_spec(id: &str) -> CheckSpec {
    CheckSpec::new(
        id,
        CheckCommand::shell(id.replace('-', " ")),
        ToolEffect::ReadOnly,
        "scope-main",
    )
}

fn trusted_plugin_check(id: &str, effect: ToolEffect) -> Result<crate::TrustedCheckSpec> {
    CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "plugin-hook".to_owned(),
            args: vec![id.to_owned()],
            cwd: None,
        },
        source_event_id: "event-plugin-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        id,
        "scope-main",
        effect,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-plugin-config".to_owned(),
        },
    )
}

fn plugin_verification_hook_started(effect: ToolEffect) -> PluginHookExecutionStartedEntry {
    let capabilities = ExecutionBackendCapabilities {
        filesystem_isolation: true,
        network_isolation: true,
        process_isolation: true,
        resource_limits: false,
        persistent_pty: false,
        workspace_snapshot: false,
    };
    PluginHookExecutionStartedEntry {
        execution_id: "plugin-hook-exec-1".to_owned(),
        plugin_id: "repo-review".to_owned(),
        manifest_hash: "sha256:manifest".to_owned(),
        capability_digest: "sha256:capability".to_owned(),
        hook_id: "verify-repo".to_owned(),
        hook_kind: PluginHookKind::Verification,
        command: vec!["plugin-hook".to_owned(), "verify-repo".to_owned()],
        declared_effect: effect,
        timeout_ms: 30_000,
        backend: ExecutionBackendKind::MacosSeatbelt,
        backend_capabilities: capabilities,
    }
}

fn plugin_verification_hook_finished(
    started: &PluginHookExecutionStartedEntry,
    status: PluginHookExecutionStatus,
) -> PluginHookExecutionFinishedEntry {
    PluginHookExecutionFinishedEntry {
        execution_id: started.execution_id.clone(),
        plugin_id: started.plugin_id.clone(),
        manifest_hash: started.manifest_hash.clone(),
        capability_digest: started.capability_digest.clone(),
        hook_id: started.hook_id.clone(),
        hook_kind: started.hook_kind,
        status,
        exit_code: match status {
            PluginHookExecutionStatus::Succeeded => Some(0),
            PluginHookExecutionStatus::Failed => Some(1),
            PluginHookExecutionStatus::TimedOut => None,
        },
        stdout_bytes: 18,
        stderr_bytes: 0,
        timed_out: status == PluginHookExecutionStatus::TimedOut,
        backend: started.backend,
        backend_capabilities: started.backend_capabilities,
        network: ExecutionNetworkReceipt::denied("plugin hook sandbox denied network"),
        resources: Default::default(),
    }
}

fn plugin_verification_hook_output(
    started: &PluginHookExecutionStartedEntry,
) -> PluginHookOutputEnvelope {
    PluginHookOutputEnvelope {
        execution_id: started.execution_id.clone(),
        plugin_id: started.plugin_id.clone(),
        hook_id: started.hook_id.clone(),
        stdout: PluginHookOutputStream {
            content: "all checks passed\n".to_owned(),
            total_bytes: 18,
            returned_bytes: 18,
            omitted_bytes: 0,
            total_lines: 1,
            returned_lines: 1,
            truncated: false,
            redaction_state: RedactionState::None,
        },
        stderr: PluginHookOutputStream {
            content: String::new(),
            total_bytes: 0,
            returned_bytes: 0,
            omitted_bytes: 0,
            total_lines: 0,
            returned_lines: 0,
            truncated: false,
            redaction_state: RedactionState::None,
        },
        artifact_refs: Vec::new(),
        artifact_refs_truncated: false,
        redaction_state: RedactionState::None,
        parse_error: None,
        model_visible_summary: "plugin verification hook completed".to_owned(),
    }
}

fn workspace_mutation(event_id: &str, sequence: u64) -> WorkspaceMutationEvidence {
    WorkspaceMutationEvidence {
        event_id: event_id.to_owned(),
        source_event_type: "mutation_committed".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: "scope-main".to_owned(),
        recorded_at_stream_sequence: sequence,
        from_workspace_snapshot_id: None,
        to_workspace_snapshot_id: Some(format!("snapshot-{sequence}")),
        tool_effect: ToolEffect::WorkspaceWrite,
        unknown_dirty: false,
    }
}

fn verification_receipt(
    receipt_id: &str,
    check: &CheckSpec,
    snapshot_id: &str,
    sequence: u64,
    status: ReceiptStatus,
    mutates_verification_scope: bool,
) -> VerificationReceipt {
    VerificationReceipt {
        receipt: EvidenceReceipt {
            workspace_snapshot_id: Some(snapshot_id.to_owned()),
            status,
            ..base_evidence_receipt(receipt_id, sequence, status)
        },
        binding: VerificationBinding {
            workspace_id: "workspace-1".to_owned(),
            workspace_snapshot_id: snapshot_id.to_owned(),
            verification_scope_hash: "scope-main".to_owned(),
            check_spec_hash: check.check_spec_hash.clone(),
            environment_fingerprint: "env-1".to_owned(),
            sandbox_profile_hash: "sandbox-local".to_owned(),
            execution_backend: None,
            execution_backend_capabilities: None,
            execution_network: Default::default(),
            workspace_trust_snapshot_id: "trust-1".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: check.check_spec_id.clone(),
        check_status: status,
        failure_reason: None,
        mutates_verification_scope,
    }
}

fn base_evidence_receipt(
    receipt_id: &str,
    sequence: u64,
    status: ReceiptStatus,
) -> EvidenceReceipt {
    EvidenceReceipt {
        receipt_id: receipt_id.to_owned(),
        source_session_id: "session-1".to_owned(),
        source_event_id: format!("event-{receipt_id}"),
        source_event_type: "check_finished".to_owned(),
        scope: EvidenceScope::Run("run-1".to_owned()),
        producer_tool_call: Some("tool-call-1".to_owned()),
        workspace_revision: Some(1),
        workspace_snapshot_id: Some("snapshot-current".to_owned()),
        policy_hash: Some("policy-hash".to_owned()),
        changeset_id: None,
        status,
        artifact_refs: Vec::new(),
        redaction_state: RedactionState::None,
        recorded_at_stream_sequence: sequence,
    }
}
