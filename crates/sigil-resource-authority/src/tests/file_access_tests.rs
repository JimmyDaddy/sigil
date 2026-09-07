use super::*;
use sigil_kernel::managed_file_access::{
    ManagedFileAccessAdmissionTokenV1, ManagedFileAccessPlanRequestV1,
    ManagedFileAdmissionBindingV1, ManagedFileExecutionInputV1, ManagedFileExecutionRequestV1,
    ManagedFileLogicalPathV1, ManagedFilePreviewRequestV1, ToolFileAccessAdmissionTokenV1,
};
use sigil_kernel::resource::{AuthorityGeneration, OpaquePermissionSubjectRef};
use std::collections::BTreeSet;

fn hash(seed: u8) -> CanonicalHash {
    CanonicalHash::from_bytes([seed; 32])
}

fn binding() -> ManagedFileAdmissionBindingV1 {
    ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash: hash(1),
        decision_hash: hash(2),
        approval_continuity_hash: hash(3),
        tool_start_event_digest: hash(4),
        file_access_plan_hash: hash(5),
        file_subject_binding_hash: hash(6),
        file_resolver_proof_digest: hash(7),
        file_authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: hash(8),
        },
        workspace_mutation_activation: None,
    }
}

fn subject(
    subject_ref: &str,
    class: BorrowedSubjectClassV1,
    identity: bool,
) -> Arc<Mutex<BorrowedSubjectRegistryV1>> {
    let mut registry = BorrowedSubjectRegistryV1::new();
    let observed = if identity {
        Some(crate::identity::CanonicalLocalIdentity {
            digest: hash(9),
            is_regular_file: false,
            is_directory: true,
            is_symlink: false,
            link_count: 2,
        })
    } else {
        None
    };
    registry
        .observe_with_identity(
            &OpaquePermissionSubjectRef::new(subject_ref.to_owned()),
            class,
            1,
            observed,
        )
        .expect("observe");
    Arc::new(Mutex::new(registry))
}

fn request(
    subject_ref: &str,
    operation: ManagedFileOperationV1,
    op_digest: CanonicalHash,
) -> ManagedFileAccessRequestV1 {
    ManagedFileAccessRequestV1 {
        subject_ref: OpaquePermissionSubjectRef::new(subject_ref.to_owned()),
        operation,
        operation_digest: op_digest,
        admission_binding: binding(),
        admission_binding_hash: hash(10),
    }
}

fn token(op_digest: CanonicalHash) -> ManagedFileAccessAdmissionTokenV1 {
    ManagedFileAccessAdmissionTokenV1::Tool(ToolFileAccessAdmissionTokenV1::qualification_fixture(
        binding(),
        hash(6),
        op_digest,
    ))
}

