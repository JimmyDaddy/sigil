use std::{
    collections::BTreeSet,
    env, fs,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, ConversationTurnRef, DisclosurePresentationError, DisclosurePresentationReceipt,
    EgressDisclosurePresenter, JsonlSessionStore, PreEgressDisclosure, ReceiptStatus, Session,
    SessionRef, TaskAdmissionReason, TaskAdmissionTrigger, TaskFinalAnswerCommittedEntry,
    TaskHandoffDecision, TaskHandoffId, TaskHandoffRequestedEntry, TaskHandoffResolvedEntry,
    TaskId, TaskParticipantAttemptId, TaskRoutingPolicy, TaskRunEntry, TaskRunStatus,
    ToolExecutionEntry, ToolExecutionStatus, ToolResultMeta, VerificationVerdict,
    changeset_only_child_contract_prompt, continue_without_task_planning_tool_spec,
    conversation_route_routing_contract_material,
    direct_conversation_continuation_prompt_contract_material, request_task_planning_tool_spec,
    runtime_context_v1_system_prompt_contract_material,
    task_participant_finalization_prompt_contract_material,
    task_participant_system_prompt_contract_material, write_file_with_mutation,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::{
    agent_supervisor::task_discovery_system_prompt,
    application_run::ApplicationRunServices,
    model_eval::{
        ModelEvalCampaignRequest, ModelEvalCostConfidence, ModelEvalOrchestrationRouteContractV1,
        ModelEvalRouteContractBuildRequest, ModelEvalRunExecutionStatus,
        build_model_eval_orchestration_route_contract, load_model_eval_fixture,
        materialize_model_eval_fixture, materialized_model_eval_fixture_file_matches_source,
        model_eval_reservation_microusd, orchestration_eval_observation, run_model_eval_campaign,
        verify_model_eval_run, write_isolated_model_eval_config,
    },
};

struct RejectingPresenter;

#[async_trait::async_trait]
impl EgressDisclosurePresenter for RejectingPresenter {
    async fn present(
        &self,
        _disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        Err(DisclosurePresentationError::SinkClosed)
    }
}

fn fixture_root(id: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dev/evals/model-fixtures")
        .join(id)
}

fn orchestration_fixture_roots() -> Vec<std::path::PathBuf> {
    let root = fixture_root("orchestration-v1");
    let mut fixtures = Vec::new();
    for class in ["negative", "positive"] {
        for entry in fs::read_dir(root.join(class)).expect("read orchestration corpus") {
            let path = entry.expect("fixture entry").path();
            if path.join("fixture.toml").is_file() {
                fixtures.push(path);
            }
        }
    }
    fixtures.sort();
    fixtures
}

fn orchestration_route_contract() -> ModelEvalOrchestrationRouteContractV1 {
    let digest = format!("sha256:{}", "1".repeat(64));
    ModelEvalOrchestrationRouteContractV1 {
        schema_version: 1,
        provider_kind: "openai_compat".to_owned(),
        endpoint_family: "openai-compatible-chat".to_owned(),
        canonical_model_version: "test-v1@fp-test".to_owned(),
        routing_prompt_digest: digest.clone(),
        planner_prompt_digest: digest.clone(),
        system_prompt_digest: digest.clone(),
        tool_profile_contract_digest: digest,
        sigil_commit: "test-commit".to_owned(),
        sigil_build: "test-build".to_owned(),
    }
}

#[test]
fn orchestration_observation_uses_typed_durable_facts() {
    let mut session = Session::new("provider", "model");
    let handoff_id = TaskHandoffId::new("handoff-1").expect("handoff id");
    let task_id = TaskId::new("task-1").expect("task id");
    let source_turn = ConversationTurnRef::new(session.session_scope_id(), "message-1", "run-1")
        .expect("source turn");
    let request = TaskHandoffRequestedEntry {
        handoff_id: handoff_id.clone(),
        source_turn,
        trigger: TaskAdmissionTrigger::ModelRequested,
        reason_codes: vec![TaskAdmissionReason::CrossLayer],
        recovery_objective: None,
        policy_snapshot_hash: "sha256:policy".to_owned(),
        requested_at_ms: 1,
    };
    session
        .append_control(ControlEntry::TaskHandoffRequested(request.clone()))
        .expect("append handoff request");
    session
        .append_control(ControlEntry::TaskHandoffRequested(request))
        .expect("append duplicate handoff request");
    session
        .append_control(ControlEntry::TaskHandoffResolved(
            TaskHandoffResolvedEntry {
                handoff_id,
                decision: TaskHandoffDecision::Accepted,
                task_id: Some(task_id.clone()),
                decided_at_ms: 2,
            },
        ))
        .expect("append handoff resolution");
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("parent ref"),
            objective: "Implement the cross-layer change".to_owned(),
            status: TaskRunStatus::Started,
            reason: None,
        }))
        .expect("append task start");
    let final_answer = TaskFinalAnswerCommittedEntry {
        task_id,
        plan_version: 1,
        synthesis_attempt_id: TaskParticipantAttemptId::new("synthesis-1")
            .expect("synthesis attempt"),
        message_id: "final-1".to_owned(),
        content_hash: "sha256:final".to_owned(),
    };
    session
        .append_control(ControlEntry::TaskFinalAnswerCommitted(final_answer.clone()))
        .expect("append final answer");
    session
        .append_control(ControlEntry::TaskFinalAnswerCommitted(final_answer))
        .expect("append duplicate final answer");
    session
        .append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "wait-1".to_owned(),
            tool_name: crate::agent_tools::WAIT_AGENT_TOOL_NAME.to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        })))
        .expect("append polling tool call");

    let observation = orchestration_eval_observation(&session);

    assert!(observation.automatic_task_created);
    assert_eq!(observation.duplicate_handoffs, 1);
    assert_eq!(observation.duplicate_parent_child_finals, 1);
    assert_eq!(observation.model_polling_turns, 1);
    assert_eq!(observation.duplicate_spawns, 0);
    assert_eq!(observation.duplicate_continuations, 0);
    assert_eq!(observation.duplicate_merges, 0);
}

