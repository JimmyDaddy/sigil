//! Real Git hooks and managed tools qualify recovery of the five-batch delivery incident.
use super::*;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

struct FiveBatchBuilder(Arc<AtomicUsize>);
struct FiveBatchProvider(Arc<AtomicUsize>);

#[async_trait]
impl TaskRoleProviderBuilder for FiveBatchBuilder {
    async fn build(&self, _config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        if role == AgentRole::Executor {
            Ok(Box::new(FiveBatchProvider(Arc::clone(&self.0))))
        } else {
            Ok(Box::new(ApplicationTaskRoleProvider { role }))
        }
    }
}

#[async_trait]
impl Provider for FiveBatchProvider {
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
        let stage = self.0.fetch_add(1, Ordering::SeqCst);
        let (name, mut args) = match stage {
            0 => (
                "exec_command",
                serde_json::json!({"command":"git commit -m 'batch 3'"}),
            ),
            1 => (
                "write_file",
                serde_json::json!({"path":"crates/delivery/src/tests.rs", "content":"#[test]\nfn increment_is_correct() { assert_eq!(super::increment(4), 5); }\n"}),
            ),
            2 => (
                "exec_command",
                serde_json::json!({"command": if cfg!(windows) {
                    "rustc --edition 2021 --test crates/delivery/src/lib.rs -o .git/delivery-tests.exe; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; & ./.git/delivery-tests.exe --exact tests::increment_is_correct; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; git add crates/delivery/src/tests.rs"
                } else {
                    "rustc --edition 2021 --test crates/delivery/src/lib.rs -o .git/delivery-tests && .git/delivery-tests --exact tests::increment_is_correct && git add crates/delivery/src/tests.rs"
                }}),
            ),
            3 => (
                "exec_command",
                serde_json::json!({"command":"git commit -m 'batch 3'"}),
            ),
            4 => (
                "write_file",
                serde_json::json!({"path":"batch-four.md", "content":"Targeted package test passed.\n"}),
            ),
            5 => (
                "exec_command",
                serde_json::json!({"command": if cfg!(windows) {
                    "git add batch-four.md; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; git commit -m 'batch 4'"
                } else {
                    "git add batch-four.md && git commit -m 'batch 4'"
                }}),
            ),
            6 => (
                "write_file",
                serde_json::json!({"path":"batch-five.md", "content":"All five batches delivered.\n"}),
            ),
            7 => (
                "exec_command",
                serde_json::json!({"command": if cfg!(windows) {
                    "git add batch-five.md; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }; git commit -m 'batch 5'"
                } else {
                    "git add batch-five.md && git commit -m 'batch 5'"
                }}),
            ),
            8 => {
                anyhow::ensure!(
                    request.messages.iter().any(|message| message
                        .content
                        .as_deref()
                        .is_some_and(|text| text.contains("batch 5"))),
                    "final answer requires actual commit evidence"
                );
                return Ok(Box::pin(stream::iter(scripted_task_completion_chunks(
                    &request,
                    "Five batches committed; package test passed.",
                    "five-batch-completion",
                ))));
            }
            _ => anyhow::bail!("unexpected provider stage {stage}"),
        };
        if name == "exec_command" {
            args["yield_time_ms"] = serde_json::json!(60_000);
        }
        let id = format!("five-batch-{stage}");
        let args_json = args.to_string();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: id.clone(),
                name: name.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: id.clone(),
                delta: args_json.clone(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id,
                name: name.to_owned(),
                args_json,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Provider construction reads the process CA environment.
async fn direct_task_resumes_real_git_hook_failure_and_commits_five_batches_once() -> Result<()> {
    let _environment_guard = crate::test_env::lock();
    let root = tempfile::tempdir()?;
    let workspace = root.path();
    git(workspace, &["init", "-q"])?;
    git(
        workspace,
        &["config", "user.name", "Sigil isolated fixture"],
    )?;
    git(
        workspace,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(workspace, &["config", "commit.gpgsign", "false"])?;
    std::fs::create_dir_all(workspace.join("scripts"))?;
    std::fs::write(
        workspace.join("scripts/check-staged-coverage.py"),
        include_str!("../../../../scripts/check-staged-coverage.py"),
    )?;
    std::fs::write(
        workspace.join(".git/hooks/pre-commit"),
        "#!/bin/sh\npython3 scripts/check-staged-coverage.py\nresult=$?\nprintf '%s\\n' \"$result\" >> .git/coverage-hook-results\nexit \"$result\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            workspace.join(".git/hooks/pre-commit"),
            std::fs::Permissions::from_mode(0o755),
        )?;
    }
    std::fs::write(
        workspace.join(".gitignore"),
        "sigil.toml\n.sigil/\nstate/\ncache/\n",
    )?;
    git(
        workspace,
        &["add", "scripts/check-staged-coverage.py", ".gitignore"],
    )?;
    git(workspace, &["commit", "-qm", "batch 1"])?;
    std::fs::write(
        workspace.join("batch-two.md"),
        "Second batch already delivered.\n",
    )?;
    git(workspace, &["add", "batch-two.md"])?;
    git(workspace, &["commit", "-qm", "batch 2"])?;
    let original = git(workspace, &["rev-list", "--reverse", "HEAD"])?;
    std::fs::create_dir_all(workspace.join("crates/delivery/src"))?;
    std::fs::write(
        workspace.join("crates/delivery/src/lib.rs"),
        "pub fn increment(value: u32) -> u32 {\n    value + 1\n}\n#[cfg(test)]\nmod tests;\n",
    )?;
    git(workspace, &["add", "crates/delivery/src/lib.rs"])?;

    let config_path = workspace.join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let mut config = RootConfig::load(&config_path)?;
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::TaskOrchestration);
    config.permission.mode = sigil_kernel::PermissionMode::DangerFullAccess;
    config.agent.max_turns = Some(1);
    std::fs::write(&config_path, config.persisted_toml()?)?;
    let stages = Arc::new(AtomicUsize::new(0));
    let services = crate::r71_authority_composition::attach_boot_authority_to_services(
        ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter)),
        &config_path,
        workspace,
    )?
    .with_task_role_provider_builder(Arc::new(FiveBatchBuilder(Arc::clone(&stages))));
    let mut prepared = Box::pin(prepare_application_run(
        ApplicationRunRequest::non_interactive(
            &config_path,
            workspace,
            "Finish five batches",
            "five-batch-seed",
        ),
        &services,
    ))
    .await?;
    let session_path = prepared.session_log_path().to_path_buf();
    let scope = prepared.session_id().to_owned();
    let task_id = TaskId::new("five-batch-task")?;
    let objective =
        "Finish five batches, preserving the first two commits and fixing missing package tests";
    let parent_session_ref = prepared.execution.parent_session_ref.clone();
    let admission =
        crate::direct_plan_fixture::append(&mut prepared.execution.session, &task_id, objective)?;
    prepared.execution.session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: Some("delivery ready to resume".to_owned()),
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(admission),
    ])?;
    drop(prepared);
    for attempt in 0..2 {
        let continued = Box::pin(prepare_application_task_continuation(
            ApplicationTaskContinuationRequest {
                config_path: config_path.clone(),
                launch_cwd: workspace.to_path_buf(),
                session_path: session_path.clone(),
                session_attachment: None,
                expected_session_scope_id: scope.clone(),
                run_id: format!("five-batch-run-{attempt}"),
                task_id: task_id.clone(),
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
        if attempt == 0 {
            assert_eq!(output.task_status, TaskRunStatus::Paused);
            assert_eq!(
                git(workspace, &["rev-list", "--reverse", "HEAD"])?,
                original
            );
            assert_eq!(stages.load(Ordering::SeqCst), 1);
            config.agent.max_turns = Some(16);
            std::fs::write(&config_path, config.persisted_toml()?)?;
        } else {
            assert_eq!(output.task_status, TaskRunStatus::Completed);
        }
    }
    assert_eq!(
        stages.load(Ordering::SeqCst),
        9,
        "eight business-tool turns and one final answer finish all five batches"
    );
    let session = Session::load_from_store(
        "application-task-test",
        "gpt-test",
        JsonlSessionStore::new(session_path)?,
    )?;
    let tool_diagnostics = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) => Some((
                result.call_id.as_str(),
                result.facts.status.as_str(),
                result.facts.exit_code,
                result.initial_model_view.preview.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let history = git(workspace, &["log", "--reverse", "--format=%s"])?;
    assert_eq!(
        history.lines().collect::<Vec<_>>(),
        ["batch 1", "batch 2", "batch 3", "batch 4", "batch 5"],
        "tool results: {tool_diagnostics:?}"
    );
    assert!(git(workspace, &["rev-list", "--reverse", "HEAD"])?.starts_with(&original));
    assert!(git(workspace, &["diff", "--cached", "--name-only"])?.is_empty());
    assert!(
        !session
            .entries()
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_))))
    );
    assert!(workspace.join(".git/hooks/pre-commit").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join(".git/coverage-hook-results"))?,
        "0\n0\n1\n0\n0\n0\n",
        "the unchanged coverage gate must reject batch three once and accept only its repair"
    );
    Ok(())
}
