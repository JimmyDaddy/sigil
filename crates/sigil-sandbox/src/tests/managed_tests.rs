use super::*;
use sigil_kernel::managed_execution::{
    CaptureModeV1, ExecutionCapturePolicy, ManagedExecutionPlanErrorV1,
};
use sigil_kernel::managed_execution::{EnvironmentProfileRefV1, ExecutionResourceLimits};
use sigil_kernel::resource::{
    BoundedVec, OpaqueAdmissionId, OpaqueExecutionPlanDraftId, OpaquePermissionSubjectRef,
    OpaqueRequirementId, OpaqueSessionId, ResourceBlockerScopeV1, ResourceCleanupPolicyV1,
    ResourceLeaseLifetimeV1, ResourcePurposeV1, ResourceQuotaClassV1, ResourceQuotaProfileV1,
    ResourceRequirementKeyV1, ResourceRequirementSetV1, ResourceRequirementV1,
    ResourceRetentionPolicyV1, ResourceVisibilityV1,
};
use std::ffi::OsString;

fn cap() -> ExecutionCapturePolicy {
    ExecutionCapturePolicy {
        stdout_capture: CaptureModeV1::BoundedRing { max_bytes: 4096 },
        stderr_capture: CaptureModeV1::BoundedRing { max_bytes: 4096 },
        pty: false,
    }
}

fn limits() -> ExecutionResourceLimits {
    ExecutionResourceLimits {
        max_output_bytes: 4096,
        max_runtime_ms: 15_000,
        max_children: 1,
        max_fds: 16,
        pty_required: false,
    }
}

fn quota() -> ResourceQuotaProfileV1 {
    ResourceQuotaProfileV1 {
        class: ResourceQuotaClassV1::AttemptEphemeral,
        max_bytes: 1024 * 1024,
        max_entries: 1024,
        max_open_holders: 1,
        max_age_ms: None,
        hard_runtime_enforcement_required: false,
        profile_hash: zero_hash(),
    }
}

fn draft_hash_for(argv: &[&str], isolation: bool) -> CanonicalHash {
    let mut acc = b"test-plan-v1".to_vec();
    for arg in argv {
        acc.extend_from_slice(arg.as_bytes());
        acc.push(0);
    }
    acc.push(if isolation { b'i' } else { b'u' });
    content_digest(&acc)
}

struct TestPlannerV1 {
    isolation: bool,
}

impl ManagedExecutionPlannerV1 for TestPlannerV1 {
    fn plan_execution(
        &self,
        request: ManagedExecutionPlanRequestV1,
    ) -> Result<ManagedExecutionPlanDraftV1, ManagedExecutionPlanErrorV1> {
        let argv: Vec<String> = request
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let draft_hash = draft_hash_for(&argv_refs, self.isolation);
        let profile_class = if self.isolation {
            EnvironmentProfileClassV1::FreshIsolatedHome
        } else {
            EnvironmentProfileClassV1::ExplicitUnconfined
        };
        let access =
            std::collections::BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]);
        let requirement = ResourceRequirementV1 {
            requirement_id: OpaqueRequirementId::new("req-1".to_owned()),
            physical_owner_scope: ResourceOwnerScopeV1::Application,
            stable_key: ResourceRequirementKeyV1 {
                blocker_scope: ResourceBlockerScopeV1::Session(OpaqueSessionId::new(
                    "s-1".to_owned(),
                )),
                kind: ResourceKindV1::ExecutionTemp,
                purpose: ResourcePurposeV1::ExecutionPrerequisite,
                access: access.clone(),
                lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
                quota_profile: quota(),
                retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
                cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
                environment_class: profile_class,
                toolchain_class: None,
                subject_binding_hash: None,
                canonical_hash: zero_hash(),
            },
            kind: ResourceKindV1::ExecutionTemp,
            lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
            access,
            purpose: ResourcePurposeV1::ExecutionPrerequisite,
            visibility: ResourceVisibilityV1::HostOnly,
            quota_profile: quota(),
            retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
            cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
            implicit: false,
        };
        Ok(ManagedExecutionPlanDraftV1 {
            draft_id: OpaqueExecutionPlanDraftId::new("d-1".to_owned()),
            argv_digest: draft_hash,
            structured_command_digest: request.structured_command_digest,
            cwd_subject_binding_hash: zero_hash(),
            attempt_journal_scope: ResourceJournalScopeV1::Application,
            attempt_journal_scope_hash: zero_hash(),
            resource_plan_hash: zero_hash(),
            resource_requirements: ResourceRequirementSetV1 {
                schema_version: 1,
                requirements: BoundedVec::try_from_vec(vec![requirement]).expect("bounded"),
                canonical_hash: zero_hash(),
            },
            environment_profile: EnvironmentProfileRefV1 {
                profile_class,
                profile_hash: zero_hash(),
            },
            toolchain_plan_hash: zero_hash(),
            resolver_proof_digest: zero_hash(),
            sandbox_preview_hash: zero_hash(),
            sandbox_binder_registration_hash: zero_hash(),
            sandbox_provider_generation: 1,
            capture_policy_hash: zero_hash(),
            resource_limits_hash: zero_hash(),
            environment_digest: sigil_kernel::managed_execution::canonical_environment_digest(
                &request.environment,
            ),
            draft_hash,
        })
    }
}