#[test]
fn route_contract_is_derived_from_the_frozen_production_surface() {
    if !enter_isolated_environment_test(
        "model_eval_tests::route_contract_is_derived_from_the_frozen_production_surface",
        "SIGIL_TEST_ROUTE_CONTRACT_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    let _base_url = EnvironmentGuard::set("SIGIL_BASE_URL", "https://api.deepseek.com");
    let _beta_url = EnvironmentGuard::set("SIGIL_BETA_BASE_URL", "https://api.deepseek.com/beta");
    let temp = tempdir().expect("temp dir");
    let config_path = temp.path().join("source.toml");
    fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-eval"
model = "deepseek-v4-flash"

[permission]
mode = "auto-edit"

[task]
enabled = true
multi_agent_mode = "proactive"

[connections.deepseek-eval]
label = "DeepSeek eval"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[connections.deepseek-eval.options]
beta_base_url = "https://api.deepseek.com/beta"
anthropic_base_url = "https://api.deepseek.com/anthropic"
"#,
    )
    .expect("write config");
    let request = ModelEvalRouteContractBuildRequest {
        config_path,
        fixture_roots: orchestration_fixture_roots(),
    };

    let first =
        build_model_eval_orchestration_route_contract(&request).expect("derive route contract");
    let second =
        build_model_eval_orchestration_route_contract(&request).expect("derive route contract");

    assert_eq!(first, second);
    assert_eq!(first.provider_kind, "deepseek");
    assert_eq!(first.endpoint_family, "openai_chat_completions");
    assert!(
        first
            .canonical_model_version
            .starts_with("DeepSeek-V4-Flash@fp_")
    );
    for digest in [
        &first.routing_prompt_digest,
        &first.planner_prompt_digest,
        &first.system_prompt_digest,
        &first.tool_profile_contract_digest,
    ] {
        assert_eq!(digest.len(), 71);
        assert!(digest.starts_with("sha256:"));
    }
    let mut expected_routing_material = b"sigil-orchestration-routing-prompt-v1\0".to_vec();
    expected_routing_material.extend(
        serde_json::to_vec(&serde_json::json!({
            "system_prompt": conversation_route_routing_contract_material(),
            "direct_conversation_continuation": direct_conversation_continuation_prompt_contract_material(),
            "tools": [
                request_task_planning_tool_spec(),
                continue_without_task_planning_tool_spec(),
            ],
        }))
        .expect("serialize routing contract material"),
    );
    assert_eq!(
        first.routing_prompt_digest,
        format!("sha256:{:x}", Sha256::digest(expected_routing_material))
    );
    let mut expected_system_material = b"sigil-orchestration-system-prompt-v1\0".to_vec();
    expected_system_material.extend(
        serde_json::to_vec(&serde_json::json!({
            "runtime_context": runtime_context_v1_system_prompt_contract_material(),
            "changeset_only_child": changeset_only_child_contract_prompt(),
            "planner_discovery": task_discovery_system_prompt(),
            "task_participant": task_participant_system_prompt_contract_material(),
            "task_participant_finalization": task_participant_finalization_prompt_contract_material(),
        }))
        .expect("serialize system prompt contract material"),
    );
    assert_eq!(
        first.system_prompt_digest,
        format!("sha256:{:x}", Sha256::digest(expected_system_material))
    );
    assert!(task_discovery_system_prompt().contains("never add a leading slash"));
    assert!(first.sigil_build.ends_with(&first.sigil_commit));
}

#[test]
fn committed_model_eval_fixtures_load_and_materialize() {
    for id in [
        "small-doc-edit",
        "small-code-edit",
        "stale-after-write",
        "workspace-trust",
        "sandbox-denial",
    ] {
        let fixture = load_model_eval_fixture(fixture_root(id)).expect("fixture should load");
        assert_eq!(fixture.manifest.id, id);
        let temp = tempdir().expect("temp dir");
        let destination = temp.path().join("workspace");
        let materialized =
            materialize_model_eval_fixture(&fixture, &destination).expect("materialize fixture");
        assert_eq!(materialized.fixture_id, id);
        assert!(materialized.tree_digest.starts_with("sha256:"));
        assert!(destination.join("Cargo.toml").is_file());
        let cargo_manifest =
            fs::read_to_string(destination.join("Cargo.toml")).expect("read Cargo manifest");
        let cargo_manifest: toml::Value =
            toml::from_str(&cargo_manifest).expect("parse Cargo manifest");
        assert!(
            cargo_manifest.get("workspace").is_some(),
            "fixture {id} must remain independent from parent Cargo workspaces"
        );
        assert!(!materialized.tool_scope.allows("bash"));
        assert!(!materialized.tool_scope.allows("websearch"));
        assert!(materialized.orchestration.is_none());
    }
}

