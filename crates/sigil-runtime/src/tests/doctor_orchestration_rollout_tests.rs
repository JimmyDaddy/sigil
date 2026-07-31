use sigil_kernel::{MultiAgentMode, RootConfig, TaskRoutingPolicy};

use super::*;

fn root_config() -> RootConfig {
    toml::from_str(
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
"#,
    )
    .expect("test config")
}

#[test]
fn doctor_reports_coarse_manual_rollback_without_loading_release_manifest() {
    let config = root_config();
    let mut report = DoctorReport::default();

    check_orchestration_rollout(&mut report, &config);

    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].status, DoctorStatus::Ok);
    assert!(
        report.checks[0]
            .message
            .contains("coarse rollback is active")
    );
}

#[test]
fn doctor_warns_when_explicit_auto_route_is_not_release_qualified() {
    let _lock = crate::test_env::lock();
    let temp = tempfile::tempdir().expect("temp dir");
    let missing = temp.path().join("missing-rollout.json");
    let _manifest = crate::test_env::EnvScope::set(
        crate::SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST_ENV,
        missing.as_os_str(),
    );
    let mut config = root_config();
    config.task.routing_policy = TaskRoutingPolicy::Auto;
    config.task.multi_agent_mode = MultiAgentMode::Proactive;
    let mut report = DoctorReport::default();

    check_orchestration_rollout(&mut report, &config);

    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].status, DoctorStatus::Warn);
    assert!(report.checks[0].message.contains("explicit orchestration"));
    assert!(
        report.checks[0]
            .remediation
            .as_deref()
            .is_some_and(|value| value.contains("routing_policy=\"manual\""))
    );
}