#[test]
fn r71_fa_workspace_read_adjudicates_with_identity_evidence() {
    let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, true);
    let svc = AuthorityManagedFileAccessServiceV1::new(registry);
    let result = svc
        .access(
            request("ws-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect("adjudicate");
    assert_eq!(
        result.effect_settlement,
        sigil_kernel::recovery::EffectSettlementV1::Applied
    );
    assert_eq!(result.access_receipt.subject_binding_hash, hash(6));
    assert_eq!(result.access_receipt.identity_before, Some(hash(9)));
    assert!(result.access_receipt.identity_after.is_none());
    // Read class has stable tag 1; grant hash is sha256([1]).
    use sha2::Digest as _;
    let mut expected = sha2::Sha256::new();
    expected.update([1u8]);
    assert_eq!(
        result.access_receipt.granted_access_hash,
        CanonicalHash::from_bytes(expected.finalize().into())
    );
}

#[test]
fn r71_fa_claim_is_one_shot() {
    let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
    let svc = AuthorityManagedFileAccessServiceV1::new(registry);
    svc.access(
        request("ws-1", ManagedFileOperationV1::Read, hash(6)),
        token(hash(6)),
    )
    .expect("first");
    let error = svc
        .access(
            request("ws-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect_err("reuse");
    assert!(matches!(error, ManagedFileAccessErrorV1::TokenReplay));
}

#[test]
fn r71_fa_operation_digest_mismatch_refused() {
    let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
    let svc = AuthorityManagedFileAccessServiceV1::new(registry);
    let error = svc
        .access(
            request("ws-1", ManagedFileOperationV1::Read, hash(99)),
            token(hash(6)),
        )
        .expect_err("mismatch");
    assert!(matches!(
        error,
        ManagedFileAccessErrorV1::OperationNotPermitted
    ));
}

#[test]
fn r71_fa_unregistered_subject_refused() {
    let svc = AuthorityManagedFileAccessServiceV1::new(Arc::new(Mutex::new(
        BorrowedSubjectRegistryV1::new(),
    )));
    let error = svc
        .access(
            request("unknown-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect_err("unregistered");
    assert!(matches!(
        error,
        ManagedFileAccessErrorV1::OperationNotPermitted
    ));
}

#[test]
fn r71_fa_system_temp_write_denied_read_allowed() {
    let registry = subject("st-1", BorrowedSubjectClassV1::SystemTemp, false);
    let svc = AuthorityManagedFileAccessServiceV1::new(registry);
    let error = svc
        .access(
            request("st-1", ManagedFileOperationV1::Write, hash(6)),
            token(hash(6)),
        )
        .expect_err("deny write");
    assert!(matches!(
        error,
        ManagedFileAccessErrorV1::OperationNotPermitted
    ));
    // Read at the boundary remains admissible.
    svc.access(
        request("st-1", ManagedFileOperationV1::Read, hash(6)),
        token(hash(6)),
    )
    .expect("read ok");
}

#[test]
fn r71_fa_granted_hash_matches_closed_class() {
    let _ = BTreeSet::<ResourceAccessV1>::new();
    let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
    let svc = AuthorityManagedFileAccessServiceV1::new(registry);
    let result = svc
        .access(
            request("ws-1", ManagedFileOperationV1::Edit, hash(6)),
            token(hash(6)),
        )
        .expect("edit");
    assert!(result.access_receipt.granted_access_hash != hash(0));
}

#[test]
fn managed_file_access_registered_workspace_plans_and_executes_without_path_ref() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("notes.txt"), "alpha\nbeta\n").expect("file");
    let authority_generation = AuthorityGeneration {
        epoch: 7,
        instance_hash: hash(8),
    };
    let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
    registry
        .lock()
        .expect("registry")
        .activate_workspace("app", "workspace", workspace.path(), authority_generation)
        .expect("activate");
    let service = AuthorityManagedFileAccessServiceV1::new(Arc::clone(&registry));
    let plan = service
        .plan(ManagedFileAccessPlanRequestV1 {
            logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
            operation: ManagedFileOperationV1::Read,
            operation_scope: "read-file".to_owned(),
        })
        .expect("plan");
    assert_ne!(plan.authority_generation.epoch, 0);
    assert_ne!(plan.resolver_proof_digest, hash(0));
    assert_ne!(plan.plan_hash, hash(0));
    assert!(!plan.subject_ref.as_str().contains('/'));
    let binding = ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash: hash(1),
        decision_hash: hash(2),
        approval_continuity_hash: hash(3),
        tool_start_event_digest: hash(4),
        file_access_plan_hash: plan.plan_hash,
        file_subject_binding_hash: plan.subject_binding_hash,
        file_resolver_proof_digest: plan.resolver_proof_digest,
        file_authority_generation: plan.authority_generation,
        workspace_mutation_activation: None,
    };
    let preview = service
        .preview(ManagedFilePreviewRequestV1 {
            plan_hash: plan.plan_hash,
            operation: ManagedFileOperationV1::Read,
            max_bytes: 5,
        })
        .expect("preview before one-shot execute");
    assert_eq!(preview.payload, "alpha");
    assert!(preview.truncated);
    let outcome = service
        .execute(
            ManagedFileExecutionRequestV1 {
                access: ManagedFileAccessRequestV1 {
                    subject_ref: plan.subject_ref.clone(),
                    operation: ManagedFileOperationV1::Read,
                    operation_digest: plan.operation_digest,
                    admission_binding: binding.clone(),
                    admission_binding_hash: plan.plan_hash,
                },
                input: ManagedFileExecutionInputV1::Read {
                    offset: 0,
                    limit: 10,
                    max_bytes: 1024,
                },
                mutation_recorder: None,
            },
            ManagedFileAccessAdmissionTokenV1::Tool(
                ToolFileAccessAdmissionTokenV1::qualification_fixture(
                    binding,
                    plan.subject_binding_hash,
                    plan.operation_digest,
                ),
            ),
        )
        .expect("execute");
    assert_eq!(outcome.payload, "alpha\nbeta");
    assert_eq!(outcome.total_lines, 2);
}

#[cfg(unix)]
#[test]
fn r71_f_fil_016_special_file_leaf_is_rejected_without_blocking() {
    let workspace = tempfile::tempdir().expect("workspace");
    let fifo = workspace.path().join("pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    let (service, plan) =
        registered_plan_for_workspace(&workspace, "pipe", ManagedFileOperationV1::Read);
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 128,
        },
    );
    let started = std::time::Instant::now();
    let error = service
        .execute(request, token)
        .expect_err("FIFO must be refused");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "special-file rejection must not wait for a writer"
    );
    assert!(matches!(error, ManagedFileAccessErrorV1::AliasCollision));
}

