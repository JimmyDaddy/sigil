use super::*;
use std::path::PathBuf;

#[test]
fn r71_release_model_eval_manifest_requires_every_requested_repetition() {
    let mut manifest = sigil_kernel::ModelEvalReportManifestV3 {
        report_schema_version: 3,
        campaign_id: "campaign-test".to_owned(),
        mode: "model".to_owned(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        requested_repetitions: 15,
        provider_admitted_repetitions: 15,
        completed_repetitions: 15,
        skipped_repetitions: 0,
        accepted_repetitions: 15,
        charged_microusd: 499_995,
        results_jsonl_path: PathBuf::from("results.jsonl"),
        summary_path: PathBuf::from("summary.md"),
        trend_buckets: Vec::new(),
    };
    validate_model_eval_manifest(&manifest).expect("valid");

    manifest.provider_admitted_repetitions = 14;
    manifest.completed_repetitions = 14;
    manifest.skipped_repetitions = 1;
    manifest.accepted_repetitions = 14;
    let error = validate_model_eval_manifest(&manifest)
        .expect_err("one skipped repetition must fail campaign acceptance");
    assert!(error.to_string().contains("requested 15"));
    assert!(error.to_string().contains("skipped 1"));

    manifest.requested_repetitions = 0;
    manifest.provider_admitted_repetitions = 0;
    manifest.completed_repetitions = 0;
    manifest.skipped_repetitions = 0;
    manifest.accepted_repetitions = 0;
    assert!(validate_model_eval_manifest(&manifest).is_err());
}

#[test]
fn r71_release_orchestration_eval_manifest_requires_qualified_route_gates() {
    let mut manifest =
        orchestration_eval_manifest(sigil_kernel::OrchestrationEvalRouteStatus::Qualified);
    validate_orchestration_eval_manifest(&manifest).expect("valid");
    manifest = orchestration_eval_manifest(sigil_kernel::OrchestrationEvalRouteStatus::Blocked);
    let error = validate_orchestration_eval_manifest(&manifest)
        .expect_err("one non-qualified gate must fail");
    assert!(error.to_string().contains("Blocked"));
}

fn orchestration_eval_manifest(
    gate_status: sigil_kernel::OrchestrationEvalRouteStatus,
) -> sigil_kernel::OrchestrationEvalReportManifestV1 {
    use sigil_kernel::{OrchestrationEvalRouteGateV1, OrchestrationEvalRouteIdentityV1};
    sigil_kernel::OrchestrationEvalReportManifestV1 {
        report_schema_version: 1,
        campaign_id: "orchestration-test".to_owned(),
        started_at_unix_ms: 1,
        ended_at_unix_ms: 2,
        requested_repetitions: 1,
        results_jsonl_path: PathBuf::from("results.jsonl"),
        summary_path: PathBuf::from("summary.md"),
        route_gates: vec![OrchestrationEvalRouteGateV1 {
            identity: OrchestrationEvalRouteIdentityV1 {
                provider_adapter: "adapter".to_owned(),
                provider_kind: "kind".to_owned(),
                endpoint_family: "family".to_owned(),
                canonical_model_id: "model".to_owned(),
                canonical_model_version: "v1".to_owned(),
                route_fingerprint: "route".to_owned(),
                routing_prompt_digest: "routing".to_owned(),
                planner_prompt_digest: "planner".to_owned(),
                system_prompt_digest: "system".to_owned(),
                tool_profile_contract_digest: "tools".to_owned(),
                task_config_digest: "task-config".to_owned(),
                corpus_version: "corpus-v1".to_owned(),
                corpus_digest: "corpus".to_owned(),
                sigil_commit: "commit".to_owned(),
                sigil_build: "build".to_owned(),
            },
            identity_digest: "gate-1".to_owned(),
            status: gate_status,
            chat_cases: 1,
            plan_review_cases: 1,
            direct_task_cases: 1,
            eligible_chat_cases: 1,
            eligible_plan_review_cases: 1,
            eligible_direct_task_cases: 1,
            provider_admitted_repetitions: 1,
            completed_repetitions: 1,
            chat_to_task_false_positive_rate_ppm: None,
            plan_review_to_task_premature_rate_ppm: None,
            direct_task_miss_rate_ppm: None,
            chat_to_plan_review_overroute_rate_ppm: None,
            plan_review_miss_rate_ppm: None,
            cases_with_majority_misroute: 0,
            cases_with_duplicate_repetition_identity: 0,
            hard_invariant_violations: 0,
            reasons: vec!["".to_owned()],
        }],
    }
}

#[test]
fn r71_release_cost_and_case_preflight_are_fail_closed() {
    assert_eq!(
        parse_model_eval_cost_microusd("0.50").expect("cost"),
        500_000
    );
    assert!(parse_model_eval_cost_microusd("0").is_err());
    assert!(parse_model_eval_cost_microusd("NaN").is_err());
    let root = tempfile::tempdir().expect("tempdir");
    assert!(resolve_model_eval_fixture_roots(root.path(), &["../escape".to_owned()]).is_err());
    assert!(resolve_model_eval_fixture_roots(root.path(), &["/absolute".to_owned()]).is_err());
    assert_eq!(
        resolve_model_eval_fixture_roots(
            root.path(),
            &[
                "small-doc-edit".to_owned(),
                "orchestration/positive/cross-layer".to_owned(),
            ],
        )
        .expect("roots"),
        vec![
            root.path().join("dev/evals/model-fixtures/small-doc-edit"),
            root.path()
                .join("dev/evals/model-fixtures/orchestration/positive/cross-layer"),
        ]
    );
}

#[test]
fn r71_release_frozen_orchestration_selector_expands_exact_corpus() {
    let root = tempfile::tempdir().expect("tempdir");
    let corpus_root = root
        .path()
        .join("dev/evals/model-fixtures/orchestration-v1");
    for (case_class, count) in [("negative", 20), ("positive", 30)] {
        for index in 0..count {
            std::fs::create_dir_all(
                corpus_root
                    .join(case_class)
                    .join(format!("case-{index:02}")),
            )
            .expect("create");
        }
    }
    let cases = resolve_model_eval_fixture_roots(root.path(), &["orchestration-v1".to_owned()])
        .expect("cases");
    assert_eq!(cases.len(), 50);
    assert!(
        cases
            .iter()
            .take(20)
            .all(|path| path.starts_with(corpus_root.join("negative")))
    );
    assert!(
        cases
            .iter()
            .skip(20)
            .all(|path| path.starts_with(corpus_root.join("positive")))
    );

    std::fs::remove_dir_all(corpus_root.join("positive/case-29")).expect("remove");
    let error = resolve_model_eval_fixture_roots(root.path(), &["orchestration-v1".to_owned()])
        .expect_err("an incomplete frozen corpus must fail closed");
    assert!(error.to_string().contains("exactly 50 cases"));
}
