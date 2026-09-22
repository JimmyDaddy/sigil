//! Actual managed check execution feeds the same Direct Task readiness used by the product.
use super::*;
use sigil_kernel::{
    CandidateCheck, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpecRecordedEntry,
    CompletionCriteria, EvidenceScope, ReceiptStatus, ToolEffect, VerificationAutoRunPolicy,
    VerificationCheckRunRequest, VerificationPolicy, VerificationPolicyChangedEntry,
    VerificationVerdict, WorkspaceTrust,
};

struct VerificationProviderBuilder {
    wrapper: bool,
    calls: Arc<AtomicUsize>,
}
struct VerificationProvider {
    wrapper: bool,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl TaskRoleProviderBuilder for VerificationProviderBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        if role == AgentRole::Executor {
            Ok(Box::new(VerificationProvider {
                wrapper: self.wrapper,
                calls: Arc::clone(&self.calls),
            }))
        } else {
            Ok(Box::new(ApplicationTaskRoleProvider { role }))
        }
    }
}
#[async_trait]
impl Provider for VerificationProvider {
    fn name(&self) -> &str {
        "application-task-test"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if self.wrapper && index == 0 {
            let args_json = serde_json::json!({"command":"bash -c 'false | cat'"}).to_string();
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "pipeline-wrapper".to_owned(),
                    name: "exec_command".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "pipeline-wrapper".to_owned(),
                    delta: args_json.clone(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "pipeline-wrapper".to_owned(),
                    name: "exec_command".to_owned(),
                    args_json,
                })),
                Ok(ProviderChunk::Done),
            ]
        } else {
            scripted_task_completion_chunks(
                &request,
                "Execution observation recorded.",
                "verification-completion",
            )
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}
fn services(root: &Path, wrapper: bool) -> Result<(std::path::PathBuf, ApplicationRunServices)> {
    let config_path = root.join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::TaskOrchestration);
    config.permission.mode = sigil_kernel::PermissionMode::DangerFullAccess;
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        root,
    )?
    .with_task_role_provider_builder(Arc::new(VerificationProviderBuilder {
        wrapper,
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    Ok((config_path, services))
}
fn seed_task(session: &mut Session, parent: SessionRef, task: &TaskId) -> Result<()> {
    let admission = crate::direct_plan_fixture::append(
        session,
        task,
        "Evaluate only declared verification evidence",
    )?;
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task.clone(),
            parent_session_ref: parent,
            objective: "Evaluate only declared verification evidence".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(admission),
    ])
}
fn check(
    session: &mut Session,
    task: &TaskId,
    command: CheckCommand,
    auto: VerificationAutoRunPolicy,
) -> Result<(sigil_kernel::TrustedCheckSpec, VerificationPolicy)> {
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command,
        source_event_id: "declared-check".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "declared-check",
        "bounded-source",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "declared-check".to_owned(),
        },
    )?;
    let scope = EvidenceScope::Task(task.as_str().to_owned());
    let mut policy = VerificationPolicy::no_checks_required("bounded-source");
    policy.required_checks = vec![trusted.check_spec.clone()];
    policy.verification_scope.include = vec!["src/**".to_owned()];
    policy.verification_scope.tracked_files_only = false;
    policy.completion_criteria = CompletionCriteria::AllRequiredChecks;
    policy.allow_unverified_completion = false;
    policy.auto_run = auto;
    session.append_controls(vec![
        ControlEntry::CheckSpecRecorded(CheckSpecRecordedEntry::new(
            scope.clone(),
            trusted.clone(),
            "declared-check",
        )),
        ControlEntry::VerificationPolicyChanged(VerificationPolicyChangedEntry::new(
            scope,
            policy.clone(),
            "declared-policy",
        )?),
    ])?;
    Ok((trusted, policy))
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn real_pipeline_wrapper_zero_does_not_become_passed_verification() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    for configured in [false, true] {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("src"))?;
        std::fs::write(root.path().join("src/input"), "stable")?;
        let (config_path, services) = services(root.path(), true)?;
        let mut prepared = Box::pin(prepare_application_run(
            ApplicationRunRequest::non_interactive(
                &config_path,
                root.path(),
                "pipeline boundary",
                "pipeline-seed",
            ),
            &services,
        ))
        .await?;
        let path = prepared.session_log_path().to_path_buf();
        let identity = prepared.session_id().to_owned();
        let task = TaskId::new("pipeline-task")?;
        let parent = prepared.execution.parent_session_ref.clone();
        seed_task(&mut prepared.execution.session, parent, &task)?;
        if configured {
            check(
                &mut prepared.execution.session,
                &task,
                CheckCommand {
                    command: "bash".to_owned(),
                    args: vec!["-c".to_owned(), "false | cat".to_owned()],
                    cwd: None,
                },
                VerificationAutoRunPolicy::TrustedOnly,
            )?;
        }
        drop(prepared);
        let continued = Box::pin(prepare_application_task_continuation(
            ApplicationTaskContinuationRequest {
                config_path,
                launch_cwd: root.path().to_path_buf(),
                session_path: path.clone(),
                session_attachment: None,
                expected_session_scope_id: identity,
                run_id: "pipeline-run".to_owned(),
                task_id: task.clone(),
                guidance: None,
                interaction: ApplicationRunInteraction::NonInteractive,
                permission_mode: None,
            },
            &services,
        ))
        .await?;
        let (execution, control) = continued.into_parts();
        let output = Box::pin(execution.execute(
            &mut RecordingApplicationRunEvents::default(),
            &mut AutoApproveHandler,
        ))
        .await?;
        drop(control);
        let session = Session::load_from_store(
            "application-task-test",
            "gpt-test",
            JsonlSessionStore::new(path)?,
        )?;
        let wrapper_result = session
            .entries()
            .iter()
            .find_map(|entry| match entry {
                SessionLogEntry::ToolResultV3(result) if result.call_id == "pipeline-wrapper" => {
                    Some(result)
                }
                _ => None,
            })
            .expect("real wrapper result");
        assert_eq!(
            wrapper_result.facts.exit_code,
            Some(0),
            "wrapper facts: {:?}",
            wrapper_result.facts
        );
        assert_eq!(wrapper_result.facts.status, "ok");
        let projection = session.verification_state_projection();
        let readiness = projection
            .latest_readiness(&EvidenceScope::Task(task.as_str().to_owned()))
            .expect("Task readiness");
        assert_ne!(
            readiness.evaluation.verification_verdict,
            VerificationVerdict::Passed
        );
        let receipts = session
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::VerificationRecorded(record)) => {
                    Some(record)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if configured {
            assert_eq!(output.task_status, TaskRunStatus::Paused);
            assert!(!receipts.is_empty());
            assert!(
                receipts
                    .iter()
                    .all(|record| record.receipt.check_status == ReceiptStatus::Inconclusive)
            );
        } else {
            assert_eq!(output.task_status, TaskRunStatus::Completed);
            assert_ne!(
                readiness.evaluation.verification_verdict,
                VerificationVerdict::Passed,
                "an unchecked shell result cannot become verification evidence"
            );
            assert!(receipts.is_empty());
        }
    }
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn real_check_receipt_is_reused_only_when_its_source_scope_is_unchanged() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    for related in [false, true] {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("src"))?;
        std::fs::write(root.path().join("src/input"), "valid\n")?;
        let (config_path, services) = services(root.path(), false)?;
        let mut prepared = Box::pin(prepare_application_run(
            ApplicationRunRequest::non_interactive(
                &config_path,
                root.path(),
                "scope validity",
                "scope-seed",
            ),
            &services,
        ))
        .await?;
        let path = prepared.session_log_path().to_path_buf();
        let identity = prepared.session_id().to_owned();
        let task = TaskId::new("scope-task")?;
        let parent = prepared.execution.parent_session_ref.clone();
        seed_task(&mut prepared.execution.session, parent, &task)?;
        let (trusted_check, policy) = check(
            &mut prepared.execution.session,
            &task,
            CheckCommand {
                command: "sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "test \"$(cat src/input)\" = valid".to_owned(),
                ],
                cwd: None,
            },
            VerificationAutoRunPolicy::Never,
        )?;
        let recorded = sigil_kernel::verification::run_verification_check(
            &mut prepared.execution.session,
            services
                .authority_composition()
                .expect("authority")
                .command_execution
                .as_ref(),
            VerificationCheckRunRequest {
                workspace_root: root.path().to_path_buf(),
                scope: EvidenceScope::Task(task.as_str().to_owned()),
                trusted_check,
                policy_hash: Some(policy.stable_hash()?),
                policy,
                workspace_trust: WorkspaceTrust::Unknown,
                workspace_trust_snapshot_id: "user-config".to_owned(),
                workspace_trust_approval_event_id: None,
                workspace_trust_sandbox_decision_id: None,
            },
        )
        .await?;
        assert_eq!(recorded.receipt.check_status, ReceiptStatus::Succeeded);
        let original_receipt = recorded.receipt.receipt.receipt_id.clone();
        prepared
            .execution
            .session
            .append_control(ControlEntry::VerificationRecorded(recorded))?;
        drop(prepared);
        // External edits do not forge mutation or verification events; readiness must resnapshot.
        std::fs::write(
            root.path().join(if related {
                "src/input"
            } else {
                "outside-scope.txt"
            }),
            "changed\n",
        )?;
        let continued = Box::pin(prepare_application_task_continuation(
            ApplicationTaskContinuationRequest {
                config_path,
                launch_cwd: root.path().to_path_buf(),
                session_path: path.clone(),
                session_attachment: None,
                expected_session_scope_id: identity,
                run_id: "scope-run".to_owned(),
                task_id: task.clone(),
                guidance: None,
                interaction: ApplicationRunInteraction::NonInteractive,
                permission_mode: None,
            },
            &services,
        ))
        .await?;
        let (execution, control) = continued.into_parts();
        let output = Box::pin(execution.execute(
            &mut RecordingApplicationRunEvents::default(),
            &mut AutoApproveHandler,
        ))
        .await?;
        drop(control);
        let session = Session::load_from_store(
            "application-task-test",
            "gpt-test",
            JsonlSessionStore::new(path)?,
        )?;
        let projection = session.verification_state_projection();
        let readiness = projection
            .latest_readiness(&EvidenceScope::Task(task.as_str().to_owned()))
            .expect("Task readiness");
        assert_eq!(
            output.task_status,
            if related {
                TaskRunStatus::Paused
            } else {
                TaskRunStatus::Completed
            }
        );
        assert_eq!(
            readiness.evaluation.verification_verdict == VerificationVerdict::Passed,
            !related
        );
        let receipts = session
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::VerificationRecorded(record)) => {
                    Some(&record.receipt.receipt.receipt_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            receipts,
            vec![&original_receipt],
            "readiness must reuse or invalidate, never silently rerun"
        );
    }
    Ok(())
}