fn registered_read_plan(
    content: Option<&str>,
) -> (
    tempfile::TempDir,
    AuthorityManagedFileAccessServiceV1,
    sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
) {
    registered_plan_for_operation(content, ManagedFileOperationV1::Read)
}

fn registered_plan_for_operation(
    content: Option<&str>,
    operation: ManagedFileOperationV1,
) -> (
    tempfile::TempDir,
    AuthorityManagedFileAccessServiceV1,
    sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
) {
    let workspace = tempfile::tempdir().expect("workspace");
    if let Some(content) = content {
        std::fs::write(workspace.path().join("notes.txt"), content).expect("file");
    }
    let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
    registry
        .lock()
        .expect("registry")
        .activate_workspace(
            "sigil",
            "fault-file-workspace",
            workspace.path(),
            AuthorityGeneration {
                epoch: 9,
                instance_hash: hash(0x91),
            },
        )
        .expect("activate");
    let service = AuthorityManagedFileAccessServiceV1::new(registry);
    let plan = service
        .plan(ManagedFileAccessPlanRequestV1 {
            logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
            operation,
            operation_scope: format!(
                "fault-file-{}",
                String::from_utf8_lossy(operation_tag(operation))
            ),
        })
        .expect("plan");
    (workspace, service, plan)
}

fn execution_request_for_plan(
    plan: &sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
    input: ManagedFileExecutionInputV1,
) -> (
    ManagedFileExecutionRequestV1,
    ManagedFileAccessAdmissionTokenV1,
) {
    let operation = match &input {
        ManagedFileExecutionInputV1::Read { .. } => ManagedFileOperationV1::Read,
        ManagedFileExecutionInputV1::List { .. } => ManagedFileOperationV1::List,
        ManagedFileExecutionInputV1::Glob { .. } => ManagedFileOperationV1::Glob,
        ManagedFileExecutionInputV1::Grep { .. } => ManagedFileOperationV1::Grep,
        ManagedFileExecutionInputV1::Write { .. } => ManagedFileOperationV1::Write,
        ManagedFileExecutionInputV1::Edit { .. } => ManagedFileOperationV1::Edit,
        ManagedFileExecutionInputV1::Delete => ManagedFileOperationV1::Delete,
    };
    let binding = ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash: hash(0x01),
        decision_hash: hash(0x02),
        approval_continuity_hash: hash(0x03),
        tool_start_event_digest: hash(0x04),
        file_access_plan_hash: plan.plan_hash,
        file_subject_binding_hash: plan.subject_binding_hash,
        file_resolver_proof_digest: plan.resolver_proof_digest,
        file_authority_generation: plan.authority_generation,
        workspace_mutation_activation: None,
    };
    let request = ManagedFileExecutionRequestV1 {
        access: ManagedFileAccessRequestV1 {
            subject_ref: plan.subject_ref.clone(),
            operation,
            operation_digest: plan.operation_digest,
            admission_binding: binding.clone(),
            admission_binding_hash: plan.plan_hash,
        },
        input,
        mutation_recorder: None,
    };
    let token = ManagedFileAccessAdmissionTokenV1::Tool(
        ToolFileAccessAdmissionTokenV1::qualification_fixture(
            binding,
            plan.subject_binding_hash,
            plan.operation_digest,
        ),
    );
    (request, token)
}