#[test]
fn committed_orchestration_corpus_has_frozen_route_classes_and_valid_hashes() {
    let corpus_root = fixture_root("orchestration-v1");
    let mut fixture_paths = Vec::new();
    for class in ["negative", "positive"] {
        for entry in fs::read_dir(corpus_root.join(class)).expect("read orchestration class") {
            let entry = entry.expect("orchestration fixture entry");
            if entry.file_type().expect("fixture entry type").is_dir() {
                fixture_paths.push(entry.path());
            }
        }
    }
    fixture_paths.sort();

    let mut ids = BTreeSet::new();
    let mut chat = 0;
    let mut plan_review = 0;
    let mut direct_task = 0;
    for path in fixture_paths {
        let fixture = load_model_eval_fixture(&path).expect("load orchestration fixture");
        assert!(ids.insert(fixture.manifest.id.clone()));
        if fixture.manifest.id.starts_with("orch-pos-cross-layer-") {
            assert!(
                fixture
                    .prompt
                    .contains("replace the formatter's existing `record:` output")
            );
            assert!(
                fixture
                    .prompt
                    .contains("must be exactly `[mixed-42]`, not `[record:mixed-42]`")
            );
            assert!(
                fixture
                    .manifest
                    .checks
                    .iter()
                    .any(|check| { check.command == ["cargo", "test", "--quiet"] })
            );
            assert!(fixture.manifest.assertions.iter().any(|assertion| {
                matches!(
                    &assertion.assertion,
                    crate::model_eval::ModelEvalFixtureAssertionKind::FileUnchanged { path }
                        if path == Path::new("tests/acceptance.rs")
                )
            }));
        }
        let orchestration = fixture
            .manifest
            .orchestration
            .expect("orchestration metadata");
        assert_eq!(orchestration.corpus_version, "rfc-0063-orchestration-v1");
        match orchestration.case_class {
            sigil_kernel::OrchestrationEvalCaseClass::Chat => chat += 1,
            sigil_kernel::OrchestrationEvalCaseClass::PlanReview => plan_review += 1,
            sigil_kernel::OrchestrationEvalCaseClass::DirectTask => direct_task += 1,
        }
    }

    assert_eq!(chat, 20);
    assert_eq!(plan_review, 15);
    assert_eq!(direct_task, 15);
    assert_eq!(ids.len(), 50);
}

#[test]
fn cross_layer_acceptance_is_semantic_and_keeps_the_oracle_immutable() {
    let fixture = load_model_eval_fixture(fixture_root(
        "orchestration-v1/positive/orch-pos-cross-layer-01",
    ))
    .expect("load cross-layer fixture");
    let temp = tempdir().expect("temp dir");
    let workspace = temp.path().join("workspace");
    let materialized =
        materialize_model_eval_fixture(&fixture, &workspace).expect("materialize fixture");
    assert!(
        materialized_model_eval_fixture_file_matches_source(
            &materialized,
            Path::new("tests/acceptance.rs")
        )
        .expect("compare acceptance oracle")
    );
    fs::write(
        workspace.join("src/parser.rs"),
        "pub fn parse(input: &str) -> String {\n    input.trim().to_ascii_lowercase()\n}\n",
    )
    .expect("write parser implementation");

    for formatter in [
        "pub fn render(value: &str) -> String {\n    format!(\"[{value}]\")\n}\n",
        "pub fn render(value: &str) -> String {\n    format!(\"[{}]\", value)\n}\n",
    ] {
        fs::write(workspace.join("src/formatter.rs"), formatter)
            .expect("write formatter implementation");
        let status = Command::new("cargo")
            .args(["test", "--quiet"])
            .current_dir(&workspace)
            .env("CARGO_TARGET_DIR", temp.path().join("cargo-target"))
            .env("CARGO_INCREMENTAL", "0")
            .status()
            .expect("run semantic acceptance test");
        assert!(status.success(), "semantic formatter variant must pass");
        assert_eq!(
            fs::read(workspace.join("tests/acceptance.rs"))
                .expect("read materialized acceptance oracle"),
            fs::read(fixture.source_root.join("files/tests/acceptance.rs"))
                .expect("read committed acceptance oracle")
        );
    }
    fs::write(
        workspace.join("tests/acceptance.rs"),
        "#[test]\nfn weakened_oracle() {}\n",
    )
    .expect("tamper acceptance oracle");
    assert!(
        !materialized_model_eval_fixture_file_matches_source(
            &materialized,
            Path::new("tests/acceptance.rs")
        )
        .expect("compare tampered acceptance oracle")
    );
    assert_eq!(
        materialized
            .fixture_file_digests
            .get(Path::new("tests/acceptance.rs")),
        fixture
            .manifest
            .files
            .iter()
            .find(|file| file.path == Path::new("tests/acceptance.rs"))
            .map(|file| &file.sha256)
    );
}

