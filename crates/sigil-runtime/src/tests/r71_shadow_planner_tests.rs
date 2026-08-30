use super::*;
use sigil_kernel::managed_execution::ExecutionCapturePolicy;

fn sample_request() -> ManagedExecutionPlanRequestV1 {
    ManagedExecutionPlanRequestV1 {
        argv: vec!["sh".into(), "-c".into(), "true".into()],
        cwd_subject_ref: sigil_kernel::resource::OpaquePermissionSubjectRef::new(
            "workspace:cwd".to_owned(),
        ),
        purpose: ExecutionPurposeV1::OneShot,
        structured_command_digest: canonical_digest(b"sh -c true"),
        owner_scope: ResourceOwnerScopeV1::Application,
        capture: ExecutionCapturePolicy {
            stdout_capture: sigil_kernel::managed_execution::CaptureModeV1::BoundedRing {
                max_bytes: 64 * 1024,
            },
            stderr_capture: sigil_kernel::managed_execution::CaptureModeV1::BoundedRing {
                max_bytes: 64 * 1024,
            },
            pty: false,
        },
        environment: Vec::new(),
        limits: sigil_kernel::managed_execution::ExecutionResourceLimits {
            max_output_bytes: 128 * 1024,
            max_runtime_ms: 30_000,
            max_children: 4,
            max_fds: 256,
            pty_required: false,
        },
    }
}

#[test]
fn r71_shadow_planner_is_side_effect_free_and_stable() {
    let planner = ShadowPlannerV1::new(ShadowPlannerConfigV1::default());
    let first = planner
        .plan_execution(sample_request())
        .expect("shadow plan");
    let second = planner
        .plan_execution(sample_request())
        .expect("shadow plan");
    assert_eq!(
        first.draft_hash, second.draft_hash,
        "hash must be deterministic"
    );
    assert_eq!(first.argv_digest, second.argv_digest);
    assert_eq!(first.attempt_journal_scope, second.attempt_journal_scope);
}

#[test]
fn r71_shadow_planner_requirement_hash_changes_with_capture_product() {
    let planner = ShadowPlannerV1::new(ShadowPlannerConfigV1 {
        capture_product_defaults: true,
        ..ShadowPlannerConfigV1::default()
    });
    let with_defaults = planner.plan_execution(sample_request()).expect("plan");
    let no_defaults = ShadowPlannerV1::new(ShadowPlannerConfigV1 {
        capture_product_defaults: false,
        ..ShadowPlannerConfigV1::default()
    })
    .plan_execution(sample_request())
    .expect("plan");
    assert_ne!(
        with_defaults.capture_policy_hash,
        no_defaults.capture_policy_hash
    );
}
