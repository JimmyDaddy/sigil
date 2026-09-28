//! Opt-in paired Task experiment: every model dispatch is real; no seeded final or repair.
use super::*;
use crate::model_eval::{
    ModelEvalFixtureAssertionKind, load_model_eval_fixture, materialize_model_eval_fixture,
    materialized_model_eval_fixture_file_matches_source, observe_task_eval_patch,
    observe_task_eval_usage, verify_model_eval_run, write_isolated_model_eval_config,
};
use sigil_kernel::{
    AutoApproveHandler, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, CompletionCriteria, EvidenceScope, ModelMessage,
    RuntimeCompositionConfig, SandboxProfileRequirement, TaskRoutingPolicy, ToolEffect,
    TrustedCheckSpec, VerificationAutoRunPolicy, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationScope, WorkspaceTrustRequirement,
};

#[derive(Default)]
struct RepositoryTaskEvents(Vec<PublicRunEvent>);

struct RepositoryDisclosurePresenter;
#[async_trait::async_trait]
impl sigil_kernel::EgressDisclosurePresenter for RepositoryDisclosurePresenter {
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
impl ApplicationRunEventHandler for RepositoryTaskEvents {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        self.0.push(event);
        Ok(())
    }
}

#[tokio::test]
#[ignore = "authorized DeepSeek fixture run: requires explicit SIGIL_A5_REPOSITORY_* inputs"]
#[allow(clippy::await_holding_lock)] // Provider construction uses the process CA environment.
async fn task_verification_repository_live_without_seeded_final() -> Result<()> {
    let _guard = crate::test_env::lock();
    let input = |name: &str| -> Result<PathBuf> {
        std::env::var_os(name)
            .map(PathBuf::from)
            .with_context(|| format!("missing {name}"))
    };
    let source_config = input("SIGIL_A5_REPOSITORY_CONFIG")?;
    let config_source_sha256 = format!(
        "sha256:{}",
        sigil_kernel::sha256_hex(&std::fs::read(&source_config)?)
    );
    let fixture_path = input("SIGIL_A5_REPOSITORY_FIXTURE")?;
    let output_root = input("SIGIL_A5_REPOSITORY_OUTPUT")?;
    anyhow::ensure!(
        output_root.is_absolute(),
        "experiment output must be absolute"
    );
    std::fs::create_dir(&output_root).context("create-new experiment output")?;
    let fixture = load_model_eval_fixture(&fixture_path)?;
    anyhow::ensure!(
        fixture.followup_prompts.is_empty(),
        "predeclared single-input experiment"
    );
    anyhow::ensure!(
        !fixture.manifest.checks.is_empty(),
        "experiment requires declared checks"
    );
    let mut materialized = materialize_model_eval_fixture(&fixture, output_root.join("workspace"))?;
    // Existing A2 config construction owns isolated storage, selected credentials and budgets.
    // Enabling its Task assembly here does not delegate work: the registry below remains scoped
    // to the same committed fixture tools and the model starts at the original Task objective.
    materialized.agent_delegation = true;
    let mut setup = RootConfig::load_persisted(&source_config)?;
    setup.composition = RuntimeCompositionConfig::new(
        sigil_kernel::RuntimeCompositionProfile::Core,
        [sigil_kernel::OptionalCapability::TaskOrchestration],
    );
    setup.task.enabled = true;
    setup.task.routing_policy = TaskRoutingPolicy::Manual;
    setup.task.executor = Default::default();
    let setup_path = output_root.join("task-source-config.toml");
    std::fs::write(&setup_path, setup.persisted_toml()?)?;
    let isolated =
        write_isolated_model_eval_config(&setup_path, &materialized, &output_root.join("run"))?;
    let mut config = RootConfig::load_persisted(&isolated.config_path)?;
    config.task.multi_agent_mode = sigil_kernel::MultiAgentMode::None;
    config.model_request.max_output_tokens = Some(materialized.max_output_tokens);
    std::fs::write(&isolated.config_path, config.persisted_toml()?)?;
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RepositoryDisclosurePresenter))
            .with_task_role_provider_builder(Arc::new(
                crate::agent_supervisor::task_role_runtime::RuntimeTaskRoleProviderBuilder,
            )),
        &isolated.config_path,
        &materialized.workspace_root,
    )?;
    let run_id = format!("a5-repository-{}", uuid::Uuid::new_v4());
    let mut seed = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &isolated.config_path,
            &materialized.workspace_root,
            fixture.prompt.clone(),
            format!("{run_id}-admission"),
        )
        .with_constraints(ApplicationRunConstraints {
            max_turns: materialized.max_turns as usize,
            max_output_tokens: materialized.max_output_tokens,
            tool_scope: materialized.tool_scope.clone(),
        }),
        &services,
    ))
    .await?;
    let session_path = seed.session_log_path().to_path_buf();
    let session_scope = seed.session_id().to_owned();
    let parent = seed.execution.parent_session_ref.clone();
    let action = crate::ConversationCoordinator::new(true, TaskRoutingPolicy::Manual)
        .admit_explicit_task(
            &mut seed.execution.session,
            ModelMessage::user(fixture.prompt.clone()),
            parent,
            format!("{run_id}-input"),
            current_unix_time_ms(),
        )?;
    let scope_hash = format!(
        "sha256:{}",
        sigil_kernel::sha256_hex(
            format!("a5-repository-scope\n{}", fixture.manifest_digest).as_bytes(),
        )
    );
    let mut source_scope = VerificationScope::all_tracked(scope_hash.clone());
    source_scope.include = materialized
        .fixture_files
        .iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .context("UTF-8 fixture path")
        })
        .collect::<Result<Vec<_>>>()?;
    source_scope.tracked_files_only = false;
    let checks = materialized
        .checks
        .iter()
        .map(|check| {
            let (command, args) = check.command.split_first().context("declared command")?;
            Ok(TrustedCheckSpec {
                check_spec: CheckSpec::new(
                    check.id.clone(),
                    CheckCommand {
                        command: command.clone(),
                        args: args.to_vec(),
                        cwd: None,
                    },
                    ToolEffect::ReadOnly,
                    scope_hash.clone(),
                ),
                source: CheckDiscoverySource::UserExplicitConfig,
                workspace_trust_snapshot_id: fixture.manifest_digest.clone(),
                promoted_by: CheckPromotion::ExplicitUserConfig {
                    config_event_id: fixture.manifest_digest.clone(),
                },
                approval_event_id: None,
                sandbox_decision_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let task_scope = EvidenceScope::Task(action.task_id.as_str().to_owned());
    let policy = VerificationPolicy {
        required_checks: checks
            .iter()
            .map(|check| check.check_spec.clone())
            .collect(),
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: source_scope,
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: materialized
            .checks
            .iter()
            .map(|check| check.timeout_ms)
            .min(),
        auto_run: VerificationAutoRunPolicy::TrustedOnly,
    };
    let mut controls = checks
        .into_iter()
        .map(|check| {
            ControlEntry::CheckSpecRecorded(CheckSpecRecordedEntry::new(
                task_scope.clone(),
                check,
                fixture.manifest_digest.clone(),
            ))
        })
        .collect::<Vec<_>>();
    controls.push(ControlEntry::VerificationPolicyChanged(
        VerificationPolicyChangedEntry::new(task_scope, policy, fixture.manifest_digest.clone())?,
    ));
    seed.execution.session.append_controls(controls)?;
    drop(seed);
    let mut prepared = Box::pin(prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path: isolated.config_path.clone(),
            launch_cwd: materialized.workspace_root.clone(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: session_scope.clone(),
            run_id: run_id.clone(),
            task_id: action.task_id.clone(),
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    ))
    .await?;
    prepared.execution.task_execution.base_registry = prepared
        .execution
        .task_execution
        .base_registry
        .scoped(materialized.tool_scope.clone())
        .into_registry();
    let (execution, control) = prepared.into_parts();
    let started = std::time::Instant::now();
    let mut events = RepositoryTaskEvents::default();
    let mut approvals = AutoApproveHandler;
    let mut running = Box::pin(execution.execute(&mut events, &mut approvals));
    let mut timed_out = false;
    let execution_joined = true;
    let mut cancellation_ticket = None;
    let mut cancellation_request_error = None;
    let result = match tokio::time::timeout(std::time::Duration::from_secs(900), running.as_mut())
        .await
    {
        Ok(result) => Some(result),
        Err(_) => {
            timed_out = true;
            match control.request_cancellation(
                "A5 experiment deadline",
                Some(std::time::Duration::from_secs(5)),
                || {},
            ) {
                Ok(ticket) => cancellation_ticket = Some(ticket),
                Err(error) => {
                    cancellation_request_error = Some(safe_persistence_text(&error.to_string()));
                    cancellation_ticket = error.into_ticket();
                }
            }
            // The deadline requests cancellation; the existing run still owns cleanup.
            // Await its real join instead of dropping a live effect at a reporting timeout.
            Some(running.as_mut().await)
        }
    };
    drop(running);
    let cancellation = if let Some(ticket) = cancellation_ticket {
        Some(
            control
                .finalize_cancellation(ticket, execution_joined, &mut events)
                .await
                .map(|outcome| format!("{outcome:?}"))
                .map_err(|error| safe_persistence_text(&format!("{error:#}"))),
        )
    } else {
        None
    };
    drop(control);
    let (status, execution_error) = match result {
        Some(Ok(output)) => (Some(output.task_status), None),
        Some(Err(error)) => (None, Some(safe_persistence_text(&format!("{error:#}")))),
        None => (
            None,
            Some("execution did not join by cancellation deadline".to_owned()),
        ),
    };
    let records = JsonlSessionStore::read_event_records(&session_path).ok();
    let billing = observe_task_eval_usage(records.as_deref(), &session_scope, &events.0);
    let final_check = if execution_joined {
        verify_model_eval_run(
            &materialized,
            &isolated.config_path,
            &session_path,
            &isolated.provider,
            &isolated.model,
            &format!("{run_id}-independent-check"),
        )
        .await
        .map(|verification| format!("{:?}", verification.verdict))
        .map_err(|error| safe_persistence_text(&format!("{error:#}")))
    } else {
        Err("execution remains unjoined; no independent check was run".to_owned())
    };
    let mut assertions = Vec::new();
    for assertion in &materialized.assertions {
        let ModelEvalFixtureAssertionKind::FileUnchanged { path } = &assertion.assertion else {
            bail!("unplanned assertion kind; do not silently omit acceptance");
        };
        assertions.push(serde_json::json!({"id":assertion.id,
            "passed":materialized_model_eval_fixture_file_matches_source(&materialized, path).unwrap_or(false)}));
    }
    let patch_fixture = materialized.clone();
    let (patch, patch_observation) =
        tokio::task::spawn_blocking(move || observe_task_eval_patch(&patch_fixture))
            .await
            .context("join final patch observation")?;
    std::fs::write(output_root.join("actual.patch"), patch)?;
    let task_receipts = records.as_ref().map(|records| {
        records
            .iter()
            .filter_map(|record| match record.session_log_entry().ok().flatten()? {
                SessionLogEntry::Control(ControlEntry::VerificationRecorded(receipt)) => {
                    Some(serde_json::to_value(receipt).ok()?)
                }
                SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(feedback)) => {
                    Some(serde_json::to_value(feedback).ok()?)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    let report = serde_json::json!({"schema_version":1,"fixture_id":fixture.manifest.id,
        "manifest_digest":fixture.manifest_digest,"task_id":action.task_id,"run_id":run_id,
        "config_source_sha256":config_source_sha256,"requested_provider":isolated.provider,
        "requested_model":isolated.model,"actual_response_model_version":serde_json::Value::Null,
        "patch":patch_observation,
        "session_scope":session_scope,"session_artifact_path":session_path,
        "first_model_dispatch":"real","status":status,"execution_error":execution_error,
        "timed_out":timed_out,"execution_joined":execution_joined,"cancellation":cancellation,
        "cancellation_request_error":cancellation_request_error,
        "independent_check":final_check,"assertions":assertions,"task_receipts":task_receipts,
        "billing":billing,"wall_time_ms":started.elapsed().as_millis(),
        "wall_time_supports_performance_claim":false});
    std::fs::write(
        output_root.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "A5_REPOSITORY_RESULT {}",
        output_root.join("result.json").display()
    );
    Ok(())
}