#[cfg(any(unix, windows))]
fn registered_plan_for_workspace(
    workspace: &tempfile::TempDir,
    logical_path: &str,
    operation: ManagedFileOperationV1,
) -> (
    AuthorityManagedFileAccessServiceV1,
    sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
) {
    let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
    registry
        .lock()
        .expect("registry")
        .activate_workspace(
            "sigil",
            "fault-file-custom-workspace",
            workspace.path(),
            AuthorityGeneration {
                epoch: 9,
                instance_hash: hash(0x93),
            },
        )
        .expect("activate");
    let service = AuthorityManagedFileAccessServiceV1::new(registry);
    let plan = service
        .plan(ManagedFileAccessPlanRequestV1 {
            logical_path: ManagedFileLogicalPathV1::new(logical_path).expect("logical"),
            operation,
            operation_scope: format!(
                "fault-file-{}",
                String::from_utf8_lossy(operation_tag(operation))
            ),
        })
        .expect("plan");
    (service, plan)
}

#[test]
fn r71_f_fil_001_unregistered_workspace_fails_closed() {
    let service = AuthorityManagedFileAccessServiceV1::new(Arc::new(Mutex::new(
        BorrowedSubjectRegistryV1::new(),
    )));
    let error = service
        .plan(ManagedFileAccessPlanRequestV1 {
            logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
            operation: ManagedFileOperationV1::Read,
            operation_scope: "fault".to_owned(),
        })
        .expect_err("missing registration");
    assert!(matches!(
        error,
        ManagedFileAccessErrorV1::ResourcePreconditionUnavailable
    ));
}

#[test]
fn r71_f_fil_002_absolute_and_traversal_paths_are_rejected() {
    for value in [
        "/etc/passwd",
        "../outside",
        "..\\outside",
        "folder\\..\\outside",
        "C:\\outside",
    ] {
        assert!(matches!(
            ManagedFileLogicalPathV1::new(value),
            Err(ManagedFileAccessErrorV1::AliasCollision)
        ));
    }
}

#[test]
fn r71_f_fil_003_registered_plan_is_pathless_and_nonzero() {
    let (_workspace, _service, plan) = registered_read_plan(Some("ok"));
    assert!(!plan.subject_ref.as_str().contains('/'));
    assert_ne!(plan.authority_generation.epoch, 0);
    assert_ne!(plan.subject_binding_hash, hash(0));
    assert_ne!(plan.resolver_proof_digest, hash(0));
    assert_ne!(plan.plan_hash, hash(0));
}

#[test]
fn r71_f_fil_004_activation_preserves_exact_generation() {
    let (_workspace, _service, plan) = registered_read_plan(Some("ok"));
    assert_eq!(plan.authority_generation.epoch, 9);
    assert_eq!(plan.authority_generation.instance_hash, hash(0x91));
}

#[test]
fn r71_f_fil_005_executor_returns_bounded_receipt() {
    let (_workspace, service, plan) = registered_read_plan(Some("alpha\nbeta\ngamma"));
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 3,
            max_bytes: 6,
        },
    );
    let outcome = service.execute(request, token).expect("execute");
    assert_eq!(outcome.payload, "alpha\n");
    assert!(outcome.truncated);
    assert_ne!(outcome.result_digest, hash(0));
    assert_eq!(
        outcome.effect_settlement,
        sigil_kernel::recovery::EffectSettlementV1::Applied
    );
}

#[test]
fn r71_f_fil_014_mutation_requires_recorder_and_projects_changed_file() {
    let (workspace, service, plan) =
        registered_plan_for_operation(None, ManagedFileOperationV1::Write);
    let (mut request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Write {
            content: "recorded".to_owned(),
        },
    );
    let state = tempfile::tempdir().expect("state");
    let store =
        sigil_kernel::JsonlSessionStore::new(state.path().join("session.jsonl")).expect("store");
    request.mutation_recorder = Some(sigil_kernel::MutationEventRecorder::with_artifact_root(
        store,
        state.path().join("artifacts"),
    ));
    let outcome = service.execute(request, token).expect("recorded write");
    assert_eq!(outcome.changed_files, vec!["notes.txt"]);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("notes.txt")).expect("written file"),
        "recorded"
    );
}

#[test]
fn r71_f_fil_015_mutation_without_recorder_stops_before_effect() {
    let (workspace, service, plan) =
        registered_plan_for_operation(None, ManagedFileOperationV1::Write);
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Write {
            content: "must-not-apply".to_owned(),
        },
    );
    let error = service
        .execute(request, token)
        .expect_err("durability is required");
    assert!(matches!(
        error,
        ManagedFileAccessErrorV1::PhysicalExecutionFailed(message)
            if message.contains("mutation recorder is required")
    ));
    assert!(!workspace.path().join("notes.txt").exists());
}

