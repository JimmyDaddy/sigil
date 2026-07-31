use std::{fs, path::Path};

use anyhow::Result;
use sigil_kernel::{
    MultiAgentMode, OrchestrationEvalReportManifestV1, OrchestrationEvalRouteGateV1,
    OrchestrationEvalRouteIdentityV1, OrchestrationEvalRouteStatus, RootConfig, TaskRoutingPolicy,
    stable_event_hash,
};

use super::*;

fn qualified_gate(task_config_digest: String) -> OrchestrationEvalRouteGateV1 {
    let commit = ORCHESTRATION_RUNTIME_BUILD_ID
        .rsplit_once('+')
        .expect("test build identity includes commit")
        .1;
    let digest = format!("sha256:{}", "a".repeat(64));
    let identity = OrchestrationEvalRouteIdentityV1 {
        provider_adapter: "deepseek".to_owned(),
        provider_kind: "deepseek".to_owned(),
        endpoint_family: DEEPSEEK_ENDPOINT_FAMILY.to_owned(),
        canonical_model_id: "deepseek-v4-flash".to_owned(),
        canonical_model_version: "DeepSeek-V4-Flash@fp-test".to_owned(),
        route_fingerprint: digest.clone(),
        routing_prompt_digest: digest.clone(),
        planner_prompt_digest: digest.clone(),
        system_prompt_digest: digest.clone(),
        tool_profile_contract_digest: digest.clone(),
        task_config_digest,
        corpus_version: "rfc-0053-orchestration-v1".to_owned(),
        corpus_digest: digest,
        sigil_commit: commit.to_owned(),
        sigil_build: ORCHESTRATION_RUNTIME_BUILD_ID.to_owned(),
    };
    let identity_digest =
        stable_event_hash(serde_json::to_vec(&identity).expect("serialize route identity"));
    OrchestrationEvalRouteGateV1 {
        identity,
        identity_digest,
        status: OrchestrationEvalRouteStatus::Qualified,
        negative_cases: 20,
        positive_cases: 10,
        eligible_negative_cases: 20,
        eligible_positive_cases: 10,
        provider_admitted_repetitions: 90,
        completed_repetitions: 90,
        false_positive_rate_ppm: Some(0),
        positive_miss_rate_ppm: Some(0),
        cases_with_majority_misroute: 0,
        cases_with_duplicate_repetition_identity: 0,
        hard_invariant_violations: 0,
        reasons: Vec::new(),
    }
}

fn qualified_report(task_config_digest: String) -> OrchestrationEvalReportManifestV1 {
    OrchestrationEvalReportManifestV1 {
        report_schema_version: 1,
        campaign_id: "campaign-qualified".to_owned(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        requested_repetitions: 90,
        results_jsonl_path: "private/results.jsonl".into(),
        summary_path: "private/summary.md".into(),
        route_gates: vec![qualified_gate(task_config_digest)],
    }
}

fn default_setup_config() -> Result<RootConfig> {
    v2_setup_config()
}

fn v2_setup_config() -> Result<RootConfig> {
    Ok(toml::from_str(
        r#"
config_version = 2

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[connections.deepseek-default.options]
beta_base_url = "https://api.deepseek.com/beta"
anthropic_base_url = "https://api.deepseek.com/anthropic"
user_id_strategy = "stable_per_end_user"
strict_tools_mode = "auto"
fim_model = "deepseek-v4-pro"
"#,
    )?)
}

fn write_rollout_manifest(path: &Path, task_digest: String) -> Result<()> {
    let report = qualified_report(task_digest);
    let manifest = build_orchestration_rollout_manifest(&report)?;
    write_orchestration_rollout_manifest(&manifest, path)
}

#[test]
fn rollout_manifest_is_path_free_and_round_trips() -> Result<()> {
    let task_digest = orchestration_task_config_digest(&sigil_kernel::TaskConfig {
        routing_policy: TaskRoutingPolicy::Auto,
        multi_agent_mode: MultiAgentMode::Proactive,
        ..sigil_kernel::TaskConfig::default()
    })?;
    let report = qualified_report(task_digest);
    let manifest = build_orchestration_rollout_manifest(&report)?;

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.sigil_build, ORCHESTRATION_RUNTIME_BUILD_ID);
    assert_eq!(manifest.qualified_routes.len(), 1);
    let rendered = serde_json::to_string(&manifest)?;
    assert!(!rendered.contains("private/results"));
    assert!(!rendered.contains("private/summary"));

    let temp = tempfile::tempdir()?;
    let path = temp.path().join(ORCHESTRATION_ROLLOUT_MANIFEST_FILE_NAME);
    write_orchestration_rollout_manifest(&manifest, &path)?;
    assert_eq!(load_orchestration_rollout_manifest(&path)?, manifest);
    assert!(write_orchestration_rollout_manifest(&manifest, &path).is_err());
    Ok(())
}