#[test]
fn model_eval_materialization_publishes_synced_files_portably() {
    let fixture = load_model_eval_fixture(fixture_root("small-code-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let workspace = temp.path().join("workspace");

    let materialized =
        materialize_model_eval_fixture(&fixture, &workspace).expect("materialize fixture");

    assert_eq!(
        materialized.workspace_root,
        fs::canonicalize(temp.path())
            .expect("canonical temp dir")
            .join("workspace")
    );
    assert_eq!(
        fs::read(workspace.join("src/lib.rs")).expect("read published source"),
        fs::read(fixture_root("small-code-edit").join("files/src/lib.rs"))
            .expect("read fixture source")
    );
}

#[test]
fn model_eval_fixture_is_a_standalone_cargo_workspace_when_nested_in_the_repository() {
    let fixture = load_model_eval_fixture(fixture_root("small-code-edit")).expect("load fixture");
    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let target_root = repository_root.join("target");
    fs::create_dir_all(&target_root).expect("create target root");
    let temp = tempfile::tempdir_in(&target_root).expect("nested temp dir");
    let workspace = temp.path().join("workspace");
    materialize_model_eval_fixture(&fixture, &workspace).expect("materialize fixture");

    let output = std::process::Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--offline",
        ])
        .current_dir(&workspace)
        .output()
        .expect("run cargo metadata");

    assert!(
        output.status.success(),
        "nested fixture must not inherit the repository workspace\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn model_eval_fixture_rejects_digest_drift() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    fs::write(
        temp.path().join("files/src/lib.rs"),
        "pub fn value() -> u32 { 9 }\n",
    )
    .expect("drift source");

    let error = load_model_eval_fixture(temp.path()).expect_err("digest drift must fail");
    assert!(error.to_string().contains("file sha256 mismatch"));
}

#[test]
fn model_eval_fixture_rejects_unknown_fields_and_tools() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let manifest_path = temp.path().join("fixture.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    fs::write(
        &manifest_path,
        manifest.replace(
            "allowed_tools = [\"read_file\", \"edit_file\"]",
            "allowed_tools = [\"read_file\", \"bash\"]\nunknown = true",
        ),
    )
    .expect("write manifest");

    let error = load_model_eval_fixture(temp.path()).expect_err("unknown field must fail");
    assert!(error.to_string().contains("failed to parse"));
}

#[cfg(unix)]
#[test]
fn model_eval_fixture_rejects_symlinked_sources() {
    use std::os::unix::fs::symlink;

    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let file = temp.path().join("files/src/lib.rs");
    fs::remove_file(&file).expect("remove copied source");
    symlink("../../prompt.txt", &file).expect("create symlink");

    let error = load_model_eval_fixture(temp.path()).expect_err("symlink must fail");
    assert!(error.to_string().contains("not a regular file"));
}

#[test]
fn model_eval_materializer_refuses_existing_destination() {
    let fixture = load_model_eval_fixture(fixture_root("small-doc-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let error = materialize_model_eval_fixture(&fixture, temp.path())
        .expect_err("existing destination must fail");
    assert!(error.to_string().contains("already exists"));
}

#[test]
fn isolated_model_eval_config_removes_secrets_and_external_surfaces() {
    let fixture = load_model_eval_fixture(fixture_root("small-code-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize fixture");
    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");

    let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect("write isolated config");
    let rendered = fs::read_to_string(&isolated.config_path).expect("read isolated config");

    assert!(!rendered.contains("inline-secret-must-not-copy"));
    assert!(!rendered.to_ascii_lowercase().contains("api_key"));
    assert!(rendered.contains("enabled = false"));
    assert!(rendered.contains(&materialized.workspace_root.display().to_string()));
    assert!(isolated.session_path.starts_with(&run_root));
    let config = sigil_kernel::RootConfig::load(&isolated.config_path).expect("load config");
    assert!(!config.task.enabled);
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Manual);

    let second_run_root = temp.path().join("run-2");
    fs::create_dir(&second_run_root).expect("second run root");
    let second_materialized =
        materialize_model_eval_fixture(&fixture, second_run_root.join("workspace"))
            .expect("materialize second fixture");
    let second =
        write_isolated_model_eval_config(&source_config, &second_materialized, &second_run_root)
            .expect("write second isolated config");
    assert_eq!(isolated.config_digest, second.config_digest);
    assert_ne!(
        isolated.isolated_config_digest,
        second.isolated_config_digest
    );
}

#[test]
fn orchestration_fixture_classification_is_manifest_owned_and_enables_auto_routing() {
    let source = fixture_root("small-code-edit");
    let fixture_temp = tempdir().expect("fixture temp dir");
    copy_directory(&source, fixture_temp.path());
    let manifest_path = fixture_temp.path().join("fixture.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    manifest.push_str(
        r#"

[orchestration]
case_class = "plan_review"
corpus_version = "rfc-0063-v1"
"#,
    );
    fs::write(&manifest_path, manifest).expect("write orchestration manifest");

    let fixture = load_model_eval_fixture(fixture_temp.path()).expect("load orchestration fixture");
    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize fixture");
    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");

    let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect("write orchestration config");
    let config = sigil_kernel::RootConfig::load(&isolated.config_path).expect("load config");

    assert_eq!(
        materialized
            .orchestration
            .as_ref()
            .expect("orchestration metadata")
            .case_class,
        sigil_kernel::OrchestrationEvalCaseClass::PlanReview
    );
    assert_eq!(
        materialized
            .orchestration
            .as_ref()
            .expect("orchestration metadata")
            .corpus_version,
        "rfc-0063-v1"
    );
    assert!(config.task.enabled);
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Auto);
    assert_eq!(
        config.task.multi_agent_mode,
        sigil_kernel::MultiAgentMode::Proactive
    );
}

#[test]
fn isolated_model_eval_config_requires_noninteractive_write_permission() {
    let fixture = load_model_eval_fixture(fixture_root("small-doc-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize fixture");
    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "manual");

    let error = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect_err("manual config without exact tool grants must fail");
    assert!(error.to_string().contains("controlled workspace edits"));
}

#[test]
fn model_eval_reservation_keeps_non_divisible_budget_admissible() {
    assert_eq!(
        model_eval_reservation_microusd(500_000, 15).expect("reserve fifteen runs"),
        33_333
    );
    assert_eq!(
        model_eval_reservation_microusd(500_000, 15).expect("stable reservation") * 15,
        499_995
    );
    assert!(model_eval_reservation_microusd(14, 15).is_err());
    assert!(model_eval_reservation_microusd(500_000, 0).is_err());
}

#[test]
fn model_eval_campaign_requires_environment_credential_before_output_creation() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_campaign_requires_environment_credential_before_output_creation",
        "SIGIL_TEST_MODEL_EVAL_MISSING_CREDENTIAL_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    let _api_key = EnvironmentGuard::unset("SIGIL_API_KEY");
    let temp = tempdir().expect("temp dir");
    let config_path = temp.path().join("source.toml");
    write_deepseek_source_config(&config_path, "auto-edit");
    let output_dir = temp.path().join("campaign");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let error = runtime
        .block_on(run_model_eval_campaign(
            ModelEvalCampaignRequest {
                config_path,
                fixture_roots: vec![fixture_root("small-code-edit")],
                orchestration_route_contract: None,
                repetitions: 1,
                max_cost_microusd: 500_000,
                campaign_timeout: Duration::from_secs(10),
                output_dir: output_dir.clone(),
            },
            &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
        ))
        .expect_err("source-config credentials must not silently enter isolated evals");

    assert!(
        error
            .to_string()
            .contains("model eval requires SIGIL_API_KEY"),
        "{error:#}"
    );
    assert!(error.to_string().contains("exclude provider credentials"));
    assert!(!output_dir.exists());
}