fn exec_request(argv: &[&str], isolation: bool) -> ManagedExecutionRequestV1 {
    let argv_owned: Vec<OsString> = argv.iter().map(OsString::from).collect();
    let draft_hash = draft_hash_for(argv, isolation);
    ManagedExecutionRequestV1 {
        argv: argv_owned,
        cwd_subject_ref: OpaquePermissionSubjectRef::new("subj-1".to_owned()),
        structured_command_digest: zero_hash(),
        admission_ref: OpaqueAdmissionId::new("adm-1".to_owned()),
        execution_plan_draft_hash: draft_hash,
        environment_profile: EnvironmentProfileRefV1 {
            profile_class: if isolation {
                EnvironmentProfileClassV1::FreshIsolatedHome
            } else {
                EnvironmentProfileClassV1::ExplicitUnconfined
            },
            profile_hash: zero_hash(),
        },
        capture: cap(),
        limits: limits(),
        pty_size: None,
        environment: Vec::new(),
    }
}

fn bundle(kind: &str) -> IssuedExecutionAdmissionBundleV1 {
    match kind {
        "one-shot" => IssuedExecutionAdmissionBundleV1::OneShot {
            consumer_token: sigil_kernel::resource::OpaqueResourceId::new("tok".to_owned()),
            resource_capability: sigil_kernel::resource::OpaqueResourceId::new("cap".to_owned()),
        },
        "terminal" => IssuedExecutionAdmissionBundleV1::Terminal {
            consumer_token: sigil_kernel::resource::OpaqueResourceId::new("tok".to_owned()),
            resource_capability: sigil_kernel::resource::OpaqueResourceId::new("cap".to_owned()),
        },
        _ => IssuedExecutionAdmissionBundleV1::Extension {
            consumer_token: sigil_kernel::resource::OpaqueResourceId::new("tok".to_owned()),
            resource_capability: sigil_kernel::resource::OpaqueResourceId::new("cap".to_owned()),
        },
    }
}

fn service(isolation: bool, root: &std::path::Path) -> SandboxManagedExecutionServiceV1 {
    SandboxManagedExecutionServiceV1::new(Arc::new(TestPlannerV1 { isolation }), root.to_path_buf())
        .with_process_inventory(Arc::new(
            sigil_resource_authority::InMemoryAuthorityProcessInventoryV1::default(),
        ))
        .with_one_shot_launcher(Arc::new(CommandManagedOneShotLaunchServiceV1::new(
            root.to_path_buf(),
            OpaquePermissionSubjectRef::new("subj-1".to_owned()),
        )))
}

fn terminal_service(
    root: &std::path::Path,
    args: Vec<OsString>,
) -> SandboxManagedExecutionServiceV1 {
    let enforcement = ManagedExtensionLaunchEnforcementV1 {
        requested: RequestedEnforcementV1 {
            requirement: EnforcementRequirementClassV1::ExplicitUnconfined,
            deny_ambient_system_temp_write: false,
            deny_ambient_home_write: false,
            deny_ungranted_workspace_write: false,
            require_process_tree_ownership: false,
            require_network_policy: false,
            requested_capability_set_hash: zero_hash(),
            profile_hash: zero_hash(),
        },
        backend: SandboxBackendClassV1::LocalUnconfined,
        completeness: EnforcementCompletenessV1::None,
        effective_access: std::collections::BTreeSet::new(),
        effective_capability_set_hash: zero_hash(),
        proof_set_hash: zero_hash(),
    };
    service(false, root).with_terminal_launcher(Arc::new(
        CommandManagedTerminalLaunchServiceV1::new(
            PathBuf::from("/bin/sh"),
            args,
            root.to_path_buf(),
            Vec::new(),
            enforcement,
        ),
    ))
}

