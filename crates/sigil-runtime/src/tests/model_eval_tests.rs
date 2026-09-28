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
    SessionRef, TaskAdmissionTrigger, TaskHandoffDecision, TaskHandoffId,
    TaskHandoffRequestedEntry, TaskHandoffResolvedEntry, TaskId, TaskRoutingPolicy, TaskRunEntry,
    TaskRunStatus, ToolExecutionEntry, ToolExecutionStatus, ToolResultMeta, VerificationVerdict,
    changeset_only_child_contract_prompt, runtime_context_v2_contract_material,
    write_file_with_mutation,
};
use tempfile::tempdir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::{
    application_run::ApplicationRunServices,
    model_eval::{
        MODEL_EVAL_ORCHESTRATION_ROUTE_CONTRACT_SCHEMA_VERSION, ModelEvalCampaignRequest,
        ModelEvalCostConfidence, ModelEvalExpectedTerminal, ModelEvalFixtureAssertionKind,
        ModelEvalOrchestrationRouteContractV1, ModelEvalRouteContractBuildRequest,
        ModelEvalRunExecutionStatus, build_model_eval_orchestration_route_contract,
        load_model_eval_fixture, materialize_model_eval_fixture,
        materialized_model_eval_fixture_file_matches_source, model_eval_reservation_microusd,
        orchestration_eval_observation, run_model_eval_campaign, verify_model_eval_run,
        write_isolated_model_eval_config,
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

#[test]
fn r71_9a_incident_golden_preserves_the_authority_failure_chain() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../dev/evals/plan-fixtures/r71-9a-authority-readiness/golden.json");
    let golden: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).expect("read R71.9a incident golden"))
            .expect("parse R71.9a incident golden");
    assert_eq!(golden["schema_version"], 1);
    assert_eq!(golden["fixture_id"], "r71-9a-authority-readiness");
    assert_eq!(golden["event_chain"].as_array().map(Vec::len), Some(5));
    assert_eq!(
        golden["event_chain"][2]["error_class"],
        "resource_precondition_unavailable"
    );
    assert_eq!(
        golden["event_chain"][3]["reason"],
        "session has no durable artifact store"
    );
    assert_eq!(
        golden["regression_contract"]["permission_allow_is_not_resource_readiness"],
        true
    );
    assert_eq!(
        golden["regression_contract"]["explicit_plan_acceptance_is_preserved"],
        true
    );
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
        schema_version: MODEL_EVAL_ORCHESTRATION_ROUTE_CONTRACT_SCHEMA_VERSION,
        provider_kind: "openai_compat".to_owned(),
        endpoint_family: "openai-compatible-chat".to_owned(),
        canonical_model_version: "test-v1@fp-test".to_owned(),
        routing_prompt_digest: digest.clone(),
        direct_task_prompt_digest: digest.clone(),
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
        title: None,
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
            title: None,

            status: TaskRunStatus::Started,
            reason: None,
        }))
        .expect("append task start");
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
        provider_system_fingerprint: "fp-live-route".to_owned(),
    };

    let first =
        build_model_eval_orchestration_route_contract(&request).expect("derive route contract");
    let second =
        build_model_eval_orchestration_route_contract(&request).expect("derive route contract");

    for invalid_fingerprint in [
        "",
        " fp-live-route",
        "fp-live-route ",
        "fp@route",
        "fp\nroute",
    ] {
        let error =
            build_model_eval_orchestration_route_contract(&ModelEvalRouteContractBuildRequest {
                provider_system_fingerprint: invalid_fingerprint.to_owned(),
                ..request.clone()
            })
            .expect_err("invalid provider fingerprint must fail before contract derivation");
        assert!(
            error
                .to_string()
                .contains("provider system fingerprint is invalid")
        );
    }

    assert_eq!(first, second);
    assert_eq!(first.provider_kind, "deepseek");
    assert_eq!(first.endpoint_family, "openai_chat_completions");
    assert!(
        first
            .canonical_model_version
            .eq("DeepSeek-V4-Flash@fp-live-route")
    );
    for digest in [
        &first.routing_prompt_digest,
        &first.direct_task_prompt_digest,
        &first.system_prompt_digest,
        &first.tool_profile_contract_digest,
    ] {
        assert_eq!(digest.len(), 71);
        assert!(digest.starts_with("sha256:"));
    }
    let mut expected_routing_material = b"sigil-orchestration-routing-prompt-v1\0".to_vec();
    expected_routing_material.extend(
        serde_json::to_vec(&serde_json::json!({
            "system_prompt": sigil_kernel::conversation_auto_execution_contract_material(),
            "tools": sigil_kernel::conversation_tool_specs_for_bound_context(
                Vec::new(), sigil_kernel::AutomaticRouteCapability::DirectTask, false, false, false,
            ),
            "continuation_tools": sigil_kernel::route_surface_tool_specs_for_bound_context(
                sigil_kernel::AutomaticRouteCapability::DirectTask, false, true, false,
            ),
            "pending_plan_tools": sigil_kernel::route_surface_tool_specs_for_bound_context(
                sigil_kernel::AutomaticRouteCapability::DirectTask, false, false, true,
            ),
        }))
        .expect("serialize routing contract material"),
    );
    assert_eq!(
        first.routing_prompt_digest,
        format!("sha256:{:x}", Sha256::digest(expected_routing_material))
    );
    let mut expected_system_material = b"sigil-orchestration-system-prompt-v2\0".to_vec();
    expected_system_material.extend(
        serde_json::to_vec(&serde_json::json!({
            "runtime_context": runtime_context_v2_contract_material(),
            "changeset_only_child": changeset_only_child_contract_prompt(),
            "plan_review": sigil_kernel::plan_review_system_prompt_contract_material(),
            "plan_review_parent_context":
                sigil_kernel::plan_review_parent_context_contract_material(),
        }))
        .expect("serialize system prompt contract material"),
    );
    assert_eq!(
        first.system_prompt_digest,
        format!("sha256:{:x}", Sha256::digest(expected_system_material))
    );
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
        assert!(!materialized.tool_scope.allows("exec_command"));
        assert!(!materialized.tool_scope.allows("websearch"));
        assert!(materialized.orchestration.is_none());
    }
}