#[test]
fn r71_f_fil_006_preview_is_bounded_and_side_effect_free() {
    let (_workspace, service, plan) = registered_read_plan(Some("preview-content"));
    let preview = service
        .preview(ManagedFilePreviewRequestV1 {
            plan_hash: plan.plan_hash,
            operation: ManagedFileOperationV1::Read,
            max_bytes: 7,
        })
        .expect("preview");
    assert_eq!(preview.payload, "preview");
    assert!(preview.truncated);
}

#[test]
fn r71_f_fil_013_absent_write_preview_is_an_empty_current_document() {
    let (_workspace, service, plan) =
        registered_plan_for_operation(None, ManagedFileOperationV1::Write);
    let preview = service
        .preview(ManagedFilePreviewRequestV1 {
            plan_hash: plan.plan_hash,
            operation: ManagedFileOperationV1::Write,
            max_bytes: 128,
        })
        .expect("absent write preview");
    assert!(preview.payload.is_empty());
    assert_eq!(preview.observed_bytes, 0);
    assert!(!preview.truncated);
}

#[test]
fn r71_f_fil_007_executor_replay_is_rejected() {
    let (_workspace, service, plan) = registered_read_plan(Some("once"));
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 32,
        },
    );
    service.execute(request, token).expect("first execute");
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 32,
        },
    );
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::PlanStale)
    ));
}

#[test]
fn r71_f_fil_008_cross_operation_binding_is_rejected() {
    let (_workspace, service, plan) = registered_read_plan(Some("read-only"));
    let (mut request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 32,
        },
    );
    request.access.operation = ManagedFileOperationV1::Write;
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::AdmissionMismatch)
    ));
}

#[test]
fn r71_f_fil_009_root_identity_drift_is_rejected() {
    let (workspace, service, plan) = registered_read_plan(Some("drift"));
    let moved = workspace.path().with_extension("moved");
    std::fs::rename(workspace.path(), &moved).expect("move root");
    std::fs::create_dir_all(workspace.path()).expect("replacement root");
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 32,
        },
    );
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::SubjectIdentityDrift)
    ));
}

#[test]
fn r71_f_fil_010_symlink_leaf_is_rejected() {
    #[cfg(unix)]
    {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), "secret").expect("secret");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            workspace.path().join("notes.txt"),
        )
        .expect("symlink");
        let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
        registry
            .lock()
            .expect("registry")
            .activate_workspace(
                "sigil",
                "fault-file-symlink",
                workspace.path(),
                AuthorityGeneration {
                    epoch: 9,
                    instance_hash: hash(0x92),
                },
            )
            .expect("activate");
        let service = AuthorityManagedFileAccessServiceV1::new(registry);
        let plan_error = service
            .plan(ManagedFileAccessPlanRequestV1 {
                logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
                operation: ManagedFileOperationV1::Read,
                operation_scope: "fault-symlink".to_owned(),
            })
            .expect_err("symlink plan must fail closed");
        assert!(matches!(
            plan_error,
            ManagedFileAccessErrorV1::AliasCollision
        ));
    }
    #[cfg(not(unix))]
    assert!(
        true,
        "symlink leaf case is covered by platform-specific authority tests"
    );
}

#[test]
fn managed_file_alias_replacement_cannot_redirect_a_planned_read() {
    #[cfg(unix)]
    {
        let (workspace, service, plan) = registered_read_plan(Some("inside"));
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), "outside-secret").expect("secret");
        std::fs::rename(
            workspace.path().join("notes.txt"),
            workspace.path().join("notes.original"),
        )
        .expect("move planned leaf");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            workspace.path().join("notes.txt"),
        )
        .expect("replace with symlink");
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 64,
            },
        );
        let error = service
            .execute(request, token)
            .expect_err("alias replacement must fail closed");
        assert!(matches!(error, ManagedFileAccessErrorV1::AliasCollision));
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.txt")).expect("secret"),
            "outside-secret"
        );
    }
    #[cfg(not(unix))]
    assert!(
        true,
        "handle-relative alias replacement is covered by Unix authority tests"
    );
}

