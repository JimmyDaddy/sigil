use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use sigil_kernel::{
    OrchestrationEvalReportManifestV1, OrchestrationEvalRouteGateV1,
    OrchestrationEvalRouteIdentityV1, OrchestrationEvalRouteStatus, RootConfig, stable_event_hash,
};

use crate::{
    ORCHESTRATION_RUNTIME_BUILD_ID, build_orchestration_rollout_manifest,
    orchestration_task_config_digest,
};

static MANIFEST_LOCK: Mutex<()> = Mutex::new(());
static MANIFEST_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn rollout_manifest_env_lock() -> MutexGuard<'static, ()> {
    MANIFEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn manifest_dir() -> PathBuf {
    MANIFEST_DIR
        .get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("sigil-rollout-test-{}", std::process::id()));
            fs::create_dir_all(&dir).expect("rollout test dir should be creatable");
            dir
        })
        .clone()
}

/// Builds a valid release-qualified route gate for one test route.
fn qualified_gate(root_config: &RootConfig) -> OrchestrationEvalRouteGateV1 {
    let commit = ORCHESTRATION_RUNTIME_BUILD_ID
        .rsplit_once('+')
        .expect("test build identity includes commit")
        .1;
    let digest = format!("sha256:{}", "a".repeat(64));
    let identity = OrchestrationEvalRouteIdentityV1 {
        provider_adapter: "deepseek".to_owned(),
        provider_kind: "deepseek".to_owned(),
        endpoint_family: "openai_chat_completions".to_owned(),
        canonical_model_id: root_config.agent.model.trim().to_owned(),
        canonical_model_version: "DeepSeek-V4-Flash@fp-test".to_owned(),
        route_fingerprint: digest.clone(),
        routing_prompt_digest: digest.clone(),
        planner_prompt_digest: digest.clone(),
        system_prompt_digest: digest.clone(),
        tool_profile_contract_digest: digest.clone(),
        task_config_digest: orchestration_task_config_digest(&root_config.task)
            .expect("test task config digest"),
        corpus_version: "rfc-0063-orchestration-v1".to_owned(),
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
        chat_cases: 20,
        plan_review_cases: 15,
        direct_task_cases: 15,
        eligible_chat_cases: 20,
        eligible_plan_review_cases: 15,
        eligible_direct_task_cases: 15,
        provider_admitted_repetitions: 150,
        completed_repetitions: 150,
        chat_to_task_false_positive_rate_ppm: Some(0),
        plan_review_to_task_premature_rate_ppm: Some(0),
        direct_task_miss_rate_ppm: Some(0),
        chat_to_plan_review_overroute_rate_ppm: Some(0),
        plan_review_miss_rate_ppm: Some(0),
        cases_with_majority_misroute: 0,
        cases_with_duplicate_repetition_identity: 0,
        hard_invariant_violations: 0,
        reasons: Vec::new(),
    }
}

/// RAII guard that points `SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST` at a valid qualified manifest
/// for the exact test route while it is alive.
///
/// The environment variable is process-global, so callers are serialized on a process-wide mutex
/// and the variable is removed when the guard drops. Async test bodies must keep the guard alive
/// across the whole await.
pub fn qualified_rollout_manifest_guard(root_config: &RootConfig) -> QualifiedRolloutManifestGuard {
    let lock = rollout_manifest_env_lock();
    let path = manifest_dir().join("sigil-orchestration-rollout-v1.json");
    let report = OrchestrationEvalReportManifestV1 {
        report_schema_version: 2,
        campaign_id: "campaign-rfc-0063-test".to_owned(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        requested_repetitions: 150,
        results_jsonl_path: "private/results.jsonl".into(),
        summary_path: "private/summary.md".into(),
        route_gates: vec![qualified_gate(root_config)],
    };
    let manifest =
        build_orchestration_rollout_manifest(&report).expect("test manifest should build");
    write_qualified_manifest(&path, &manifest);
    unsafe {
        std::env::set_var("SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST", &path);
    }
    QualifiedRolloutManifestGuard { _lock: lock }
}

pub struct QualifiedRolloutManifestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for QualifiedRolloutManifestGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST");
        }
    }
}

fn write_qualified_manifest(path: &Path, manifest: &crate::OrchestrationRolloutManifestV1) {
    let bytes = serde_json::to_vec(manifest).expect("manifest should serialize");
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, &bytes).expect("manifest temp write should succeed");
    fs::rename(&temporary, path).expect("manifest rename should succeed");
}