#[test]
fn r71_managed_execute_once_yields_truthful_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let receipt = futures::executor::block_on(svc.execute_once(
        bundle("one-shot"),
        exec_request(&["/bin/sh", "-c", "printf hello-world"], false),
    ))
    .expect("execute");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
    assert_eq!(receipt.process.stdout_summary.retained_bytes, 11);
    assert!(!receipt.process.stdout_summary.truncated);
    assert_eq!(receipt.process.stdout_summary.observed_bytes, 11);
    // Local is truthful none, never a fabricated subset.
    assert_eq!(
        receipt.resources.effective_enforcement.completeness,
        EnforcementCompletenessV1::None
    );
    assert_eq!(
        receipt.resources.cleanup_status,
        ResourceCleanupStatusV1::Released
    );
    assert_eq!(
        receipt.resources.resources[0].enforcement,
        EnforcementCompletenessV1::None
    );
}

#[test]
fn r71_managed_required_isolation_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(true, dir.path());
    let error = futures::executor::block_on(svc.execute_once(
        bundle("one-shot"),
        exec_request(&["/bin/sh", "-c", "exit 0"], true),
    ))
    .expect_err("isolation required");
    assert!(matches!(
        error,
        ManagedExecutionErrorV1::ConfinementUnproven
    ));
}

#[test]
fn r71_managed_wrong_bundle_purpose_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let error = futures::executor::block_on(svc.execute_once(
        bundle("terminal"),
        exec_request(&["/bin/sh", "-c", "exit 0"], false),
    ))
    .expect_err("purpose mismatch");
    assert!(matches!(error, ManagedExecutionErrorV1::AdmissionMismatch));
}

#[test]
fn r71_managed_draft_drift_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut request = exec_request(&["/bin/sh", "-c", "exit 0"], false);
    request.execution_plan_draft_hash = zero_hash();
    let error = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect_err("drift");
    assert!(matches!(error, ManagedExecutionErrorV1::ExecutionPlanDrift));
}

#[test]
fn r71_managed_output_cap_truncates_truthfully() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut request = exec_request(&["/bin/sh", "-c", "printf '%0100d' 7"], false);
    request.limits.max_output_bytes = 10;
    request.capture.stdout_capture = CaptureModeV1::BoundedRing { max_bytes: 10 };
    let receipt = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect("execute");
    assert!(receipt.process.stdout_summary.truncated);
    assert_eq!(receipt.process.stdout_summary.observed_bytes, 100);
    assert_eq!(receipt.process.stdout_summary.retained_bytes, 10);
}

#[test]
fn r71_managed_pty_required_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut request = exec_request(&["/bin/sh", "-c", "exit 0"], false);
    request.limits.pty_required = true;
    let error = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect_err("pty");
    assert!(matches!(
        error,
        ManagedExecutionErrorV1::ProviderUnavailable
    ));
}

#[test]
fn r71_managed_persistent_pty_supports_input_resize_and_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = terminal_service(
        dir.path(),
        vec![
            OsString::from("-c"),
            OsString::from("read line; printf '%s\\n' \"$line\""),
        ],
    );
    let mut request = exec_request(
        &["/bin/sh", "-c", "read line; printf '%s\\n' \"$line\""],
        false,
    );
    request.capture.pty = true;
    request.limits.pty_required = true;
    request.pty_size = Some(BoundedPtySizeV1 { rows: 24, cols: 80 });
    let mut handle = futures::executor::block_on(svc.start_persistent(bundle("terminal"), request))
        .expect("pty spawn");
    futures::executor::block_on(handle.resize_pty(BoundedPtySizeV1 {
        rows: 40,
        cols: 120,
    }))
    .expect("resize");
    let mut stream = handle.take_output_stream().expect("stream");
    futures::executor::block_on(handle.write_stdin(BoundedProcessInputV1 {
        payload: b"managed-pty\n".to_vec(),
    }))
    .expect("write");
    futures::executor::block_on(handle.close_stdin()).expect("close");
    let mut output = Vec::new();
    while let Some(frame) = futures::executor::block_on(stream.next_frame()).expect("frame") {
        if !frame.end_of_stream {
            output.extend(frame.payload);
        }
    }
    let receipt = futures::executor::block_on(handle.wait_and_finalize()).expect("receipt");
    assert!(
        output
            .windows(b"managed-pty".len())
            .any(|window| window == b"managed-pty")
    );
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
    assert_eq!(
        receipt.resources.cleanup_status,
        ResourceCleanupStatusV1::Released
    );
}