#[test]
fn model_eval_campaign_uses_production_run_constraints_and_budget() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_campaign_uses_production_run_constraints_and_budget",
        "SIGIL_TEST_MODEL_EVAL_CAMPAIGN_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_deepseek_eval_server(Arc::clone(&requests))
            .await
            .expect("spawn server");
        let _api_key = EnvironmentGuard::set("SIGIL_API_KEY", "model-eval-test-key");
        let _base_url = EnvironmentGuard::set("SIGIL_BASE_URL", &base_url);
        let _beta_url = EnvironmentGuard::set("SIGIL_BETA_BASE_URL", &base_url);
        let _anthropic_url = EnvironmentGuard::set("SIGIL_ANTHROPIC_BASE_URL", &base_url);
        let temp = tempdir().expect("temp dir");
        let config_path = temp.path().join("source.toml");
        write_source_config(&config_path, &base_url, "auto-edit");
        let services = ApplicationRunServices::new(Arc::new(RejectingPresenter));

        let campaign = run_model_eval_campaign(
            ModelEvalCampaignRequest {
                config_path,
                fixture_roots: vec![fixture_root("small-code-edit")],
                orchestration_route_contract: None,
                repetitions: 2,
                max_cost_microusd: 500_000,
                campaign_timeout: Duration::from_secs(10),
                output_dir: temp.path().join("campaign"),
            },
            &services,
        )
        .await
        .expect("run campaign");

        assert_eq!(campaign.planned_runs, 2);
        assert_eq!(campaign.runs.len(), 2);
        assert_eq!(
            campaign.runs[0].status,
            ModelEvalRunExecutionStatus::Completed
        );
        assert_eq!(
            campaign.runs[0].cost_confidence,
            ModelEvalCostConfidence::Reported
        );
        assert_eq!(
            campaign.runs[1].status,
            ModelEvalRunExecutionStatus::Completed
        );
        assert_eq!(
            campaign.charged_microusd,
            campaign.reservation_microusd_per_run * 2
        );
        assert!(campaign.runs[0].session_path.is_file());
        assert!(campaign.output_dir.join("results.jsonl").is_file());
        assert!(campaign.output_dir.join("manifest.json").is_file());
        assert!(campaign.output_dir.join("summary.md").is_file());
        let request = requests
            .lock()
            .expect("requests lock")
            .first()
            .cloned()
            .expect("provider request");
        assert!(request.contains(r#""max_tokens":4096"#));
        assert!(request.contains(r#""name":"read_file""#));
        assert!(request.contains(r#""name":"edit_file""#));
        assert!(!request.contains(r#""name":"request_task_planning""#));
        assert!(!request.contains(r#""name":"bash""#));
        assert!(!request.contains("websearch"));
        assert_eq!(requests.lock().expect("requests lock").len(), 2);
    });
}

#[test]
fn orchestration_campaign_attaches_the_production_task_executor() {
    if !enter_isolated_environment_test(
        "model_eval_tests::orchestration_campaign_attaches_the_production_task_executor",
        "SIGIL_TEST_ORCHESTRATION_EVAL_CAMPAIGN_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let base_url = spawn_direct_routing_eval_server(Arc::clone(&requests))
            .await
            .expect("spawn server");
        let _api_key = EnvironmentGuard::set("SIGIL_API_KEY", "model-eval-test-key");
        let _base_url = EnvironmentGuard::set("SIGIL_BASE_URL", &base_url);
        let _beta_url = EnvironmentGuard::set("SIGIL_BETA_BASE_URL", &base_url);
        let _anthropic_url = EnvironmentGuard::set("SIGIL_ANTHROPIC_BASE_URL", &base_url);
        let fixture_temp = tempdir().expect("fixture temp dir");
        copy_directory(&fixture_root("small-code-edit"), fixture_temp.path());
        let manifest_path = fixture_temp.path().join("fixture.toml");
        let mut manifest = fs::read_to_string(&manifest_path).expect("read manifest");
        manifest.push_str(
            r#"

[orchestration]
case_class = "plan_review"
corpus_version = "rfc-0063-v1"
"#,
        );
        fs::write(&manifest_path, manifest).expect("write orchestration manifest");
        let temp = tempdir().expect("temp dir");
        let config_path = temp.path().join("source.toml");
        write_source_config(&config_path, &base_url, "auto-edit");
        let missing_contract = run_model_eval_campaign(
            ModelEvalCampaignRequest {
                config_path: config_path.clone(),
                fixture_roots: vec![fixture_temp.path().to_path_buf()],
                orchestration_route_contract: None,
                repetitions: 1,
                max_cost_microusd: 500_000,
                campaign_timeout: Duration::from_secs(10),
                output_dir: temp.path().join("missing-contract"),
            },
            &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
        )
        .await
        .expect_err("orchestration route contract is mandatory");
        assert!(
            missing_contract
                .to_string()
                .contains("require an exact route contract")
        );
        assert!(requests.lock().expect("requests lock").is_empty());

        let campaign = run_model_eval_campaign(
            ModelEvalCampaignRequest {
                config_path,
                fixture_roots: vec![fixture_temp.path().to_path_buf()],
                orchestration_route_contract: Some(orchestration_route_contract()),
                repetitions: 1,
                max_cost_microusd: 500_000,
                campaign_timeout: Duration::from_secs(10),
                output_dir: temp.path().join("campaign"),
            },
            &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
        )
        .await
        .expect("run orchestration campaign");

        assert_eq!(campaign.runs.len(), 1);
        assert_eq!(
            campaign.runs[0].status,
            ModelEvalRunExecutionStatus::Completed
        );
        let orchestration_dir = campaign.output_dir.join("orchestration");
        assert!(orchestration_dir.join("results.jsonl").is_file());
        assert!(orchestration_dir.join("manifest.json").is_file());
        assert!(orchestration_dir.join("summary.md").is_file());
        let orchestration_manifest: sigil_kernel::OrchestrationEvalReportManifestV1 =
            serde_json::from_slice(
                &fs::read(orchestration_dir.join("manifest.json"))
                    .expect("read orchestration manifest"),
            )
            .expect("decode orchestration manifest");
        assert_eq!(orchestration_manifest.route_gates.len(), 1);
        assert_eq!(
            orchestration_manifest.route_gates[0].status,
            sigil_kernel::OrchestrationEvalRouteStatus::InsufficientEvidence
        );
        assert_eq!(
            orchestration_manifest.route_gates[0]
                .identity
                .provider_adapter,
            "openai_compat"
        );
        assert_eq!(
            orchestration_manifest.route_gates[0]
                .identity
                .canonical_model_id,
            "deepseek-v4-flash"
        );
        let expected_task = sigil_kernel::TaskConfig {
            routing_policy: TaskRoutingPolicy::Auto,
            multi_agent_mode: sigil_kernel::MultiAgentMode::Proactive,
            ..sigil_kernel::TaskConfig::default()
        };
        assert_eq!(
            orchestration_manifest.route_gates[0]
                .identity
                .task_config_digest,
            crate::orchestration_task_config_digest(&expected_task)
                .expect("digest rollout task policy")
        );
        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 2);
        // Custom eval endpoints are never release-qualified, so the automatic route stays at the
        // ReviewFirst baseline: no direct task decision is exposed.
        assert!(requests[0].contains(r#""name":"request_plan_review""#));
        assert!(requests[0].contains(r#""name":"continue_without_task_planning""#));
        assert!(!requests[0].contains(r#""name":"request_task_planning""#));
        assert!(!requests[1].contains(r#""name":"request_plan_review""#));
    });
}

#[test]
fn model_eval_verification_records_pass_then_durable_stale_mutation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let fixture =
            load_model_eval_fixture(fixture_root("stale-after-write")).expect("load fixture");
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("canonical repository root");
        let ignored_target_root = repository_root.join("target");
        fs::create_dir_all(&ignored_target_root).expect("create target root");
        let temp = tempfile::tempdir_in(&ignored_target_root).expect("ignored temp dir");
        let run_root = temp.path().join("run");
        fs::create_dir(&run_root).expect("run root");
        let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
            .expect("materialize fixture");
        let source_config = temp.path().join("source.toml");
        write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");
        let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
            .expect("write isolated config");
        let store = JsonlSessionStore::new(&isolated.session_path).expect("session store");
        let mut session = Session::load_from_store(&isolated.provider, &isolated.model, store)
            .expect("initialize current session");
        session
            .append_control(ControlEntry::Note {
                kind: "model_eval_test".to_owned(),
                data: serde_json::json!({"phase": "model_completed"}),
            })
            .expect("initialize session");
        let source_path = materialized.workspace_root.join("src/lib.rs");
        let source = fs::read_to_string(&source_path).expect("read fixture source");
        let updated = source.replace("    1\n", "    2\n");
        let recorder = session
            .mutation_event_recorder()
            .expect("durable mutation recorder");
        write_file_with_mutation(
            Some(&recorder),
            &materialized.workspace_root,
            "model-edit",
            "src/lib.rs",
            &source_path,
            updated.as_bytes(),
        )
        .expect("record model mutation");
        drop(session);

        let verification = verify_model_eval_run(
            &materialized,
            &isolated.config_path,
            &isolated.session_path,
            &isolated.provider,
            &isolated.model,
            "run-stale",
        )
        .await
        .expect("verify fixture");

        assert_eq!(verification.verdict, VerificationVerdict::Stale);
        assert!(verification.post_run_mutation_recorded);
        assert_eq!(verification.receipts.len(), 1);
        assert_eq!(
            verification.receipts[0].receipt.check_status,
            ReceiptStatus::Succeeded
        );
        assert!(
            fs::read_to_string(materialized.workspace_root.join("README.md"))
                .expect("read mutated readme")
                .contains("fixture_generation = 2")
        );
        let reloaded = Session::load_from_store(
            &isolated.provider,
            &isolated.model,
            JsonlSessionStore::new(&isolated.session_path).expect("reopen store"),
        )
        .expect("reload session");
        assert!(reloaded.entries().iter().any(|entry| matches!(
            entry,
            sigil_kernel::SessionLogEntry::Control(ControlEntry::VerificationRecorded(_))
        )));
    });
}

#[test]
fn all_committed_model_eval_fixtures_satisfy_structured_acceptance() {
    if !enter_isolated_environment_test(
        "model_eval_tests::all_committed_model_eval_fixtures_satisfy_structured_acceptance",
        "SIGIL_TEST_MODEL_EVAL_FIXTURES_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let cases = [
            (
                "small-doc-edit",
                "edit_file",
                serde_json::json!({
                    "path": "README.md",
                    "old_text": "relaiable",
                    "new_text": "reliable"
                }),
            ),
            (
                "small-code-edit",
                "edit_file",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": "left + right",
                    "new_text": "left * right"
                }),
            ),
            (
                "stale-after-write",
                "edit_file",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": "    1\n",
                    "new_text": "    2\n"
                }),
            ),
            (
                "workspace-trust",
                "edit_file",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": "    4\n",
                    "new_text": "    5\n"
                }),
            ),
            (
                "sandbox-denial",
                "write_file",
                serde_json::json!({
                    "path": "../outside.txt",
                    "content": "denied"
                }),
            ),
        ];

        for (case_id, tool_name, arguments) in cases {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let base_url =
                spawn_scripted_eval_tool_server(Arc::clone(&requests), tool_name, arguments)
                    .await
                    .expect("spawn scripted server");
            let _api_key = EnvironmentGuard::set("SIGIL_API_KEY", "model-eval-test-key");
            let _base_url = EnvironmentGuard::set("SIGIL_BASE_URL", &base_url);
            let _beta_url = EnvironmentGuard::set("SIGIL_BETA_BASE_URL", &base_url);
            let _anthropic_url = EnvironmentGuard::set("SIGIL_ANTHROPIC_BASE_URL", &base_url);
            let temp = tempdir().expect("temp dir");
            let config_path = temp.path().join("source.toml");
            write_source_config(&config_path, &base_url, "auto-edit");
            let campaign = run_model_eval_campaign(
                ModelEvalCampaignRequest {
                    config_path,
                    fixture_roots: vec![fixture_root(case_id)],
                    orchestration_route_contract: None,
                    repetitions: 1,
                    max_cost_microusd: 500_000,
                    campaign_timeout: Duration::from_secs(30),
                    output_dir: temp.path().join("campaign"),
                },
                &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
            )
            .await
            .expect("run fixture campaign");
            let manifest: sigil_kernel::ModelEvalReportManifestV3 = serde_json::from_slice(
                &fs::read(campaign.output_dir.join("manifest.json")).expect("read manifest"),
            )
            .expect("decode manifest");
            let rendered = fs::read_to_string(campaign.output_dir.join("results.jsonl"))
                .expect("read results");
            let record: serde_json::Value =
                serde_json::from_str(rendered.trim()).expect("decode result");
            assert_eq!(manifest.accepted_repetitions, 1, "case {case_id}: {record}");
            assert_eq!(record["acceptance_passed"], true, "case {case_id}");
            assert!(
                record["assertion_results"]
                    .as_array()
                    .is_some_and(|assertions| assertions.iter().all(|item| item["passed"] == true)),
                "case {case_id}: {record}"
            );
            let requests = requests.lock().expect("requests lock");
            assert_eq!(requests.len(), 2, "case {case_id}");
            assert!(!requests[0].contains(r#""name":"bash""#));
            assert!(!requests[0].contains("websearch"));
        }
    });
}

fn copy_directory(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            fs::create_dir_all(&target).expect("copy directory");
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

fn write_source_config(path: &Path, base_url: &str, permission_mode: &str) {
    fs::write(
        path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "openai-compatible-eval"
model = "deepseek-v4-flash"
max_turns = 12

[permission]
mode = "{permission_mode}"

[connections.openai-compatible-eval]
label = "OpenAI-compatible eval"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("write source config");
}

fn write_deepseek_source_config(path: &Path, permission_mode: &str) {
    fs::write(
        path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-eval"
model = "deepseek-v4-flash"
max_turns = 12

[permission]
mode = "{permission_mode}"

[connections.deepseek-eval]
label = "DeepSeek eval"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
        ),
    )
    .expect("write DeepSeek source config");
}

async fn spawn_deepseek_eval_server(requests: Arc<Mutex<Vec<String>>>) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        for _ in 0..2 {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = socket.read(&mut chunk).await.unwrap_or_default();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
                if http_request_is_complete(&bytes) || bytes.len() >= 64 * 1024 {
                    break;
                }
            }
            requests
                .lock()
                .expect("requests lock")
                .push(String::from_utf8_lossy(&bytes).into_owned());
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}],",
                "\"usage\":{\"prompt_tokens\":10000000,\"completion_tokens\":2,",
                "\"prompt_cache_hit_tokens\":0,\"prompt_cache_miss_tokens\":10000000},",
                "\"system_fingerprint\":\"fp-test\"}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    Ok(format!("http://{address}"))
}

async fn spawn_direct_routing_eval_server(
    requests: Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        for index in 0..2 {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = socket.read(&mut chunk).await.unwrap_or_default();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
                if http_request_is_complete(&bytes) || bytes.len() >= 128 * 1024 {
                    break;
                }
            }
            requests
                .lock()
                .expect("requests lock")
                .push(String::from_utf8_lossy(&bytes).into_owned());
            let envelope = if index == 0 {
                serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call-direct-routing",
                                "function": {
                                    "name": "continue_without_task_planning",
                                    "arguments": serde_json::json!({
                                        "reason": "does_not_meet_task_planning_criteria"
                                    }).to_string()
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "prompt_cache_hit_tokens": 0,
                        "prompt_cache_miss_tokens": 10
                    },
                    "system_fingerprint": "fp-test"
                })
            } else {
                serde_json::json!({
                    "choices": [{
                        "delta": {"content": "done"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 20,
                        "completion_tokens": 2,
                        "prompt_cache_hit_tokens": 0,
                        "prompt_cache_miss_tokens": 20
                    },
                    "system_fingerprint": "fp-test"
                })
            };
            let body = format!("data: {envelope}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    Ok(format!("http://{address}"))
}

async fn spawn_scripted_eval_tool_server(
    requests: Arc<Mutex<Vec<String>>>,
    tool_name: &str,
    arguments: serde_json::Value,
) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let tool_name = tool_name.to_owned();
    tokio::spawn(async move {
        for index in 0..2 {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = socket.read(&mut chunk).await.unwrap_or_default();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
                if http_request_is_complete(&bytes) || bytes.len() >= 128 * 1024 {
                    break;
                }
            }
            requests
                .lock()
                .expect("requests lock")
                .push(String::from_utf8_lossy(&bytes).into_owned());
            let envelope = if index == 0 {
                serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call-fixture",
                                "function": {
                                    "name": tool_name,
                                    "arguments": arguments.to_string()
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "prompt_cache_hit_tokens": 0,
                        "prompt_cache_miss_tokens": 10
                    }
                })
            } else {
                serde_json::json!({
                    "choices": [{
                        "delta": {"content": "fixture complete"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 20,
                        "completion_tokens": 2,
                        "prompt_cache_hit_tokens": 0,
                        "prompt_cache_miss_tokens": 20
                    }
                })
            };
            let body = format!("data: {envelope}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    Ok(format!("http://{address}"))
}

fn http_request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    content_length.is_some_and(|length| bytes.len() >= header_end + 4 + length)
}

struct EnvironmentGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvironmentGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = env::var_os(name);
        // SAFETY: runtime tests serialize environment mutation through `test_env::lock`.
        unsafe { env::set_var(name, value) };
        Self { name, previous }
    }

    fn unset(name: &'static str) -> Self {
        let previous = env::var_os(name);
        // SAFETY: runtime tests serialize environment mutation through `test_env::lock`.
        unsafe { env::remove_var(name) };
        Self { name, previous }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => {
                // SAFETY: the same serialized test guard is still held during drop.
                unsafe { env::set_var(self.name, value) };
            }
            None => {
                // SAFETY: the same serialized test guard is still held during drop.
                unsafe { env::remove_var(self.name) };
            }
        }
    }
}

fn enter_isolated_environment_test(test_name: &str, marker: &'static str) -> bool {
    if env::var_os(marker).is_some() {
        return true;
    }

    let output = std::process::Command::new(env::current_exe().expect("current test executable"))
        .args(["--exact", test_name, "--nocapture"])
        .env(marker, "1")
        .output()
        .expect("spawn isolated model eval test process");
    assert!(
        output.status.success(),
        "isolated model eval test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
