use sigil_kernel::{MultiAgentMode, RootConfig, TaskRoutingPolicy};

use super::*;
use crate::tests::rollout_manifest_test_support::qualified_rollout_manifest_guard;

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

fn check_names(report: &DoctorReport) -> Vec<&str> {
    report
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect()
}

#[test]
fn doctor_reports_fresh_auto_default_as_review_first_facts() {
    let config = root_config();
    let mut report = DoctorReport::default();

    check_orchestration_rollout(&mut report, &config);

    assert_eq!(
        check_names(&report),
        vec![
            "orchestration:automatic-routing",
            "orchestration:plan-review",
            "orchestration:direct-task",
        ]
    );
    assert_eq!(report.checks[0].status, DoctorStatus::Ok);
    assert!(report.checks[0].message.contains("review-first baseline"));
    assert_eq!(report.checks[1].status, DoctorStatus::Ok);
    assert!(report.checks[1].message.contains("available"));
    assert_eq!(report.checks[2].status, DoctorStatus::Ok);
    assert!(report.checks[2].message.contains("review-first fallback"));
    assert!(
        report.checks[2]
            .remediation
            .as_deref()
            .is_some_and(|value| value.contains("routing_policy=\"manual\""))
    );
}

#[test]
fn doctor_reports_explicit_manual_as_disabled_facts() {
    let mut config = root_config();
    config.task.routing_policy = TaskRoutingPolicy::Manual;
    let mut report = DoctorReport::default();

    check_orchestration_rollout(&mut report, &config);

    assert_eq!(report.checks.len(), 3);
    assert_eq!(report.checks[0].status, DoctorStatus::Ok);
    assert!(report.checks[0].message.contains("disabled"));
    assert!(report.checks[1].message.contains("blocked"));
    assert!(report.checks[2].message.contains("unavailable"));
}

#[test]
fn doctor_reports_disabled_task_mode_as_unavailable_facts() {
    let mut config = root_config();
    config.task.enabled = false;
    let mut report = DoctorReport::default();

    check_orchestration_rollout(&mut report, &config);

    assert_eq!(report.checks.len(), 3);
    assert_eq!(report.checks[0].status, DoctorStatus::Ok);
    assert!(report.checks[0].message.contains("unavailable"));
    assert!(report.checks[1].message.contains("blocked"));
    assert!(report.checks[2].message.contains("unavailable"));
}

#[test]
fn doctor_reports_qualified_release_route_facts() {
    let mut config = root_config();
    config.task.routing_policy = TaskRoutingPolicy::Auto;
    config.task.multi_agent_mode = MultiAgentMode::Proactive;
    let _guard = qualified_rollout_manifest_guard(&config);
    let mut report = DoctorReport::default();

    check_orchestration_rollout(&mut report, &config);

    assert_eq!(report.checks.len(), 3);
    assert_eq!(report.checks[0].status, DoctorStatus::Ok);
    assert!(report.checks[0].message.contains("qualified release route"));
    assert!(report.checks[1].message.contains("available"));
    assert_eq!(report.checks[2].status, DoctorStatus::Ok);
    assert!(report.checks[2].message.contains("qualified"));
}

#[test]
fn doctor_warns_when_proactive_agents_are_configured_without_qualification() {
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

    assert_eq!(report.checks.len(), 4);
    assert_eq!(report.checks[0].status, DoctorStatus::Ok);
    assert_eq!(report.checks[3].status, DoctorStatus::Warn);
    assert!(report.checks[3].name.contains("proactive-agents"));
    assert!(
        report.checks[3]
            .message
            .contains("without a qualified release route")
    );
}
