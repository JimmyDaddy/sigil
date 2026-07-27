use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use super::*;

const ACCEPTED_PLAN: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/accepted-plan.json");
const CONTRACT_CASES: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/contract-cases.json");
const EVENT_TYPES: &str = include_str!("../../../../dev/fixtures/intent-stack-v1/event-types.json");
const LOCKED_DECISIONS: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/locked-decisions.json");
const OPERATION_ERRORS: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/operation-errors.json");
const PROPOSAL: &str = include_str!("../../../../dev/fixtures/intent-stack-v1/proposal.json");
const USER_ROOT_PLAN: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/user-root-plan.json");
const SHARED_FILE_ARTIFACTS: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/shared-file-artifacts.json");
const DELETED_ARTIFACT: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/deleted-artifact.json");
const AUTHORITY_BOUNDARIES: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/authority-boundaries.json");
const DIGEST_GRAPH: &str =
    include_str!("../../../../dev/fixtures/intent-stack-v1/digest-graph.json");
const ENUM_TOKENS: &str = include_str!("../../../../dev/fixtures/intent-stack-v1/enum-tokens.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractCases {
    plan_accepted: Value,
    task_execution_bound: Value,
    chat_execution_bound: Value,
    file_hunk_artifact: Value,
    shared_artifact_conflict: Value,
    verification_linked: Value,
    read_only_public_stack: Value,
    artifact_unavailable_conflict: Value,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LockedDecision {
    decision: String,
    fixture: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityBoundaries {
    resume: IntentAuthorityState,
    fork: IntentAuthorityState,
    worktree_child: IntentAuthorityState,
    workspace_switch: IntentAuthorityState,
    old_session: PublicIntentStackStateV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DigestGraph {
    layer_core: IntentLayerCoreV1,
    artifact_manifest: IntentArtifactManifestV1,
    layer_manifest: IntentLayerManifestV1,
    operation_preview: IntentOperationPreviewV1,
}

fn digest(hex: char) -> Result<IntentDigest> {
    IntentDigest::new(format!(
        "{INTENT_CANONICAL_DIGEST_PREFIX}{}",
        hex.to_string().repeat(64)
    ))
}

fn intent_ref(id: &str) -> Result<IntentVersionRef> {
    IntentVersionRef::new(IntentId::new(id)?, 1)
}

#[test]
fn accepted_plan_fixture_locks_canonical_digest() -> Result<()> {
    let plan: IntentPlanV1 = serde_json::from_str(ACCEPTED_PLAN)?;
    assert_eq!(
        plan.computed_digest()?.as_str(),
        "sha256:jcs-v1:4f2919a904a0a10b566ba43d8f9800f4b9a63f211237d7295b8c9ca225e776b1",
        "update the committed digest only after reviewing an intentional contract change"
    );
    plan.validate_contract()?;
    Ok(())
}

#[test]
fn proposal_and_user_root_fixtures_freeze_pre_acceptance_boundary() -> Result<()> {
    let proposal: IntentPlanProposalV1 = serde_json::from_str(PROPOSAL)?;
    assert_eq!(
        proposal.computed_digest()?.as_str(),
        "sha256:jcs-v1:3a789c36cf41af813e17b47617567e5801da7b7eeb33cab700c480ebbae98984",
        "update the committed proposal digest only after intentional contract review"
    );
    proposal.validate_contract()?;

    let root: IntentPlanV1 = serde_json::from_str(USER_ROOT_PLAN)?;
    assert_eq!(
        root.computed_digest()?.as_str(),
        "sha256:jcs-v1:73a183f674dada905ef5ff796286d4480a865ca8e4eb8d1c5706534ea2fe35a9",
        "update the committed root-plan digest only after intentional contract review"
    );
    root.validate_contract()?;

    let proposal_json = serde_json::to_value(&proposal)?;
    assert!(proposal_json.pointer("/intents/0/intent_alias").is_some());
    assert!(proposal_json.pointer("/intents/0/intent_ref").is_none());
    assert!(proposal_json.get("acceptance_kind").is_none());
    assert!(proposal_json.get("stack_id").is_none());
    assert!(matches!(root.kind, IntentPlanKind::UserDeclaredRoot));
    Ok(())
}

#[test]
fn canonical_digest_sorts_object_keys_but_preserves_semantic_array_order() -> Result<()> {
    let left = json!({"z": 1, "nested": {"b": true, "a": false}, "items": ["a", "b"]});
    let right = json!({"items": ["a", "b"], "nested": {"a": false, "b": true}, "z": 1});
    let reordered = json!({"z": 1, "nested": {"b": true, "a": false}, "items": ["b", "a"]});

    assert_eq!(
        canonical_intent_digest(IntentDigestDomain::Plan, &left)?,
        canonical_intent_digest(IntentDigestDomain::Plan, &right)?
    );
    assert_ne!(
        canonical_intent_digest(IntentDigestDomain::Plan, &left)?,
        canonical_intent_digest(IntentDigestDomain::Plan, &reordered)?
    );
    assert_ne!(
        canonical_intent_digest(IntentDigestDomain::Plan, &left)?,
        canonical_intent_digest(IntentDigestDomain::Proposal, &left)?
    );
    assert_ne!(
        canonical_intent_digest(IntentDigestDomain::LayerCore, &left)?,
        canonical_intent_digest(IntentDigestDomain::LayerManifest, &left)?
    );
    Ok(())
}

#[test]
fn cycle_free_layer_artifact_and_preview_digests_are_golden() -> Result<()> {
    let graph: DigestGraph = serde_json::from_str(DIGEST_GRAPH)?;
    assert_eq!(
        graph.layer_core.computed_digest()?,
        graph.layer_core.core_digest
    );
    graph.layer_core.validate_contract()?;
    assert_eq!(
        graph.artifact_manifest.layer_core_digest,
        graph.layer_core.core_digest
    );
    assert_eq!(
        graph.artifact_manifest.computed_digest()?,
        graph.artifact_manifest.manifest_digest
    );
    graph.artifact_manifest.validate_contract()?;
    assert_eq!(graph.layer_manifest.core, graph.layer_core);
    assert_eq!(
        graph.layer_manifest.artifact_manifest_digest,
        graph.artifact_manifest.manifest_digest
    );
    assert_eq!(
        graph.layer_manifest.computed_digest()?,
        graph.layer_manifest.manifest_digest
    );
    graph.layer_manifest.validate_contract()?;
    assert_eq!(
        graph.operation_preview.computed_digest()?,
        graph.operation_preview.preview_digest
    );
    graph.operation_preview.validate_contract()?;
    Ok(())
}

#[test]
fn locked_decision_fixtures_round_trip_through_strict_contracts() -> Result<()> {
    let cases: ContractCases = serde_json::from_str(CONTRACT_CASES)?;

    let accepted: IntentEventV1 = serde_json::from_value(cases.plan_accepted)?;
    let task: IntentEventV1 = serde_json::from_value(cases.task_execution_bound)?;
    let chat: IntentEventV1 = serde_json::from_value(cases.chat_execution_bound)?;
    let artifact: IntentArtifactBindingV1 = serde_json::from_value(cases.file_hunk_artifact)?;
    let shared: IntentConflictV1 = serde_json::from_value(cases.shared_artifact_conflict)?;
    let verification: IntentEventV1 = serde_json::from_value(cases.verification_linked)?;
    let read_only: PublicIntentStackV1 = serde_json::from_value(cases.read_only_public_stack)?;
    let unavailable: IntentConflictV1 =
        serde_json::from_value(cases.artifact_unavailable_conflict)?;

    assert_eq!(accepted.event_type(), "intent_plan_accepted");
    accepted.validate_contract()?;
    task.validate_contract()?;
    chat.validate_contract()?;
    artifact.validate_contract()?;
    verification.validate_contract()?;
    assert!(matches!(
        task,
        IntentEventV1::ExecutionBound {
            binding: IntentExecutionBindingV1 {
                origin: IntentExecutionOriginV1::Task {
                    attempt_id: Some(_),
                    ..
                },
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        chat,
        IntentEventV1::ExecutionBound {
            binding: IntentExecutionBindingV1 {
                origin: IntentExecutionOriginV1::Chat {
                    attempt_id: Some(_),
                    ..
                },
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        artifact.subject,
        BoundedIntentArtifactSubjectV1::FileHunk { .. }
    ));
    assert_eq!(artifact.ownership, IntentArtifactOwnership::Exclusive);
    assert_eq!(shared.code, IntentOperationErrorCode::SharedArtifact);
    assert!(matches!(
        verification,
        IntentEventV1::VerificationLinked { ref evidence, .. }
            if evidence.iter().any(|item| item.level == IntentCriterionEvidenceLevel::Advisory)
                && evidence.iter().any(|item| {
                    item.level == IntentCriterionEvidenceLevel::SystemVerified
                })
    ));
    assert_eq!(
        read_only.authority_state,
        IntentAuthorityState::ReadOnlyProvenance
    );
    assert!(read_only.intents[0].available_actions.is_empty());
    assert_eq!(
        unavailable.code,
        IntentOperationErrorCode::ArtifactUnavailable
    );

    let shared: Vec<IntentArtifactBindingV1> = serde_json::from_str(SHARED_FILE_ARTIFACTS)?;
    assert_eq!(shared.len(), 2);
    assert!(
        shared
            .iter()
            .all(|artifact| artifact.ownership == IntentArtifactOwnership::Shared)
    );
    let paths = shared
        .iter()
        .map(|artifact| match &artifact.subject {
            BoundedIntentArtifactSubjectV1::FileHunk {
                normalized_relative_path,
                ..
            } => normalized_relative_path.as_str(),
            _ => panic!("shared file fixture must contain file hunks"),
        })
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["src/retry.rs", "src/retry.rs"]);

    let deleted: IntentArtifactBindingV1 = serde_json::from_str(DELETED_ARTIFACT)?;
    assert_eq!(deleted.availability, IntentArtifactAvailability::Deleted);
    assert_eq!(unavailable.artifact_id.as_ref(), Some(&deleted.artifact_id));

    let boundaries: AuthorityBoundaries = serde_json::from_str(AUTHORITY_BOUNDARIES)?;
    assert_eq!(boundaries.resume, IntentAuthorityState::Active);
    assert_eq!(boundaries.fork, IntentAuthorityState::ReadOnlyProvenance);
    assert_eq!(
        boundaries.worktree_child,
        IntentAuthorityState::ReadOnlyProvenance
    );
    assert_eq!(
        boundaries.workspace_switch,
        IntentAuthorityState::OutOfScope
    );
    assert!(matches!(
        boundaries.old_session,
        PublicIntentStackStateV1::HistoryUnavailable { .. }
    ));
    Ok(())
}

#[test]
fn event_type_names_and_payload_variants_are_frozen() -> Result<()> {
    let plan: IntentPlanV1 = serde_json::from_str(ACCEPTED_PLAN)?;
    let graph: DigestGraph = serde_json::from_str(DIGEST_GRAPH)?;
    let DigestGraph {
        layer_core,
        artifact_manifest,
        layer_manifest,
        operation_preview,
    } = graph;
    let conflict = IntentConflictV1 {
        code: IntentOperationErrorCode::DriftedArtifact,
        intent_ref: Some(intent_ref("retry")?),
        artifact_id: Some(IntentArtifactId::new("artifact-hunk-1")?),
        safe_reason: "current bytes no longer match the layer-local digest".to_owned(),
    };
    let binding = IntentExecutionBindingV1 {
        execution_id: IntentExecutionId::new("execution-task-1")?,
        intent_ref: intent_ref("retry")?,
        origin: layer_core.execution_origin.clone(),
        binding_kind: IntentExecutionBindingKind::Mutation,
        source_event_id: "event-bind-1".to_owned(),
    };
    let events = vec![
        IntentEventV1::StackCreated {
            schema_version: 1,
            stack_id: IntentStackId::new("stack-demo-1")?,
            workspace_id: "workspace-demo".to_owned(),
            source_session_id: "session-demo".to_owned(),
        },
        IntentEventV1::PlanRecorded {
            schema_version: 1,
            plan: plan.clone(),
        },
        IntentEventV1::PlanAccepted {
            schema_version: 1,
            stack_id: plan.stack_id.clone(),
            stack_version: plan.stack_version,
            plan_digest: plan.plan_digest.clone(),
            acceptance_kind: IntentAcceptanceKind::ExplicitUserConfirmation,
            source_turn_id: "turn-1".to_owned(),
        },
        IntentEventV1::ExecutionBound {
            schema_version: 1,
            stack_id: plan.stack_id.clone(),
            stack_version: plan.stack_version,
            binding,
        },
        IntentEventV1::ChangeSetBound {
            schema_version: 1,
            intent_ref: intent_ref("retry")?,
            execution_id: IntentExecutionId::new("execution-task-1")?,
            changeset_ids: vec!["changeset-1".to_owned()],
        },
        IntentEventV1::LayerManifestRecorded {
            schema_version: 1,
            manifest: layer_manifest,
        },
        IntentEventV1::ArtifactBindingsRecorded {
            schema_version: 1,
            manifest: artifact_manifest,
        },
        IntentEventV1::VerificationLinked {
            schema_version: 1,
            evidence: vec![IntentCriterionEvidenceV1 {
                intent_ref: intent_ref("retry")?,
                criterion_id: IntentCriterionId::new("retry-test")?,
                level: IntentCriterionEvidenceLevel::SystemVerified,
                receipt_id: "receipt-1".to_owned(),
                parent_snapshot_id: "snapshot-result-1".to_owned(),
                verification_policy_digest: digest('c')?,
                changeset_ids: vec!["changeset-1".to_owned()],
                source_event_id: "event-verify-1".to_owned(),
            }],
        },
        IntentEventV1::OperationRequested {
            schema_version: 1,
            preview: operation_preview,
            safe_reason: "remove telemetry".to_owned(),
        },
        IntentEventV1::OperationPrepared {
            schema_version: 1,
            operation_id: IntentOperationId::new("operation-drop-1")?,
            stack_id: IntentStackId::new("stack-demo-1")?,
            stack_version: IntentStackVersion::new(1)?,
            preview_digest: digest('9')?,
            artifact_manifest_digest: digest('b')?,
            workspace_revision: 7,
            permission_policy_digest: digest('e')?,
            approval_authority_id: "approval-1".to_owned(),
            expires_at_ms: Some(1_000),
            mutation_batch_id: "mutation-batch-1".to_owned(),
        },
        IntentEventV1::OperationResolved {
            schema_version: 1,
            operation_id: IntentOperationId::new("operation-drop-1")?,
            resolution: IntentOperationResolution::Conflicted,
            mutation_batch_id: Some("mutation-batch-1".to_owned()),
            result_snapshot_id: None,
            error_code: Some(IntentOperationErrorCode::DriftedArtifact),
        },
        IntentEventV1::ConflictRecorded {
            schema_version: 1,
            stack_id: IntentStackId::new("stack-demo-1")?,
            stack_version: IntentStackVersion::new(1)?,
            conflict,
        },
        IntentEventV1::VersionSuperseded {
            schema_version: 1,
            previous: intent_ref("retry")?,
            replacement: IntentVersionRef::new(IntentId::new("retry")?, 2)?,
            safe_reason: "acceptance criteria changed".to_owned(),
        },
    ];

    for event in &events {
        event.validate_contract()?;
        let encoded = serde_json::to_value(event)?;
        let decoded: IntentEventV1 = serde_json::from_value(encoded)?;
        assert_eq!(&decoded, event);
    }
    let actual = events
        .iter()
        .map(IntentEventV1::event_type)
        .collect::<Vec<_>>();
    let expected: Vec<String> = serde_json::from_str(EVENT_TYPES)?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn operation_error_taxonomy_is_frozen() -> Result<()> {
    let codes = [
        IntentOperationErrorCode::UnsupportedSchema,
        IntentOperationErrorCode::IntentHistoryUnavailable,
        IntentOperationErrorCode::UnknownIntent,
        IntentOperationErrorCode::UnknownOperation,
        IntentOperationErrorCode::StaleIntentVersion,
        IntentOperationErrorCode::StaleStackVersion,
        IntentOperationErrorCode::InvalidDependencyGraph,
        IntentOperationErrorCode::TargetNotLeaf,
        IntentOperationErrorCode::SharedArtifact,
        IntentOperationErrorCode::UnownedArtifact,
        IntentOperationErrorCode::DriftedArtifact,
        IntentOperationErrorCode::ArtifactUnavailable,
        IntentOperationErrorCode::ArtifactDigestMismatch,
        IntentOperationErrorCode::UnsupportedArtifact,
        IntentOperationErrorCode::UnsupportedSideEffect,
        IntentOperationErrorCode::MissingExecutionLineage,
        IntentOperationErrorCode::MissingParentMutationEvidence,
        IntentOperationErrorCode::MissingCurrentVerificationEvidence,
        IntentOperationErrorCode::PreviewDigestMismatch,
        IntentOperationErrorCode::WorkspaceRevisionMismatch,
        IntentOperationErrorCode::PermissionDenied,
        IntentOperationErrorCode::ApprovalAuthorityUnavailable,
        IntentOperationErrorCode::WorkspaceLeaseUnavailable,
        IntentOperationErrorCode::WorkspaceOutOfScope,
        IntentOperationErrorCode::OperationStateConflict,
        IntentOperationErrorCode::IntentStateConflict,
        IntentOperationErrorCode::PartialApplication,
        IntentOperationErrorCode::ReconciliationRequired,
    ];
    let expected: Value = serde_json::from_str(OPERATION_ERRORS)?;
    assert_eq!(serde_json::to_value(codes)?, expected);
    Ok(())
}

#[test]
fn enum_tokens_are_frozen() -> Result<()> {
    let actual = json!({
        "plan_kind": [
            IntentPlanKind::UserDeclaredRoot,
            IntentPlanKind::SuggestedDecomposition,
        ],
        "digest_domain": [
            IntentDigestDomain::Plan,
            IntentDigestDomain::Proposal,
            IntentDigestDomain::LayerCore,
            IntentDigestDomain::LayerManifest,
            IntentDigestDomain::ArtifactManifest,
            IntentDigestDomain::OperationPreview,
        ],
        "acceptance_kind": [
            IntentAcceptanceKind::UserDeclaredRootAdmission,
            IntentAcceptanceKind::ExplicitUserConfirmation,
            IntentAcceptanceKind::ContentBoundSpecDecision,
        ],
        "definition_state": [
            IntentDefinitionState::Proposed,
            IntentDefinitionState::Accepted,
            IntentDefinitionState::Superseded,
            IntentDefinitionState::Invalid,
        ],
        "application_state": [
            IntentApplicationState::Unapplied,
            IntentApplicationState::Applied,
            IntentApplicationState::Dropped,
            IntentApplicationState::NeedsReview,
            IntentApplicationState::NeedsRebuild,
            IntentApplicationState::ReadOnly,
            IntentApplicationState::OutOfScope,
        ],
        "authority_state": [
            IntentAuthorityState::Active,
            IntentAuthorityState::ReadOnlyProvenance,
            IntentAuthorityState::OutOfScope,
        ],
        "execution_binding_kind": [
            IntentExecutionBindingKind::Admission,
            IntentExecutionBindingKind::Mutation,
            IntentExecutionBindingKind::Review,
            IntentExecutionBindingKind::Verification,
        ],
        "artifact_kind": [
            IntentArtifactKind::FileHunk,
            IntentArtifactKind::TestEvidence,
            IntentArtifactKind::Documentation,
            IntentArtifactKind::ChangeSet,
            IntentArtifactKind::VerificationReceipt,
            IntentArtifactKind::UnsupportedSideEffect,
        ],
        "artifact_ownership": [
            IntentArtifactOwnership::Exclusive,
            IntentArtifactOwnership::Shared,
            IntentArtifactOwnership::Unowned,
            IntentArtifactOwnership::Drifted,
        ],
        "artifact_availability": [
            IntentArtifactAvailability::Available,
            IntentArtifactAvailability::Deleted,
            IntentArtifactAvailability::Expired,
            IntentArtifactAvailability::Corrupted,
        ],
        "criterion_evidence_level": [
            IntentCriterionEvidenceLevel::Advisory,
            IntentCriterionEvidenceLevel::SystemVerified,
        ],
        "operation_kind": [
            IntentOperationKind::Drop,
            IntentOperationKind::ReviseImpactPreview,
            IntentOperationKind::ReplaceImpactPreview,
            IntentOperationKind::Adopt,
        ],
        "operation_resolution": [
            IntentOperationResolution::Committed,
            IntentOperationResolution::Rejected,
            IntentOperationResolution::Cancelled,
            IntentOperationResolution::Conflicted,
            IntentOperationResolution::PartiallyApplied,
            IntentOperationResolution::Interrupted,
        ],
        "file_action": [
            IntentOperationFileAction::Create,
            IntentOperationFileAction::Update,
            IntentOperationFileAction::Delete,
        ],
        "verification_impact": [
            IntentVerificationImpact::BecomesStale,
            IntentVerificationImpact::RerunRequired,
        ],
    });
    assert_eq!(actual, serde_json::from_str::<Value>(ENUM_TOKENS)?);
    Ok(())
}

#[test]
fn schema_and_identity_decoding_fail_closed() -> Result<()> {
    let mut plan: Value = serde_json::from_str(ACCEPTED_PLAN)?;
    plan.as_object_mut()
        .expect("plan fixture must be an object")
        .insert("future_authority".to_owned(), json!(true));
    assert!(
        serde_json::from_value::<IntentPlanV1>(plan)
            .expect_err("unknown plan fields must fail")
            .to_string()
            .contains("unknown field")
    );

    let cases: ContractCases = serde_json::from_str(CONTRACT_CASES)?;
    let mut accepted = cases.plan_accepted;
    accepted
        .as_object_mut()
        .expect("event fixture must be an object")
        .insert("schema_version".to_owned(), json!(2));
    let event: IntentEventV1 = serde_json::from_value(accepted)?;
    assert!(
        event
            .validate_schema()
            .expect_err("unknown schema must fail")
            .to_string()
            .contains("unsupported intent contract schema version")
    );

    assert!(IntentStackVersion::new(0).is_err());
    assert!(IntentVersionRef::new(IntentId::new("intent")?, 0).is_err());
    assert!(IntentId::new("../escape").is_err());
    assert!(IntentDigest::new("sha256:deadbeef").is_err());
    assert!(IntentContentDigest::new("sha256:jcs-v1:deadbeef").is_err());
    assert_eq!(
        IntentContentDigest::from_bytes(b"exact bytes").as_str(),
        "sha256:e38e581aade78b64cc86f7ac9f3555ca78c2dcca747942a7f1d9b3275a834f75"
    );
    Ok(())
}

#[test]
fn contract_validation_rejects_ambiguous_plan_and_layer_shapes() -> Result<()> {
    let mut cyclic: IntentPlanV1 = serde_json::from_str(ACCEPTED_PLAN)?;
    cyclic.intents[0]
        .depends_on
        .push(IntentId::new("operations-docs")?);
    assert!(
        cyclic
            .validate_contract()
            .expect_err("dependency cycle must fail")
            .to_string()
            .contains("contains a cycle")
    );

    let mut duplicate: IntentPlanV1 = serde_json::from_str(ACCEPTED_PLAN)?;
    duplicate.intents[1]
        .depends_on
        .push(IntentId::new("retry")?);
    assert!(
        duplicate
            .validate_contract()
            .expect_err("duplicate dependency must fail")
            .to_string()
            .contains("duplicate dependencies")
    );

    let mut root: IntentPlanV1 = serde_json::from_str(USER_ROOT_PLAN)?;
    root.kind = IntentPlanKind::SuggestedDecomposition;
    assert!(
        root.validate_contract()
            .expect_err("suggested plan cannot masquerade as user root")
            .to_string()
            .contains("direct user-root provenance")
    );

    let mut missing_source: IntentPlanV1 = serde_json::from_str(USER_ROOT_PLAN)?;
    missing_source.intents[0].source = IntentSourceV1::UserTurn {
        source_turn_id: String::new(),
    };
    assert!(
        missing_source
            .validate_contract()
            .expect_err("empty intent provenance must fail")
            .to_string()
            .contains("source turn id cannot be empty")
    );

    let mut bad_supersede: IntentPlanV1 = serde_json::from_str(USER_ROOT_PLAN)?;
    bad_supersede.intents[0].intent_ref.version = 2;
    bad_supersede.intents[0].supersedes = Some(intent_ref("another-intent")?);
    assert!(
        bad_supersede
            .validate_contract()
            .expect_err("cross-intent supersede must fail")
            .to_string()
            .contains("earlier version of the same intent")
    );

    let mut cyclic_proposal: IntentPlanProposalV1 = serde_json::from_str(PROPOSAL)?;
    cyclic_proposal.intents[0]
        .depends_on_aliases
        .push("telemetry".to_owned());
    assert!(
        cyclic_proposal
            .validate_contract()
            .expect_err("proposal cycle must fail")
            .to_string()
            .contains("contains a cycle")
    );

    let mut graph: DigestGraph = serde_json::from_str(DIGEST_GRAPH)?;
    match &mut graph.layer_core.execution_origin {
        IntentExecutionOriginV1::Task { attempt_id, .. }
        | IntentExecutionOriginV1::Chat { attempt_id, .. } => *attempt_id = None,
    }
    assert!(
        graph
            .layer_core
            .validate_contract()
            .expect_err("executable layer without terminal attempt must fail")
            .to_string()
            .contains("concrete terminal attempt")
    );

    let mut empty_attempt: DigestGraph = serde_json::from_str(DIGEST_GRAPH)?;
    match &mut empty_attempt.layer_core.execution_origin {
        IntentExecutionOriginV1::Task { attempt_id, .. }
        | IntentExecutionOriginV1::Chat { attempt_id, .. } => *attempt_id = Some(String::new()),
    }
    assert!(
        empty_attempt
            .layer_core
            .validate_contract()
            .expect_err("empty terminal attempt must fail")
            .to_string()
            .contains("attempt id cannot be empty")
    );

    let mut cross_execution: DigestGraph = serde_json::from_str(DIGEST_GRAPH)?;
    cross_execution.artifact_manifest.artifacts[0]
        .provenance
        .execution_id = IntentExecutionId::new("execution-other")?;
    assert!(
        cross_execution
            .artifact_manifest
            .validate_contract()
            .expect_err("cross-execution artifact lineage must fail")
            .to_string()
            .contains("different execution")
    );

    let mut invalid_preview: DigestGraph = serde_json::from_str(DIGEST_GRAPH)?;
    invalid_preview.operation_preview.target_intents[0].version = 0;
    assert!(
        invalid_preview
            .operation_preview
            .validate_contract()
            .expect_err("zero-version preview target must fail")
            .to_string()
            .contains("intent version must be non-zero")
    );

    assert!(
        serde_json::from_value::<IntentEventV1>(json!({
            "record": "future_intent_event",
            "schema_version": 1
        }))
        .expect_err("unknown recovery-critical event payload must fail")
        .to_string()
        .contains("unknown variant")
    );
    Ok(())
}

#[test]
fn fixture_corpus_covers_every_locked_decision_and_stays_adapter_neutral() -> Result<()> {
    let locked: Vec<LockedDecision> = serde_json::from_str(LOCKED_DECISIONS)?;
    assert_eq!(locked.len(), 8);
    assert_eq!(
        locked
            .iter()
            .map(|decision| decision.decision.as_str())
            .collect::<Vec<_>>(),
        vec![
            "acceptance_is_independent",
            "task_and_chat_provenance_are_explicit",
            "file_hunk_identity_is_layer_local_canonical_bytes",
            "same_file_across_intents_is_shared_read_only",
            "criterion_evidence_has_advisory_and_system_verified_levels",
            "fork_and_workspace_switch_do_not_inherit_mutation_authority",
            "deleted_artifact_permanently_downgrades_operations",
            "contract_is_provider_and_adapter_neutral",
        ]
    );
    assert!(locked.iter().all(|decision| !decision.fixture.is_empty()));

    let corpus = [
        ACCEPTED_PLAN,
        PROPOSAL,
        USER_ROOT_PLAN,
        CONTRACT_CASES,
        SHARED_FILE_ARTIFACTS,
        DELETED_ARTIFACT,
        AUTHORITY_BOUNDARIES,
        DIGEST_GRAPH,
        ENUM_TOKENS,
        EVENT_TYPES,
        OPERATION_ERRORS,
    ]
    .join("\n");
    for forbidden in [
        "provider_name",
        "provider_payload",
        "raw_patch",
        "forward_patch_bytes",
        "reverse_patch_bytes",
        "/Users/",
        "/home/",
    ] {
        assert!(
            !corpus.contains(forbidden),
            "fixture corpus must not expose {forbidden}"
        );
    }
    Ok(())
}