#[test]
fn managed_file_regular_inode_replacement_is_plan_stale() {
    let (workspace, service, plan) = registered_read_plan(Some("inside"));
    std::fs::rename(
        workspace.path().join("notes.txt"),
        workspace.path().join("notes.original"),
    )
    .expect("move planned leaf");
    std::fs::write(workspace.path().join("notes.txt"), "replacement")
        .expect("replace with a regular file");
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 64,
        },
    );
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::PlanStale)
    ));
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("notes.txt")).expect("replacement"),
        "replacement"
    );
}

#[cfg(unix)]
#[test]
fn managed_file_hard_link_replacement_is_alias_collision() {
    let (workspace, service, plan) = registered_read_plan(Some("inside"));
    let outside = tempfile::tempdir().expect("outside");
    let outside_file = outside.path().join("outside.txt");
    std::fs::write(&outside_file, "outside-secret").expect("outside file");
    std::fs::rename(
        workspace.path().join("notes.txt"),
        workspace.path().join("notes.original"),
    )
    .expect("move planned leaf");
    std::fs::hard_link(&outside_file, workspace.path().join("notes.txt"))
        .expect("replace with a hard link");
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 64,
        },
    );
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::AliasCollision)
    ));
    assert_eq!(
        std::fs::read_to_string(outside_file).expect("outside secret"),
        "outside-secret"
    );
}

#[test]
fn managed_file_absent_to_present_transition_is_plan_stale() {
    let (workspace, service, plan) = registered_read_plan(None);
    std::fs::write(workspace.path().join("notes.txt"), "created-after-plan")
        .expect("create planned leaf after approval");
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 64,
        },
    );
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::PlanStale)
    ));
}

#[cfg(any(unix, windows))]
#[test]
fn windows_nested_directory_tools_open_any_leaf_kind() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join("nested/deeper")).expect("directories");
    std::fs::write(workspace.path().join("root.txt"), "needle at root").expect("root file");
    std::fs::write(
        workspace.path().join("nested/match.txt"),
        "needle in nested",
    )
    .expect("nested file");
    std::fs::write(
        workspace.path().join("nested/deeper/deep.txt"),
        "needle in deeper",
    )
    .expect("deep file");

    let (service, list_plan) =
        registered_plan_for_workspace(&workspace, ".", ManagedFileOperationV1::List);
    let (request, token) = execution_request_for_plan(
        &list_plan,
        ManagedFileExecutionInputV1::List {
            recursive: true,
            limit: 20,
            max_depth: 8,
        },
    );
    let list = service.execute(request, token).expect("recursive list");
    assert!(list.payload.contains("nested"));
    assert!(list.payload.contains("nested/deeper/deep.txt"));

    let (service, glob_plan) =
        registered_plan_for_workspace(&workspace, ".", ManagedFileOperationV1::Glob);
    let (request, token) = execution_request_for_plan(
        &glob_plan,
        ManagedFileExecutionInputV1::Glob {
            pattern: "*.txt".to_owned(),
            limit: 20,
        },
    );
    let glob = service.execute(request, token).expect("recursive glob");
    assert!(glob.payload.contains("nested/match.txt"));
    assert!(glob.payload.contains("nested/deeper/deep.txt"));

    let (service, grep_plan) =
        registered_plan_for_workspace(&workspace, ".", ManagedFileOperationV1::Grep);
    let (request, token) = execution_request_for_plan(
        &grep_plan,
        ManagedFileExecutionInputV1::Grep {
            pattern: "needle".to_owned(),
            limit: 20,
            max_bytes: 4096,
        },
    );
    let grep = service.execute(request, token).expect("recursive grep");
    assert!(grep.payload.contains("nested/match.txt"));
    assert!(grep.payload.contains("nested/deeper/deep.txt"));
}

#[test]
fn r71_f_fil_011_stale_preview_is_rejected() {
    let (_workspace, service, _plan) = registered_read_plan(Some("stale"));
    assert!(matches!(
        service.preview(ManagedFilePreviewRequestV1 {
            plan_hash: hash(0xee),
            operation: ManagedFileOperationV1::Read,
            max_bytes: 32,
        }),
        Err(ManagedFileAccessErrorV1::PlanStale)
    ));
}

#[test]
fn r71_f_fil_012_missing_leaf_is_typed_physical_failure() {
    let (_workspace, service, plan) = registered_read_plan(None);
    let (request, token) = execution_request_for_plan(
        &plan,
        ManagedFileExecutionInputV1::Read {
            offset: 0,
            limit: 1,
            max_bytes: 32,
        },
    );
    assert!(matches!(
        service.execute(request, token),
        Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(_))
    ));
}