#[test]
fn quick_setup_applies_only_the_exact_qualified_auto_proactive_route() -> Result<()> {
    let _lock = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let path = temp.path().join(ORCHESTRATION_ROLLOUT_MANIFEST_FILE_NAME);
    let mut config = default_setup_config()?;
    let mut target_task = config.task.clone();
    target_task.routing_policy = TaskRoutingPolicy::Auto;
    target_task.multi_agent_mode = MultiAgentMode::Proactive;
    write_rollout_manifest(&path, orchestration_task_config_digest(&target_task)?)?;
    let _manifest =
        crate::test_env::EnvScope::set(SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST_ENV, path.as_os_str());

    let decision = apply_new_install_orchestration_rollout(&mut config);

    assert!(decision.is_qualified());
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Auto);
    assert_eq!(config.task.multi_agent_mode, MultiAgentMode::Proactive);
    assert!(decision.route_identity_digest.is_some());
    Ok(())
}

#[test]
fn quick_setup_applies_the_qualified_route_after_v2_materialization() -> Result<()> {
    let _lock = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let path = temp.path().join(ORCHESTRATION_ROLLOUT_MANIFEST_FILE_NAME);
    let mut config = v2_setup_config()?;
    let mut target_task = config.task.clone();
    target_task.routing_policy = TaskRoutingPolicy::Auto;
    target_task.multi_agent_mode = MultiAgentMode::Proactive;
    write_rollout_manifest(&path, orchestration_task_config_digest(&target_task)?)?;
    let _manifest =
        crate::test_env::EnvScope::set(SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST_ENV, path.as_os_str());

    let decision = apply_new_install_orchestration_rollout(&mut config);

    assert!(decision.is_qualified());
    assert!(config.agent.runtime_provider.is_empty());
    assert_eq!(
        config.agent.connection.as_ref().map(|id| id.as_str()),
        Some("deepseek-default")
    );
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Auto);
    assert_eq!(config.task.multi_agent_mode, MultiAgentMode::Proactive);
    Ok(())
}

#[test]
fn quick_setup_fails_closed_for_missing_stale_or_mismatched_manifests() -> Result<()> {
    let _lock = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("missing.json");
    let _manifest = crate::test_env::EnvScope::set(
        SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST_ENV,
        missing.as_os_str(),
    );
    let mut config = default_setup_config()?;

    let missing_decision = apply_new_install_orchestration_rollout(&mut config);
    assert_eq!(
        missing_decision.status,
        NewInstallOrchestrationRolloutStatus::ManifestUnavailable
    );
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Manual);
    assert_eq!(
        config.task.multi_agent_mode,
        MultiAgentMode::ExplicitRequestOnly
    );

    fs::write(&missing, b"{not json")?;
    let invalid_decision = apply_new_install_orchestration_rollout(&mut config);
    assert_eq!(
        invalid_decision.status,
        NewInstallOrchestrationRolloutStatus::ManifestInvalid
    );

    fs::remove_file(&missing)?;
    let wrong_task_digest = format!("sha256:{}", "f".repeat(64));
    write_rollout_manifest(&missing, wrong_task_digest)?;
    let mismatch = apply_new_install_orchestration_rollout(&mut config);
    assert_eq!(
        mismatch.status,
        NewInstallOrchestrationRolloutStatus::RouteNotQualified
    );
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Manual);
    Ok(())
}

#[test]
fn rollout_rejects_tampered_gate_and_custom_endpoint() -> Result<()> {
    let _lock = crate::test_env::lock();
    let temp = tempfile::tempdir()?;
    let path = temp.path().join(ORCHESTRATION_ROLLOUT_MANIFEST_FILE_NAME);
    let mut config = default_setup_config()?;
    let mut target_task = config.task.clone();
    target_task.routing_policy = TaskRoutingPolicy::Auto;
    target_task.multi_agent_mode = MultiAgentMode::Proactive;
    let task_digest = orchestration_task_config_digest(&target_task)?;
    let mut report = qualified_report(task_digest);
    report.route_gates[0].positive_miss_rate_ppm = Some(100_001);
    assert!(build_orchestration_rollout_manifest(&report).is_err());

    let task_digest = orchestration_task_config_digest(&target_task)?;
    write_rollout_manifest(&path, task_digest)?;
    let _manifest =
        crate::test_env::EnvScope::set(SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST_ENV, path.as_os_str());
    config
        .connections
        .get_mut("deepseek-default")
        .and_then(serde_json::Value::as_object_mut)
        .expect("DeepSeek connection object")
        .insert(
            "base_url".to_owned(),
            serde_json::Value::String("https://proxy.example.test".to_owned()),
        );

    let decision = apply_new_install_orchestration_rollout(&mut config);
    assert_eq!(
        decision.status,
        NewInstallOrchestrationRolloutStatus::RouteNotQualified
    );
    assert_eq!(config.task.routing_policy, TaskRoutingPolicy::Manual);
    Ok(())
}

#[cfg(unix)]
#[test]
fn rollout_loader_rejects_symlinks() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let target = temp.path().join("target.json");
    let link = temp.path().join("link.json");
    let config = default_setup_config()?;
    let mut task = config.task;
    task.routing_policy = TaskRoutingPolicy::Auto;
    task.multi_agent_mode = MultiAgentMode::Proactive;
    write_rollout_manifest(&target, orchestration_task_config_digest(&task)?)?;
    symlink(&target, &link)?;

    assert!(load_orchestration_rollout_manifest(&link).is_err());
    Ok(())
}