#[test]
fn r71_managed_persistent_pty_cancel_reaps_owned_process_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = terminal_service(
        dir.path(),
        vec![OsString::from("-c"), OsString::from("sleep 30")],
    );
    let mut request = exec_request(&["/bin/sh", "-c", "sleep 30"], false);
    request.capture.pty = true;
    request.limits.pty_required = true;
    request.pty_size = Some(BoundedPtySizeV1 { rows: 24, cols: 80 });
    let mut handle = futures::executor::block_on(svc.start_persistent(bundle("terminal"), request))
        .expect("pty spawn");
    futures::executor::block_on(handle.cancel(ProcessCancelReasonV1::UserCancelled))
        .expect("cancel");
    let receipt = futures::executor::block_on(handle.wait_and_finalize()).expect("receipt");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Cancelled
    ));
    assert_eq!(
        receipt.resources.cleanup_status,
        ResourceCleanupStatusV1::Released
    );
}

#[test]
fn r71_managed_persistent_non_pty_cancel_has_cancelled_terminal_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut handle = futures::executor::block_on(svc.start_persistent(
        bundle("terminal"),
        exec_request(&["/bin/sh", "-c", "sleep 30"], false),
    ))
    .expect("spawn");
    futures::executor::block_on(handle.cancel(ProcessCancelReasonV1::UserCancelled))
        .expect("cancel");
    let receipt = futures::executor::block_on(handle.wait_and_finalize()).expect("receipt");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Cancelled
    ));
    assert_eq!(
        receipt.resources.cleanup_status,
        ResourceCleanupStatusV1::Released
    );
}

#[test]
fn r71_managed_persistent_stdin_echo_finalizes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut handle = futures::executor::block_on(svc.start_persistent(
        bundle("terminal"),
        exec_request(&["/bin/sh", "-c", "cat"], false),
    ))
    .expect("spawn");
    futures::executor::block_on(handle.write_stdin(BoundedProcessInputV1 {
        payload: b"ping\n".to_vec(),
    }))
    .expect("write");
    futures::executor::block_on(handle.close_stdin()).expect("close");
    let receipt = futures::executor::block_on(handle.wait_and_finalize()).expect("finalize");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
    // The drain thread retained the echoed bytes under the cap.
    assert_eq!(receipt.process.stdout_summary.retained_bytes, 5);
    assert!(!receipt.process.stdout_summary.truncated);
}

#[test]
fn r71_managed_extension_uses_injected_real_command_launcher() {
    let dir = tempfile::tempdir().expect("tempdir");
    let enforcement = ManagedExtensionLaunchEnforcementV1 {
        requested: RequestedEnforcementV1 {
            requirement: EnforcementRequirementClassV1::ExplicitUnconfined,
            deny_ambient_system_temp_write: false,
            deny_ambient_home_write: false,
            deny_ungranted_workspace_write: false,
            require_process_tree_ownership: false,
            require_network_policy: false,
            requested_capability_set_hash: zero_hash(),
            profile_hash: zero_hash(),
        },
        backend: SandboxBackendClassV1::LocalUnconfined,
        completeness: EnforcementCompletenessV1::None,
        effective_access: std::collections::BTreeSet::new(),
        effective_capability_set_hash: zero_hash(),
        proof_set_hash: zero_hash(),
    };
    let launcher = Arc::new(CommandManagedExtensionLaunchServiceV1::new(
        PathBuf::from("/bin/sh"),
        vec![OsString::from("-c"), OsString::from("printf extension")],
        std::env::current_dir().expect("cwd"),
        Vec::new(),
        enforcement,
    ));
    let svc = service(false, dir.path()).with_extension_launcher(launcher);
    let mut handle = futures::executor::block_on(svc.start_persistent(
        bundle("extension"),
        exec_request(&["/bin/sh", "-c", "printf extension"], false),
    ))
    .expect("extension spawn");
    let mut stream = handle.take_output_stream().expect("stream");
    let mut output = Vec::new();
    while let Some(frame) = futures::executor::block_on(stream.next_frame()).expect("frame") {
        if !frame.end_of_stream {
            output.extend(frame.payload);
        }
    }
    let receipt = futures::executor::block_on(handle.wait_and_finalize()).expect("finalize");
    assert_eq!(output, b"extension");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
}