#[test]
fn limited_read_only_symbol_case_has_search_tools_and_completes() {
    let fixture =
        load_model_eval_fixture(fixture_root("orchestration-v1/negative/orch-neg-symbol-06"))
            .expect("load symbol clarification fixture");
    assert!(
        fixture
            .manifest
            .allowed_tools
            .iter()
            .any(|tool| tool == "read_file")
    );
    assert!(
        fixture
            .manifest
            .allowed_tools
            .iter()
            .any(|tool| tool == "grep")
    );
    assert_eq!(
        fixture.manifest.expected_terminal,
        [ModelEvalExpectedTerminal::Completed]
    );
    assert!(!fixture.manifest.assertions.iter().any(|assertion| {
        matches!(
            assertion.assertion,
            ModelEvalFixtureAssertionKind::UserInputPending
        )
    }));
}

#[test]
fn limited_read_only_question_case_has_discovery_tools_and_completes() {
    let fixture = load_model_eval_fixture(fixture_root(
        "orchestration-v1/negative/orch-neg-question-02",
    ))
    .expect("load read-only question fixture");
    for required_tool in ["read_file", "glob", "grep"] {
        assert!(
            fixture
                .manifest
                .allowed_tools
                .iter()
                .any(|tool| tool == required_tool),
            "question fixture must expose {required_tool}"
        );
    }
    assert_eq!(
        fixture.manifest.expected_terminal,
        [ModelEvalExpectedTerminal::Completed]
    );
    assert!(!fixture.manifest.assertions.iter().any(|assertion| {
        matches!(
            assertion.assertion,
            ModelEvalFixtureAssertionKind::UserInputPending
        )
    }));
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
        if fixture.manifest.id.starts_with("orch-neg-symbol-") {
            assert!(
                fixture
                    .manifest
                    .allowed_tools
                    .iter()
                    .any(|tool| tool == "grep")
            );
        }
        if fixture.manifest.id.starts_with("orch-neg-question-") {
            for required_tool in ["read_file", "glob", "grep"] {
                assert!(
                    fixture
                        .manifest
                        .allowed_tools
                        .iter()
                        .any(|tool| tool == required_tool),
                    "question fixture {} must expose discovery tool {required_tool}",
                    fixture.manifest.id
                );
            }
        }
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
        if matches!(
            orchestration.case_class,
            sigil_kernel::OrchestrationEvalCaseClass::PlanReview
                | sigil_kernel::OrchestrationEvalCaseClass::DirectTask
        ) {
            for required_tool in ["read_file", "glob", "grep"] {
                assert!(
                    fixture
                        .manifest
                        .allowed_tools
                        .iter()
                        .any(|tool| tool == required_tool),
                    "fixture {} must expose the production discovery tool {required_tool}",
                    fixture.manifest.id
                );
            }
        }
        match orchestration.case_class {
            sigil_kernel::OrchestrationEvalCaseClass::Chat => chat += 1,
            sigil_kernel::OrchestrationEvalCaseClass::PlanReview => {
                assert!(
                    !fixture
                        .manifest
                        .allowed_tools
                        .iter()
                        .any(|tool| { matches!(tool.as_str(), "edit_file" | "write_file") })
                );
                plan_review += 1;
            }
            sigil_kernel::OrchestrationEvalCaseClass::DirectTask => {
                for required_tool in ["edit_file", "write_file"] {
                    assert!(
                        fixture
                            .manifest
                            .allowed_tools
                            .iter()
                            .any(|tool| tool == required_tool),
                        "fixture {} must expose the production execution tool {required_tool}",
                        fixture.manifest.id
                    );
                }
                direct_task += 1;
            }
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
fn model_eval_fixture_ignores_unknown_fields_and_defers_tool_availability_to_registry() {
    let source = fixture_root("small-code-edit");
    let temp = tempdir().expect("temp dir");
    copy_directory(&source, temp.path());
    let manifest_path = temp.path().join("fixture.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read manifest");
    fs::write(
        &manifest_path,
        manifest.replace(
            "allowed_tools = [\"read_file\", \"edit_file\"]",
            "allowed_tools = [\"read_file\", \"edit_file\"]\nunknown = true",
        ),
    )
    .expect("write manifest");

    load_model_eval_fixture(temp.path()).expect("unknown fields are ignored");

    fs::write(
        &manifest_path,
        manifest.replace(
            "allowed_tools = [\"read_file\", \"edit_file\"]",
            "allowed_tools = [\"read_file\", \"bash\"]",
        ),
    )
    .expect("write registered tool manifest");
    load_model_eval_fixture(temp.path()).expect("tool availability belongs to the actual registry");
    fs::write(
        &manifest_path,
        manifest.replace("\"edit_file\"", "\"invalid tool\""),
    )
    .expect("write malformed tool identifier");
    let error = load_model_eval_fixture(temp.path()).expect_err("malformed identifier must fail");
    assert!(error.to_string().contains("bounded tool identifier"));
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
fn agent_delegation_fixture_enables_only_manual_model_directed_delegation() {
    let fixture = load_model_eval_fixture(fixture_root("agent-collaboration-v1"))
        .expect("load agent collaboration fixture");
    assert!(fixture.manifest.agent_delegation);

    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize agent collaboration fixture");
    assert!(materialized.tool_scope.allows("spawn_agents"));

    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");
    let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect("write isolated delegation config");
    let config = sigil_kernel::RootConfig::load(&isolated.config_path).expect("load config");

    assert!(config.task.enabled);
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Manual);
    assert_eq!(
        config.task.multi_agent_mode,
        sigil_kernel::MultiAgentMode::Proactive
    );
}

#[test]
fn isolated_model_eval_core_config_preserves_selection_without_dormant_payloads() {
    let fixture = load_model_eval_fixture(fixture_root("small-code-edit")).expect("load fixture");
    let temp = tempdir().expect("temp dir");
    let run_root = temp.path().join("run");
    fs::create_dir(&run_root).expect("run root");
    let materialized = materialize_model_eval_fixture(&fixture, run_root.join("workspace"))
        .expect("materialize fixture");
    let source_config = temp.path().join("source.toml");
    write_source_config(&source_config, "http://127.0.0.1:9", "auto-edit");
    let mut source: toml::Value =
        toml::from_str(&fs::read_to_string(&source_config).expect("source")).expect("toml");
    let source_table = source.as_table_mut().expect("source config table");
    source_table.insert(
        "composition".to_owned(),
        toml::toml! { profile = "core" enhancements = [] }.into(),
    );
    source_table.insert(
        "memory".to_owned(),
        toml::Value::String("unselected module is intentionally malformed".to_owned()),
    );
    fs::write(&source_config, toml::to_string(&source).expect("serialize")).expect("write source");
    let isolated = write_isolated_model_eval_config(&source_config, &materialized, &run_root)
        .expect("isolate core source without activating memory");
    let config = sigil_kernel::RootConfig::load(&isolated.config_path)
        .expect("load")
        .with_effective_composition()
        .expect("compose");
    assert!(config.selected_capabilities().is_empty());
    assert!(!config.task.enabled);
    assert!(!config.memory.enabled);
    assert!(!config.web.enabled);
    assert!(
        !fs::read_to_string(&isolated.config_path)
            .expect("config")
            .contains("intentionally malformed")
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
                release_output_owner: None,
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
                // Current-schema authority boot performs durable admission and workspace
                // registration for each isolated repetition; keep this integration budget
                // independent from host disk/journal latency while still exercising the
                // campaign deadline path in the dedicated timeout tests below.
                campaign_timeout: Duration::from_secs(30),
                output_dir: temp.path().join("campaign"),
                release_output_owner: None,
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
            ModelEvalCostConfidence::Unknown
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
        assert!(!request.contains(r#""name":"start_task""#));
        assert!(!request.contains(r#""name":"exec_command""#));
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
                release_output_owner: None,
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
                release_output_owner: None,
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
        assert_eq!(requests.len(), 1);
        // Evaluation uses the same runtime capability as user routes: an attached executor
        // exposes direct task routing even without a release-qualified endpoint.
        assert!(requests[0].contains(r#""name":"request_plan_review""#));
        assert!(requests[0].contains(r#""name":"start_task""#));
        assert!(requests[0].contains("Writable memory tools are unavailable"));
        assert!(!requests[0].contains("Writable memory is available"));
        assert!(requests[0].contains(r#""name":"request_user_input""#));
    });
}

#[test]
fn model_eval_verification_records_pass_then_durable_stale_mutation() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_verification_records_pass_then_durable_stale_mutation",
        "SIGIL_TEST_MODEL_EVAL_STALE_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
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

        assert_eq!(
            verification.verdict,
            VerificationVerdict::Stale,
            "fixture verification must succeed before its post-run mutation: {verification:#?}"
        );
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
                    release_output_owner: None,
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
            assert!(!requests[0].contains(r#""name":"exec_command""#));
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
        loop {
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
            let envelope = {
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
    let (base_url, _server) =
        spawn_scripted_eval_tool_sequence_server(requests, vec![(tool_name.to_owned(), arguments)])
            .await?;
    Ok(base_url)
}

async fn spawn_scripted_eval_tool_sequence_server(
    requests: Arc<Mutex<Vec<String>>>,
    calls: Vec<(String, serde_json::Value)>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        for index in 0..=calls.len() {
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
            let envelope = if let Some((tool_name, arguments)) = calls.get(index) {
                let call_id = if index == 0 {
                    "call-fixture".to_owned()
                } else {
                    format!("call-fixture-{index}")
                };
                serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": call_id,
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
    Ok((format!("http://{address}"), server))
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
    // The child inherits the parent's environment at spawn time. Acquire the same process-wide
    // lock used by EnvironmentGuard before inspecting the marker or spawning it, otherwise a
    // concurrent model-eval parent can leak transient provider variables into this child.
    let _env_lock = crate::test_env::lock();
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

fn write_python_model_eval_fixture(root: &Path, followup: bool) {
    copy_directory(&fixture_root("small-code-edit"), root);
    let manifest_path = root.join("fixture.toml");
    let mut manifest: crate::model_eval::ModelEvalFixtureManifest =
        toml::from_str(&fs::read_to_string(&manifest_path).expect("manifest")).expect("parse");
    let check_source =
        "from pathlib import Path\nassert 'left * right' in Path('src/lib.rs').read_text()\n";
    fs::write(root.join("files/check.py"), check_source).expect("check source");
    manifest
        .files
        .push(crate::model_eval::ModelEvalFixtureFile {
            path: "check.py".into(),
            source: "files/check.py".into(),
            sha256: format!("sha256:{:x}", Sha256::digest(check_source.as_bytes())),
        });
    manifest.checks[0].command = vec!["python3".into(), "-B".into(), "check.py".into()];
    manifest
        .assertions
        .push(crate::model_eval::ModelEvalFixtureAssertion {
            id: "independent-oracle".into(),
            assertion: ModelEvalFixtureAssertionKind::FileUnchanged {
                path: "check.py".into(),
            },
        });
    if followup {
        let prompt =
            "Second fixed turn: retain the existing implementation and explain its result.";
        fs::write(root.join("followup.txt"), prompt).expect("follow-up");
        manifest
            .followup_prompts
            .push(crate::model_eval::ModelEvalFixturePrompt {
                prompt_file: "followup.txt".into(),
                prompt_sha256: format!("sha256:{:x}", Sha256::digest(prompt.as_bytes())),
            });
    }
    fs::write(manifest_path, toml::to_string(&manifest).expect("encode")).expect("save manifest");
}

#[test]
fn model_eval_fixed_followups_and_direct_python_checks_are_hash_bound() {
    let temp = tempdir().expect("temp");
    write_python_model_eval_fixture(temp.path(), true);
    let fixture = load_model_eval_fixture(temp.path()).expect("Python/multiturn fixture");
    assert_eq!(fixture.followup_prompts.len(), 1);
    assert_eq!(
        fixture.manifest.checks[0].command,
        ["python3", "-B", "check.py"]
    );
    fs::write(temp.path().join("followup.txt"), "modified turn").expect("tamper");
    assert!(
        load_model_eval_fixture(temp.path())
            .expect_err("reject prompt drift")
            .to_string()
            .contains("sha256 mismatch")
    );
}

#[test]
fn model_eval_followups_use_source_budget_instead_of_a_turn_count_gate() {
    let temp = tempdir().expect("temp");
    write_python_model_eval_fixture(temp.path(), true);
    let manifest_path = temp.path().join("fixture.toml");
    let mut manifest: crate::model_eval::ModelEvalFixtureManifest =
        toml::from_str(&fs::read_to_string(&manifest_path).expect("manifest")).expect("parse");
    manifest.followup_prompts = vec![manifest.followup_prompts[0].clone(); 6];
    fs::write(&manifest_path, toml::to_string(&manifest).expect("encode")).expect("save");
    let loaded = load_model_eval_fixture(temp.path()).expect("six fixed follow-up turns");
    assert_eq!(loaded.followup_prompts.len(), 6);

    let prompt = "x".repeat(crate::model_eval::MODEL_EVAL_MAX_PROMPT_BYTES as usize);
    fs::write(temp.path().join(&manifest.prompt_file), &prompt).expect("initial prompt");
    fs::write(temp.path().join("followup.txt"), &prompt).expect("follow-up prompt");
    let digest = format!("sha256:{:x}", Sha256::digest(prompt.as_bytes()));
    manifest.prompt_sha256 = digest.clone();
    let followup = crate::model_eval::ModelEvalFixturePrompt {
        prompt_file: "followup.txt".into(),
        prompt_sha256: digest,
    };
    // With 63 follow-ups the prompts fill the budget and workspace files exceed it.
    // With 64 follow-ups the prompt bytes alone exceed the same budget.
    for count in [63, 64] {
        manifest.followup_prompts = vec![followup.clone(); count];
        fs::write(&manifest_path, toml::to_string(&manifest).expect("encode")).expect("save");
        assert!(
            load_model_eval_fixture(temp.path())
                .expect_err("shared source byte budget")
                .to_string()
                .contains("source exceeds 1048576 bytes"),
            "follow-up count {count}"
        );
    }
}

#[test]
fn model_eval_multiturn_reuses_session_and_preserves_usage_unknown_cost() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_multiturn_reuses_session_and_preserves_usage_unknown_cost",
        "SIGIL_TEST_MODEL_EVAL_MULTITURN_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let base_url = spawn_direct_routing_eval_server(Arc::clone(&requests))
                .await
                .expect("server");
            let temp = tempdir().expect("temp");
            let fixture_path = temp.path().join("fixture");
            fs::create_dir(&fixture_path).expect("fixture directory");
            write_python_model_eval_fixture(&fixture_path, true);
            let config_path = temp.path().join("source.toml");
            write_source_config(&config_path, &base_url, "auto-edit");
            let campaign = run_model_eval_campaign(
                ModelEvalCampaignRequest {
                    config_path,
                    fixture_roots: vec![fixture_path],
                    orchestration_route_contract: None,
                    repetitions: 1,
                    max_cost_microusd: 500_000,
                    campaign_timeout: Duration::from_secs(30),
                    output_dir: temp.path().join("campaign"),
                    release_output_owner: None,
                },
                &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
            )
            .await
            .expect("campaign");
            let run = &campaign.runs[0];
            assert_eq!(run.turns.len(), 2);
            assert_eq!(run.usage.usage_events, 2);
            assert_eq!(run.usage.prompt_tokens, 40);
            assert_eq!(
                run.total_cost_usd(),
                None,
                "custom route has no known price"
            );
            assert_eq!(
                run.verification
                    .as_ref()
                    .expect("real verification")
                    .verdict,
                VerificationVerdict::Failed
            );
            let captured = requests.lock().expect("requests");
            assert_eq!(captured.len(), 2);
            assert!(!captured[0].contains("Second fixed turn"));
            assert!(captured[1].contains("Second fixed turn"));
            assert!(
                captured[1].contains("done"),
                "second turn must include first assistant response"
            );
            let records =
                JsonlSessionStore::read_event_records(&run.session_path).expect("session");
            let users = records
                .iter()
                .filter_map(|record| record.session_log_entry().ok().flatten())
                .filter(|entry| matches!(entry, sigil_kernel::SessionLogEntry::User(_)))
                .count();
            assert_eq!(users, 2);
            let trajectory: serde_json::Value = serde_json::from_str(
                fs::read_to_string(campaign.output_dir.join("trajectory.jsonl"))
                    .expect("trajectory")
                    .trim(),
            )
            .expect("decode trajectory");
            assert_eq!(trajectory["turns"].as_array().map(Vec::len), Some(2));
            for turn in trajectory["turns"].as_array().expect("turns") {
                let timings = &turn["stage_timings"];
                assert_eq!(timings["snapshots_available"], true);
                let phases = timings["phases"].as_array().expect("phase side table");
                for observed in ["preparation", "provider_dispatch", "provider_first_content"] {
                    let phase = phases
                        .iter()
                        .find(|phase| phase["phase"] == observed)
                        .expect("closed timing phase");
                    assert!(
                        phase["elapsed_us"]
                            .as_array()
                            .is_some_and(|values| values.len() == 1),
                        "each turn must contain only its own actual phase: {turn}"
                    );
                }
                let feedback = phases
                    .iter()
                    .find(|phase| phase["phase"] == "first_feedback_frame")
                    .expect("UI phase remains explicit");
                assert!(
                    feedback["elapsed_us"].is_null(),
                    "model eval has no UI frame sampling"
                );
            }
            assert!(trajectory["trajectory"]["human_interventions"].is_null());
            assert!(trajectory["trajectory"]["ineffective_repair_rounds"].is_null());
            assert!(trajectory["billing"]["reported_or_priced_cost_usd"].is_null());
            assert_eq!(
                trajectory["billing"]["budget_accounting_is_actual_bill"],
                false
            );
            assert!(trajectory["trajectory"]["redundant_reads"].is_null());
            assert_eq!(
                trajectory["trajectory"]["activity"]["scope"],
                "observed_session_stream"
            );
            assert!(
                trajectory["trajectory"]["activity"]["reads"]["repeated_output_reads"].is_null()
            );
            assert_eq!(
                trajectory["trajectory"]["activity"]["decisions"]["tool_decision_events"],
                0
            );
        });
}

#[test]
fn model_eval_activity_qualifies_real_read_file_calls_and_v3_output() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_activity_qualifies_real_read_file_calls_and_v3_output",
        "SIGIL_TEST_MODEL_EVAL_ACTIVITY_READ_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let args = serde_json::json!({"path": "src/lib.rs", "offset": 0, "limit": 2000});
            let (base_url, mut server) = spawn_scripted_eval_tool_sequence_server(
                Arc::clone(&requests),
                vec![
                    ("read_file".to_owned(), args.clone()),
                    ("read_file".to_owned(), args),
                ],
            )
            .await
            .expect("scripted provider");
            let temp = tempdir().expect("temp");
            let fixture_path = temp.path().join("fixture");
            fs::create_dir(&fixture_path).expect("fixture directory");
            write_python_model_eval_fixture(&fixture_path, false);
            let source = fs::read_to_string(fixture_path.join("files/src/lib.rs"))
                .expect("original fixture source");
            assert!(source.contains("left + right"));
            let config_path = temp.path().join("source.toml");
            write_source_config(&config_path, &base_url, "auto-edit");
            let result = run_model_eval_campaign(
                ModelEvalCampaignRequest {
                    config_path,
                    fixture_roots: vec![fixture_path],
                    orchestration_route_contract: None,
                    repetitions: 1,
                    max_cost_microusd: 500_000,
                    campaign_timeout: Duration::from_secs(30),
                    output_dir: temp.path().join("campaign"),
                    release_output_owner: None,
                },
                &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
            )
            .await;
            // Retain and join the local provider on both campaign outcomes. A failed run must
            // not strand a server waiting for its next scripted request.
            let joined = match tokio::time::timeout(Duration::from_secs(2), &mut server).await {
                Ok(joined) => joined,
                Err(_) => {
                    server.abort();
                    let _ = server.await;
                    panic!("campaign returned without consuming the bounded provider script");
                }
            };
            joined.expect("provider joined");
            let campaign = result.expect("campaign");
            let run = &campaign.runs[0];
            assert_eq!(run.turns.len(), 1);
            assert_eq!(
                run.verification
                    .as_ref()
                    .expect("independent check")
                    .verdict,
                VerificationVerdict::Failed,
                "two real reads do not repair the fixture"
            );
            let captured = requests.lock().expect("requests");
            assert_eq!(captured.len(), 3, "two tool rounds and one final response");
            let final_request: serde_json::Value =
                serde_json::from_str(captured[2].split_once("\r\n\r\n").expect("HTTP body").1)
                    .expect("provider request JSON");
            let messages = final_request["messages"]
                .as_array()
                .expect("provider messages");
            let replies = messages
                .iter()
                .filter(|message| message["role"] == "tool")
                .collect::<Vec<_>>();
            assert_eq!(replies.len(), 2);
            for (reply, call_id) in replies.iter().zip(["call-fixture", "call-fixture-1"]) {
                assert_eq!(reply["tool_call_id"], call_id);
                let envelope: serde_json::Value =
                    serde_json::from_str(reply["content"].as_str().expect("real tool reply"))
                        .expect("tool envelope");
                assert_eq!(envelope["facts"]["status"], "ok");
                let preview = envelope["projection"]["preview"]
                    .as_str()
                    .expect("read preview");
                assert_eq!(preview, source.trim_end_matches('\n'));
                assert!(preview.contains("left + right"));
            }
            drop(captured);
            let records = JsonlSessionStore::read_event_records(&run.session_path)
                .expect("actual durable execution");
            let entries = records
                .iter()
                .filter_map(|record| record.session_log_entry().expect("typed durable entry"))
                .collect::<Vec<_>>();
            let outputs = entries
                .iter()
                .filter_map(|entry| match entry {
                    sigil_kernel::SessionLogEntry::ToolResultV3(output)
                        if output.tool_name == "read_file" =>
                    {
                        Some(output)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(outputs.len(), 2);
            let mut descriptors = Vec::new();
            for (output, call_id) in outputs.iter().zip(["call-fixture", "call-fixture-1"]) {
                assert_eq!(output.call_id, call_id);
                assert!(output.initial_model_view.preview.contains("left + right"));
                let audits = entries
                    .iter()
                    .filter_map(|entry| match entry {
                        sigil_kernel::SessionLogEntry::Control(ControlEntry::ToolExecution(
                            audit,
                        )) if audit.call_id == call_id => Some(audit),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(audits.len(), 2);
                assert_eq!(audits[0].status, ToolExecutionStatus::Started);
                assert_eq!(audits[1].status, ToolExecutionStatus::Completed);
                assert_eq!(audits[1].metadata.limit_lines, Some(2000));
                assert!(audits[1].metadata.limit_bytes.is_some());
                // Builtin read_file is a known pure read, so the real parallel-read owner
                // records its exact permission plan instead of an unknown-mutation profile.
                assert!(
                    audits[0]
                        .metadata
                        .details
                        .pointer("/execution_mutation_profile")
                        .is_none()
                );
                assert!(
                    audits[0]
                        .metadata
                        .details
                        .pointer("/permission_plan_hash")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|hash| !hash.is_empty())
                );
                assert_eq!(audits[0].subjects, audits[1].subjects);
                assert_eq!(audits[0].subjects.len(), 1);
                assert_eq!(
                    audits[0].subjects[0].kind,
                    sigil_kernel::ToolSubjectKind::Path
                );
                assert!(audits[0].subjects[0].canonical_path_sha256.is_none());
                assert_eq!(
                    audits[0].subjects[0].scope,
                    sigil_kernel::ToolSubjectScope::Workspace
                );
                assert_eq!(
                    audits[0].subjects[0].identity_sha256,
                    sigil_kernel::stable_event_hash(b"src/lib.rs")
                );
                let access: sigil_kernel::managed_execution::BorrowedResourceAccessReceiptV1 =
                    serde_json::from_value(
                        output.facts.tool_specific["managed_access_receipt"].clone(),
                    )
                    .expect("actual managed-file receipt survives the bounded V3 facts");
                assert_eq!(
                    access.effect_settlement,
                    sigil_kernel::recovery::EffectSettlementV1::Applied
                );
                assert!(
                    access.identity_before.is_some(),
                    "receipt binds the borrowed workspace root"
                );
                assert_eq!(
                    audits[0].metadata.details.get("call"),
                    audits[1].metadata.details.get("call")
                );
                assert_eq!(
                    audits[1]
                        .metadata
                        .details
                        .pointer("/call/path_sha256")
                        .and_then(serde_json::Value::as_str),
                    Some(format!("{:x}", Sha256::digest(b"src/lib.rs")).as_str())
                );
                descriptors.push(
                    output
                        .artifact
                        .descriptor()
                        .expect("actual full output artifact"),
                );
            }
            assert_eq!(
                descriptors[0].content_sha256,
                sigil_kernel::stable_event_hash(source.trim_end_matches('\n').as_bytes())
            );
            assert_eq!(descriptors[0].content_sha256, descriptors[1].content_sha256);
            assert_eq!(
                descriptors[0].persisted_bytes,
                descriptors[1].persisted_bytes
            );
            let trajectory: serde_json::Value = serde_json::from_str(
                fs::read_to_string(campaign.output_dir.join("trajectory.jsonl"))
                    .expect("trajectory")
                    .trim(),
            )
            .expect("trajectory JSON");
            let activity = &trajectory["trajectory"]["activity"];
            assert_eq!(activity["reads"]["completed_read_events"], 2);
            assert_eq!(activity["reads"]["qualified_reads"], 2);
            assert_eq!(activity["reads"]["canonical_subject_reads"], 0);
            assert_eq!(activity["reads"]["managed_logical_subject_reads"], 2);
            assert_eq!(activity["reads"]["excluded_completed_read_events"], 0);
            assert_eq!(activity["reads"]["repeated_output_reads"], 1);
            assert_eq!(
                activity["reads"]["repeated_persisted_output_bytes"],
                descriptors[1].persisted_bytes
            );
            assert!(trajectory["trajectory"]["redundant_reads"].is_null());
            let serialized = serde_json::to_string(activity).expect("activity serialization");
            assert!(!serialized.contains("src/lib.rs"));
            assert!(!serialized.contains("left + right"));
            assert!(!serialized.contains(temp.path().to_string_lossy().as_ref()));
        });
}

#[test]
fn model_eval_python_verification_retains_actual_patch_and_rejects_changed_oracle() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_python_verification_retains_actual_patch_and_rejects_changed_oracle",
        "SIGIL_TEST_MODEL_EVAL_PYTHON_PATCH_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
        .block_on(async {
            for corrupt_oracle in [false, true] {
                let requests = Arc::new(Mutex::new(Vec::new()));
                let args = if corrupt_oracle {
                    serde_json::json!({"path": "check.py", "old_text": "assert 'left * right' in Path('src/lib.rs').read_text()", "new_text": "assert True"})
                } else {
                    serde_json::json!({"path": "src/lib.rs", "old_text": "left + right", "new_text": "left * right"})
                };
                let base_url = spawn_scripted_eval_tool_server(requests, "edit_file", args).await.expect("server");
                let temp = tempdir().expect("temp");
                let fixture_path = temp.path().join("fixture");
                fs::create_dir(&fixture_path).expect("fixture directory");
                write_python_model_eval_fixture(&fixture_path, false);
                let config_path = temp.path().join("source.toml");
                write_source_config(&config_path, &base_url, "auto-edit");
                let campaign = run_model_eval_campaign(ModelEvalCampaignRequest {
                    config_path, fixture_roots: vec![fixture_path], orchestration_route_contract: None,
                    repetitions: 1, max_cost_microusd: 500_000,
                    campaign_timeout: Duration::from_secs(30), output_dir: temp.path().join("campaign"),
                    release_output_owner: None,
                }, &ApplicationRunServices::new(Arc::new(RejectingPresenter))).await.expect("campaign");
                let run = &campaign.runs[0];
                assert_eq!(run.verification.as_ref().expect("verification").verdict, VerificationVerdict::Passed);
                let row: serde_json::Value = serde_json::from_str(
                    fs::read_to_string(campaign.output_dir.join("results.jsonl")).expect("report").trim(),
                ).expect("decode report");
                assert_eq!(row["acceptance_passed"], !corrupt_oracle);
                let oracle = row["assertion_results"].as_array().expect("assertions").iter()
                    .find(|entry| entry["assertion_id"] == "independent-oracle" || entry["id"] == "independent-oracle")
                    .expect("oracle assertion");
                assert_eq!(oracle["passed"], !corrupt_oracle);
                let patch = fs::read_to_string(campaign.output_dir.join("small-code-edit-1.patch")).expect("actual patch");
                if corrupt_oracle {
                    assert!(patch.contains("+++ b/check.py"));
                    assert!(patch.contains("+assert True"));
                } else {
                    assert!(patch.contains("--- a/src/lib.rs"));
                    assert!(patch.contains("-    left + right"));
                    assert!(patch.contains("+    left * right"));
                }
            }
        });
}

#[cfg(unix)]
#[test]
fn model_eval_deadline_joins_real_python_verification_before_returning() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_deadline_joins_real_python_verification_before_returning",
        "SIGIL_TEST_MODEL_EVAL_DEADLINE_JOIN_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
        .block_on(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let base_url = spawn_direct_routing_eval_server(requests).await.expect("server");
            let temp = tempdir().expect("temp");
            let fixture_path = temp.path().join("fixture");
            fs::create_dir(&fixture_path).expect("fixture directory");
            write_python_model_eval_fixture(&fixture_path, false);
            let manifest_path = fixture_path.join("fixture.toml");
            let mut manifest: crate::model_eval::ModelEvalFixtureManifest =
                toml::from_str(&fs::read_to_string(&manifest_path).expect("manifest")).expect("decode");
            manifest.checks[0].command = vec!["python3".into(), "-B".into(), "-c".into(),
                "import os,time; from pathlib import Path; Path('.observed-check-pid').write_text(str(os.getpid())); time.sleep(30)".into()];
            fs::write(&manifest_path, toml::to_string(&manifest).expect("encode")).expect("manifest");
            let config_path = temp.path().join("source.toml");
            write_source_config(&config_path, &base_url, "auto-edit");
            let started = std::time::Instant::now();
            let campaign = run_model_eval_campaign(ModelEvalCampaignRequest {
                config_path, fixture_roots: vec![fixture_path], orchestration_route_contract: None,
                repetitions: 1, max_cost_microusd: 500_000,
                campaign_timeout: Duration::from_secs(5), output_dir: temp.path().join("campaign"),
                release_output_owner: None,
            }, &ApplicationRunServices::new(Arc::new(RejectingPresenter))).await.expect("campaign");
            assert!(started.elapsed() < Duration::from_secs(12));
            let run = &campaign.runs[0];
            assert_eq!(run.status, ModelEvalRunExecutionStatus::TimedOut);
            let pid = fs::read_to_string(run.workspace_root.join(".observed-check-pid"))
                .expect("real check must start before its deadline");
            let still_alive = Command::new("kill").args(["-0", pid.trim()])
                .output().expect("observe owned check exit").status.success();
            assert!(!still_alive, "verification process must be reaped before returning");
            let records = JsonlSessionStore::read_event_records(&run.session_path).expect("session");
            let finished = records.iter().map(|record| record.stored_event()).find(|event|
                event.event_type == sigil_kernel::DurableEventType::CommandFinished.as_str())
                .expect("real durable command receipt");
            assert_eq!(finished.payload["timed_out"], true);
            assert_eq!(finished.payload["execution_resources"]["cleanup"]["status"], "completed");
            assert_ne!(run.verification.as_ref().expect("joined verification").verdict, VerificationVerdict::Passed);
        });
}

#[cfg(unix)]
#[test]
fn model_eval_registered_exec_command_runs_beyond_old_six_tool_gate() {
    if !enter_isolated_environment_test(
        "model_eval_tests::model_eval_registered_exec_command_runs_beyond_old_six_tool_gate",
        "SIGIL_TEST_MODEL_EVAL_EXEC_SCOPE_CHILD",
    ) {
        return;
    }
    let _env_lock = crate::test_env::lock();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let arguments = serde_json::json!({
                "command": "python3 -B -c 'from pathlib import Path; p=Path(\"src/lib.rs\"); s=p.read_text(); assert \"left + right\" in s; p.write_text(s.replace(\"left + right\",\"left * right\"))'",
                "shell": "sh",
                "yield_time_ms": 30000,
                "max_runtime_secs": 10
            });
            let base_url = spawn_scripted_eval_tool_server(
                Arc::clone(&requests), "exec_command", arguments,
            ).await.expect("server");
            let temp = tempdir().expect("temp");
            let fixture_path = temp.path().join("fixture");
            fs::create_dir(&fixture_path).expect("fixture directory");
            write_python_model_eval_fixture(&fixture_path, false);
            let manifest_path = fixture_path.join("fixture.toml");
            let mut manifest: crate::model_eval::ModelEvalFixtureManifest =
                toml::from_str(&fs::read_to_string(&manifest_path).expect("manifest"))
                    .expect("parse manifest");
            // One real registered tool isolates membership from the old six-entry count limit.
            manifest.allowed_tools = vec!["exec_command".to_owned()];
            fs::write(&manifest_path, toml::to_string(&manifest).expect("encode manifest"))
                .expect("save manifest");
            let config_path = temp.path().join("source.toml");
            write_source_config(&config_path, &base_url, "danger-full-access");
            let config = fs::read_to_string(&config_path).expect("source config");
            fs::write(&config_path, format!("{config}\n[execution]\nstrategy = \"local\"\n"))
                .expect("explicit local fixture backend");
            let campaign = run_model_eval_campaign(
                ModelEvalCampaignRequest {
                    config_path,
                    fixture_roots: vec![fixture_path],
                    orchestration_route_contract: None,
                    repetitions: 1,
                    max_cost_microusd: 500_000,
                    campaign_timeout: Duration::from_secs(30),
                    output_dir: temp.path().join("campaign"),
                    release_output_owner: None,
                },
                &ApplicationRunServices::new(Arc::new(RejectingPresenter)),
            ).await.expect("registered exec_command campaign");
            let run = &campaign.runs[0];
            assert_eq!(run.verification.as_ref().expect("actual Python check").verdict,
                VerificationVerdict::Passed);
            assert!(fs::read_to_string(run.workspace_root.join("src/lib.rs"))
                .expect("actual process edit").contains("left * right"));
            let stored = JsonlSessionStore::read_event_records(&run.session_path).expect("records");
            assert!(stored.iter().any(|record| matches!(record.session_log_entry().ok().flatten(),
                Some(sigil_kernel::SessionLogEntry::Control(ControlEntry::ToolExecution(entry)))
                if entry.tool_name == "exec_command"
                    && entry.status == sigil_kernel::ToolExecutionStatus::Completed)));
            let report: serde_json::Value = serde_json::from_str(
                fs::read_to_string(campaign.output_dir.join("results.jsonl"))
                    .expect("report").trim(),
            ).expect("decode report");
            assert_eq!(report["acceptance_passed"], true, "{report}");
            assert_eq!(requests.lock().expect("requests").len(), 2);
        });
}
