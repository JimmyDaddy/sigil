use serde::{Deserialize, Serialize};
use sigil_desktop::{
    DesktopIntent, DesktopIntentApplicationState, DesktopIntentArtifactAvailability,
    DesktopIntentArtifactKind, DesktopIntentArtifactOwnership, DesktopIntentArtifactSummary,
    DesktopIntentAuthorityState, DesktopIntentConflict, DesktopIntentDefinitionState,
    DesktopIntentDropRequest, DesktopIntentOperationErrorCode, DesktopIntentOperationExecution,
    DesktopIntentOperationFileAction, DesktopIntentOperationKind, DesktopIntentOperationPreview,
    DesktopIntentOperationResolution, DesktopIntentSource, DesktopIntentStack,
    DesktopIntentStackState, DesktopIntentVerificationImpact, DesktopIntentVersionRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopIntentVersionBinding {
    pub(crate) intent_id: String,
    pub(crate) version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopIntentDropPreviewInput {
    pub(crate) session_id: String,
    pub(crate) intent_ref: DesktopIntentVersionBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopIntentDropBinding {
    pub(crate) operation_id: String,
    pub(crate) stack_version: u64,
    pub(crate) preview_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopIntentDropInput {
    pub(crate) session_id: String,
    pub(crate) request: DesktopIntentDropBinding,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DesktopIntentStackSummary {
    Available {
        #[serde(rename = "schemaVersion")]
        schema_version: u16,
        stack: DesktopIntentStackDetails,
    },
    NotCreated {
        #[serde(rename = "schemaVersion")]
        schema_version: u16,
        #[serde(rename = "safeMessage")]
        safe_message: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentStackDetails {
    pub(crate) schema_version: u16,
    pub(crate) stack_id: String,
    pub(crate) stack_version: u64,
    pub(crate) authority_state: &'static str,
    pub(crate) plan_digest: String,
    pub(crate) intents: Vec<DesktopIntentSummary>,
    pub(crate) conflicts: Vec<DesktopIntentConflictSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentCriterionSummary {
    pub(crate) criterion_id: String,
    pub(crate) statement: String,
    pub(crate) required: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DesktopIntentSourceSummary {
    UserTurn {
        #[serde(rename = "sourceTurnId")]
        source_turn_id: String,
    },
    AcceptedSuggestion {
        #[serde(rename = "sourceTurnId")]
        source_turn_id: String,
    },
    TrustedSpec {
        #[serde(rename = "safeSourceLabel")]
        safe_source_label: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentArtifactSummaryView {
    pub(crate) artifact_id: String,
    pub(crate) artifact_kind: &'static str,
    pub(crate) ownership: &'static str,
    pub(crate) availability: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) normalized_relative_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentConflictSummary {
    pub(crate) code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) intent_ref: Option<DesktopIntentVersionBinding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) artifact_id: Option<String>,
    pub(crate) safe_reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentSummary {
    pub(crate) intent_ref: DesktopIntentVersionBinding,
    pub(crate) title: String,
    pub(crate) statement: String,
    pub(crate) acceptance_criteria: Vec<DesktopIntentCriterionSummary>,
    pub(crate) depends_on: Vec<String>,
    pub(crate) source: DesktopIntentSourceSummary,
    pub(crate) definition_state: &'static str,
    pub(crate) application_state: &'static str,
    pub(crate) exclusive_artifact_count: u32,
    pub(crate) shared_artifact_count: u32,
    pub(crate) unowned_artifact_count: u32,
    pub(crate) drifted_artifact_count: u32,
    pub(crate) unavailable_artifact_count: u32,
    pub(crate) advisory_criterion_count: u32,
    pub(crate) system_verified_criterion_count: u32,
    pub(crate) artifacts: Vec<DesktopIntentArtifactSummaryView>,
    pub(crate) available_actions: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentFileEffectSummary {
    pub(crate) normalized_relative_path: String,
    pub(crate) action: &'static str,
    pub(crate) artifact_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentVerificationImpactSummaryView {
    pub(crate) receipt_id: String,
    pub(crate) impact: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentDropPreviewSummary {
    pub(crate) schema_version: u16,
    pub(crate) operation_id: String,
    pub(crate) operation_kind: &'static str,
    pub(crate) stack_id: String,
    pub(crate) stack_version: u64,
    pub(crate) target_intents: Vec<DesktopIntentVersionBinding>,
    pub(crate) target_is_leaf: bool,
    pub(crate) workspace_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at_ms: Option<u64>,
    pub(crate) file_effects: Vec<DesktopIntentFileEffectSummary>,
    pub(crate) retained_intents: Vec<DesktopIntentVersionBinding>,
    pub(crate) verification_impacts: Vec<DesktopIntentVerificationImpactSummaryView>,
    pub(crate) conflicts: Vec<DesktopIntentConflictSummary>,
    pub(crate) preview_digest: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopIntentDropExecutionSummary {
    pub(crate) preview: DesktopIntentDropPreviewSummary,
    pub(crate) resolution: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mutation_batch_id: Option<String>,
    pub(crate) committed_operation_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_code: Option<&'static str>,
}

impl From<DesktopIntentVersionBinding> for DesktopIntentVersionRef {
    fn from(value: DesktopIntentVersionBinding) -> Self {
        Self {
            intent_id: value.intent_id,
            version: value.version,
        }
    }
}

impl From<DesktopIntentVersionRef> for DesktopIntentVersionBinding {
    fn from(value: DesktopIntentVersionRef) -> Self {
        Self {
            intent_id: value.intent_id,
            version: value.version,
        }
    }
}

impl From<DesktopIntentDropBinding> for DesktopIntentDropRequest {
    fn from(value: DesktopIntentDropBinding) -> Self {
        Self {
            operation_id: value.operation_id,
            stack_version: value.stack_version,
            preview_digest: value.preview_digest,
        }
    }
}

impl From<DesktopIntentStackState> for DesktopIntentStackSummary {
    fn from(value: DesktopIntentStackState) -> Self {
        match value {
            DesktopIntentStackState::Available {
                schema_version,
                stack,
            } => Self::Available {
                schema_version,
                stack: stack.into(),
            },
            DesktopIntentStackState::NotCreated {
                schema_version,
                safe_message,
            } => Self::NotCreated {
                schema_version,
                safe_message,
            },
        }
    }
}

impl From<DesktopIntentStack> for DesktopIntentStackDetails {
    fn from(value: DesktopIntentStack) -> Self {
        Self {
            schema_version: value.schema_version,
            stack_id: value.stack_id,
            stack_version: value.stack_version,
            authority_state: intent_authority_state_label(value.authority_state),
            plan_digest: value.plan_digest,
            intents: value.intents.into_iter().map(Into::into).collect(),
            conflicts: value.conflicts.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<DesktopIntent> for DesktopIntentSummary {
    fn from(value: DesktopIntent) -> Self {
        Self {
            intent_ref: value.intent_ref.into(),
            title: value.title,
            statement: value.statement,
            acceptance_criteria: value
                .acceptance_criteria
                .into_iter()
                .map(|criterion| DesktopIntentCriterionSummary {
                    criterion_id: criterion.criterion_id,
                    statement: criterion.statement,
                    required: criterion.required,
                })
                .collect(),
            depends_on: value.depends_on,
            source: match value.source {
                DesktopIntentSource::UserTurn { source_turn_id } => {
                    DesktopIntentSourceSummary::UserTurn { source_turn_id }
                }
                DesktopIntentSource::AcceptedSuggestion { source_turn_id } => {
                    DesktopIntentSourceSummary::AcceptedSuggestion { source_turn_id }
                }
                DesktopIntentSource::TrustedSpec { safe_source_label } => {
                    DesktopIntentSourceSummary::TrustedSpec { safe_source_label }
                }
            },
            definition_state: intent_definition_state_label(value.definition_state),
            application_state: intent_application_state_label(value.application_state),
            exclusive_artifact_count: value.exclusive_artifact_count,
            shared_artifact_count: value.shared_artifact_count,
            unowned_artifact_count: value.unowned_artifact_count,
            drifted_artifact_count: value.drifted_artifact_count,
            unavailable_artifact_count: value.unavailable_artifact_count,
            advisory_criterion_count: value.advisory_criterion_count,
            system_verified_criterion_count: value.system_verified_criterion_count,
            artifacts: value.artifacts.into_iter().map(Into::into).collect(),
            available_actions: value
                .available_actions
                .into_iter()
                .map(intent_operation_kind_label)
                .collect(),
        }
    }
}

impl From<DesktopIntentArtifactSummary> for DesktopIntentArtifactSummaryView {
    fn from(value: DesktopIntentArtifactSummary) -> Self {
        Self {
            artifact_id: value.artifact_id,
            artifact_kind: intent_artifact_kind_label(value.artifact_kind),
            ownership: intent_artifact_ownership_label(value.ownership),
            availability: intent_artifact_availability_label(value.availability),
            normalized_relative_path: value.normalized_relative_path,
        }
    }
}

impl From<DesktopIntentConflict> for DesktopIntentConflictSummary {
    fn from(value: DesktopIntentConflict) -> Self {
        Self {
            code: intent_operation_error_label(value.code),
            intent_ref: value.intent_ref.map(Into::into),
            artifact_id: value.artifact_id,
            safe_reason: value.safe_reason,
        }
    }
}

impl From<DesktopIntentOperationPreview> for DesktopIntentDropPreviewSummary {
    fn from(value: DesktopIntentOperationPreview) -> Self {
        Self {
            schema_version: value.schema_version,
            operation_id: value.operation_id,
            operation_kind: intent_operation_kind_label(value.operation_kind),
            stack_id: value.stack_id,
            stack_version: value.stack_version,
            target_intents: value.target_intents.into_iter().map(Into::into).collect(),
            target_is_leaf: value.target_is_leaf,
            workspace_revision: value.workspace_revision,
            expires_at_ms: value.expires_at_ms,
            file_effects: value
                .file_effects
                .into_iter()
                .map(|effect| DesktopIntentFileEffectSummary {
                    normalized_relative_path: effect.normalized_relative_path,
                    action: intent_file_action_label(effect.action),
                    artifact_ids: effect.artifact_ids,
                })
                .collect(),
            retained_intents: value.retained_intents.into_iter().map(Into::into).collect(),
            verification_impacts: value
                .verification_impacts
                .into_iter()
                .map(|impact| DesktopIntentVerificationImpactSummaryView {
                    receipt_id: impact.receipt_id,
                    impact: intent_verification_impact_label(impact.impact),
                })
                .collect(),
            conflicts: value.conflicts.into_iter().map(Into::into).collect(),
            preview_digest: value.preview_digest,
        }
    }
}

impl From<DesktopIntentOperationExecution> for DesktopIntentDropExecutionSummary {
    fn from(value: DesktopIntentOperationExecution) -> Self {
        Self {
            preview: value.preview.into(),
            resolution: intent_operation_resolution_label(value.resolution),
            mutation_batch_id: value.mutation_batch_id,
            committed_operation_ids: value.committed_operation_ids,
            result_snapshot_id: value.result_snapshot_id,
            error_code: value.error_code.map(intent_operation_error_label),
        }
    }
}

fn intent_definition_state_label(value: DesktopIntentDefinitionState) -> &'static str {
    match value {
        DesktopIntentDefinitionState::Proposed => "proposed",
        DesktopIntentDefinitionState::Accepted => "accepted",
        DesktopIntentDefinitionState::Superseded => "superseded",
        DesktopIntentDefinitionState::Invalid => "invalid",
    }
}

fn intent_application_state_label(value: DesktopIntentApplicationState) -> &'static str {
    match value {
        DesktopIntentApplicationState::Unapplied => "unapplied",
        DesktopIntentApplicationState::Applied => "applied",
        DesktopIntentApplicationState::Dropped => "dropped",
        DesktopIntentApplicationState::NeedsReview => "needs_review",
        DesktopIntentApplicationState::NeedsRebuild => "needs_rebuild",
        DesktopIntentApplicationState::ReadOnly => "read_only",
        DesktopIntentApplicationState::OutOfScope => "out_of_scope",
    }
}

fn intent_authority_state_label(value: DesktopIntentAuthorityState) -> &'static str {
    match value {
        DesktopIntentAuthorityState::Active => "active",
        DesktopIntentAuthorityState::ReadOnlyProvenance => "read_only_provenance",
        DesktopIntentAuthorityState::OutOfScope => "out_of_scope",
    }
}

fn intent_artifact_kind_label(value: DesktopIntentArtifactKind) -> &'static str {
    match value {
        DesktopIntentArtifactKind::FileHunk => "file_hunk",
        DesktopIntentArtifactKind::TestEvidence => "test_evidence",
        DesktopIntentArtifactKind::Documentation => "documentation",
        DesktopIntentArtifactKind::ChangeSet => "change_set",
        DesktopIntentArtifactKind::VerificationReceipt => "verification_receipt",
        DesktopIntentArtifactKind::UnsupportedSideEffect => "unsupported_side_effect",
    }
}

fn intent_artifact_ownership_label(value: DesktopIntentArtifactOwnership) -> &'static str {
    match value {
        DesktopIntentArtifactOwnership::Exclusive => "exclusive",
        DesktopIntentArtifactOwnership::Shared => "shared",
        DesktopIntentArtifactOwnership::Unowned => "unowned",
        DesktopIntentArtifactOwnership::Drifted => "drifted",
    }
}

fn intent_artifact_availability_label(value: DesktopIntentArtifactAvailability) -> &'static str {
    match value {
        DesktopIntentArtifactAvailability::Available => "available",
        DesktopIntentArtifactAvailability::Deleted => "deleted",
        DesktopIntentArtifactAvailability::Expired => "expired",
        DesktopIntentArtifactAvailability::Corrupted => "corrupted",
    }
}

fn intent_operation_kind_label(value: DesktopIntentOperationKind) -> &'static str {
    match value {
        DesktopIntentOperationKind::Drop => "drop",
        DesktopIntentOperationKind::ReviseImpactPreview => "revise_impact_preview",
        DesktopIntentOperationKind::ReplaceImpactPreview => "replace_impact_preview",
        DesktopIntentOperationKind::Adopt => "adopt",
    }
}

fn intent_file_action_label(value: DesktopIntentOperationFileAction) -> &'static str {
    match value {
        DesktopIntentOperationFileAction::Create => "create",
        DesktopIntentOperationFileAction::Update => "update",
        DesktopIntentOperationFileAction::Delete => "delete",
    }
}

fn intent_verification_impact_label(value: DesktopIntentVerificationImpact) -> &'static str {
    match value {
        DesktopIntentVerificationImpact::BecomesStale => "becomes_stale",
        DesktopIntentVerificationImpact::RerunRequired => "rerun_required",
    }
}

fn intent_operation_resolution_label(value: DesktopIntentOperationResolution) -> &'static str {
    match value {
        DesktopIntentOperationResolution::Committed => "committed",
        DesktopIntentOperationResolution::Rejected => "rejected",
        DesktopIntentOperationResolution::Cancelled => "cancelled",
        DesktopIntentOperationResolution::Conflicted => "conflicted",
        DesktopIntentOperationResolution::PartiallyApplied => "partially_applied",
        DesktopIntentOperationResolution::Interrupted => "interrupted",
    }
}

fn intent_operation_error_label(value: DesktopIntentOperationErrorCode) -> &'static str {
    match value {
        DesktopIntentOperationErrorCode::UnsupportedSchema => "unsupported_schema",
        DesktopIntentOperationErrorCode::UnknownIntent => "unknown_intent",
        DesktopIntentOperationErrorCode::UnknownOperation => "unknown_operation",
        DesktopIntentOperationErrorCode::StaleIntentVersion => "stale_intent_version",
        DesktopIntentOperationErrorCode::StaleStackVersion => "stale_stack_version",
        DesktopIntentOperationErrorCode::InvalidDependencyGraph => "invalid_dependency_graph",
        DesktopIntentOperationErrorCode::TargetNotLeaf => "target_not_leaf",
        DesktopIntentOperationErrorCode::SharedArtifact => "shared_artifact",
        DesktopIntentOperationErrorCode::UnownedArtifact => "unowned_artifact",
        DesktopIntentOperationErrorCode::DriftedArtifact => "drifted_artifact",
        DesktopIntentOperationErrorCode::ArtifactUnavailable => "artifact_unavailable",
        DesktopIntentOperationErrorCode::ArtifactDigestMismatch => "artifact_digest_mismatch",
        DesktopIntentOperationErrorCode::UnsupportedArtifact => "unsupported_artifact",
        DesktopIntentOperationErrorCode::UnsupportedSideEffect => "unsupported_side_effect",
        DesktopIntentOperationErrorCode::MissingExecutionLineage => "missing_execution_lineage",
        DesktopIntentOperationErrorCode::MissingParentMutationEvidence => {
            "missing_parent_mutation_evidence"
        }
        DesktopIntentOperationErrorCode::MissingCurrentVerificationEvidence => {
            "missing_current_verification_evidence"
        }
        DesktopIntentOperationErrorCode::PreviewDigestMismatch => "preview_digest_mismatch",
        DesktopIntentOperationErrorCode::WorkspaceRevisionMismatch => "workspace_revision_mismatch",
        DesktopIntentOperationErrorCode::PermissionDenied => "permission_denied",
        DesktopIntentOperationErrorCode::ApprovalAuthorityUnavailable => {
            "approval_authority_unavailable"
        }
        DesktopIntentOperationErrorCode::WorkspaceLeaseUnavailable => "workspace_lease_unavailable",
        DesktopIntentOperationErrorCode::WorkspaceOutOfScope => "workspace_out_of_scope",
        DesktopIntentOperationErrorCode::OperationStateConflict => "operation_state_conflict",
        DesktopIntentOperationErrorCode::IntentStateConflict => "intent_state_conflict",
        DesktopIntentOperationErrorCode::PartialApplication => "partial_application",
        DesktopIntentOperationErrorCode::ReconciliationRequired => "reconciliation_required",
    }
}