#[test]
fn r71_managed_stream_is_single_flight_with_exact_one_eof() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut handle = futures::executor::block_on(svc.start_persistent(
        bundle("terminal"),
        exec_request(&["/bin/sh", "-c", "printf stream-out"], false),
    ))
    .expect("spawn");
    let mut stream = handle.take_output_stream().expect("stream");
    // Second take is refused (single-flight).
    let second_take = handle.take_output_stream();
    assert!(matches!(
        second_take,
        Err(ManagedProcessControlErrorV1::StreamAlreadyTaken)
    ));
    let mut eof_count: i32 = 0;
    let mut payload_seen = 0usize;
    while let Some(frame) = futures::executor::block_on(stream.next_frame()).expect("frame") {
        if frame.end_of_stream {
            eof_count += 1;
        } else {
            payload_seen += frame.payload.len();
        }
    }
    // Stash the stream back so finalize drains coherently.
    drop(stream);
    let receipt = futures::executor::block_on(handle.wait_and_finalize()).expect("finalize");
    assert_eq!(payload_seen, 10);
    assert!(eof_count >= 2, "mock: stdout+stderr EOF frames");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
}

#[test]
fn r71_managed_environment_planner_sealed_and_reaches_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let mut request = exec_request(
        &["/bin/sh", "-c", "test \"$R71_ENV_SEAL\" = \"yes\""],
        false,
    );
    request.environment = vec![(OsString::from("R71_ENV_SEAL"), OsString::from("yes"))];
    let receipt = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect("agreed environment must execute");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
}

#[test]
fn r71_reserved_execution_temp_environment_cannot_be_redirected_by_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("tmp")).expect("managed tmp");
    let expected_tmp = dir.path().join("tmp");
    let svc = service(false, dir.path());
    let mut request = exec_request(
        &[
            "/bin/sh",
            "-c",
            "test \"$TMPDIR\" = \"$EXPECTED_MANAGED_TMP\"",
        ],
        false,
    );
    request.environment = vec![
        (
            OsString::from("TMPDIR"),
            OsString::from("/ambient-override"),
        ),
        (
            OsString::from("EXPECTED_MANAGED_TMP"),
            expected_tmp.into_os_string(),
        ),
    ];
    let receipt = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect("reserved environment must execute");
    assert!(matches!(
        receipt.process.termination,
        ProcessTerminationV1::Exited { code: 0 }
    ));
}

#[test]
fn r71_environment_digest_is_order_independent_and_exact() {
    use sigil_kernel::managed_execution::canonical_environment_digest;
    let a = vec![
        (OsString::from("A"), OsString::from("1")),
        (OsString::from("B"), OsString::from("2")),
    ];
    let b = vec![
        (OsString::from("B"), OsString::from("2")),
        (OsString::from("A"), OsString::from("1")),
    ];
    let c = vec![
        (OsString::from("A"), OsString::from("2")),
        (OsString::from("B"), OsString::from("1")),
    ];
    assert_eq!(
        canonical_environment_digest(&a),
        canonical_environment_digest(&b)
    );
    assert_ne!(
        canonical_environment_digest(&a),
        canonical_environment_digest(&c)
    );
}

#[test]
fn r71_managed_over_bound_agreement_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = service(false, dir.path());
    let too_few_envs = {
        let mut request = exec_request(&["/bin/sh", "-c", "exit 0"], false);
        request.environment = Vec::new();
        request
    };
    // argv over the closed entry bound: fail closed, never partial execution.
    let mut argv = Vec::new();
    for index in 0..=sigil_kernel::managed_execution::MAX_MANAGED_EXECUTION_ARGV_ENTRIES {
        argv.push(OsString::from(format!("arg-{index}")));
    }
    let mut request = exec_request(&["/bin/sh", "-c", "exit 0"], false);
    request.argv = argv;
    let error = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect_err("over-bound argv must refuse");
    assert!(matches!(error, ManagedExecutionErrorV1::AdmissionMismatch));
    // env over the closed entry bound: fail closed.
    let mut request = exec_request(&["/bin/sh", "-c", "exit 0"], false);
    request.environment = (0..=sigil_kernel::managed_execution::MAX_MANAGED_EXECUTION_ENV_ENTRIES)
        .map(|index| {
            (
                OsString::from(format!("K{index}")),
                OsString::from(index.to_string()),
            )
        })
        .collect();
    let error = futures::executor::block_on(svc.execute_once(bundle("one-shot"), request))
        .expect_err("over-bound env must refuse");
    assert!(matches!(error, ManagedExecutionErrorV1::AdmissionMismatch));
    let _ = too_few_envs;
}
