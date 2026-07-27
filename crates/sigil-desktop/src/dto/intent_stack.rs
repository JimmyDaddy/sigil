use serde::{Deserialize, Serialize};

/// Stable reference to one immutable Intent definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentVersionRef {
    pub intent_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentAcceptanceCriterion {
    pub criterion_id: String,
    pub statement: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentDefinitionState {
    Proposed,
    Accepted,
    Superseded,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentApplicationState {
    Unapplied,
    Applied,
    Dropped,
    NeedsReview,
    NeedsRebuild,
    ReadOnly,
    OutOfScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentAuthorityState {
    Active,
    ReadOnlyProvenance,
    OutOfScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentArtifactKind {
    FileHunk,
    TestEvidence,
    Documentation,
    ChangeSet,
    VerificationReceipt,
    UnsupportedSideEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentArtifactOwnership {
    Exclusive,
    Shared,
    Unowned,
    Drifted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentArtifactAvailability {
    Available,
    Deleted,
    Expired,
    Corrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentOperationKind {
    Drop,
    ReviseImpactPreview,
    ReplaceImpactPreview,
    Adopt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentOperationResolution {
    Committed,
    Rejected,
    Cancelled,
    Conflicted,
    PartiallyApplied,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentOperationErrorCode {
    UnsupportedSchema,
    IntentHistoryUnavailable,
    UnknownIntent,
    UnknownOperation,
    StaleIntentVersion,
    StaleStackVersion,
    InvalidDependencyGraph,
    TargetNotLeaf,
    SharedArtifact,
    UnownedArtifact,
    DriftedArtifact,
    ArtifactUnavailable,
    ArtifactDigestMismatch,
    UnsupportedArtifact,
    UnsupportedSideEffect,
    MissingExecutionLineage,
    MissingParentMutationEvidence,
    MissingCurrentVerificationEvidence,
    PreviewDigestMismatch,
    WorkspaceRevisionMismatch,
    PermissionDenied,
    ApprovalAuthorityUnavailable,
    WorkspaceLeaseUnavailable,
    WorkspaceOutOfScope,
    OperationStateConflict,
    IntentStateConflict,
    PartialApplication,
    ReconciliationRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentOperationFileAction {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntentVerificationImpact {
    BecomesStale,
    RerunRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopIntentSource {
    UserTurn { source_turn_id: String },
    AcceptedSuggestion { source_turn_id: String },
    TrustedSpec { safe_source_label: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentArtifactSummary {
    pub artifact_id: String,
    pub artifact_kind: DesktopIntentArtifactKind,
    pub ownership: DesktopIntentArtifactOwnership,
    pub availability: DesktopIntentArtifactAvailability,
    #[serde(default)]
    pub normalized_relative_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentConflict {
    pub code: DesktopIntentOperationErrorCode,
    #[serde(default)]
    pub intent_ref: Option<DesktopIntentVersionRef>,
    #[serde(default)]
    pub artifact_id: Option<String>,
    pub safe_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntent {
    pub intent_ref: DesktopIntentVersionRef,
    pub title: String,
    pub statement: String,
    pub acceptance_criteria: Vec<DesktopIntentAcceptanceCriterion>,
    pub depends_on: Vec<String>,
    pub source: DesktopIntentSource,
    pub definition_state: DesktopIntentDefinitionState,
    pub application_state: DesktopIntentApplicationState,
    pub exclusive_artifact_count: u32,
    pub shared_artifact_count: u32,
    pub unowned_artifact_count: u32,
    pub drifted_artifact_count: u32,
    pub unavailable_artifact_count: u32,
    pub advisory_criterion_count: u32,
    pub system_verified_criterion_count: u32,
    pub artifacts: Vec<DesktopIntentArtifactSummary>,
    pub available_actions: Vec<DesktopIntentOperationKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentStack {
    pub schema_version: u16,
    pub stack_id: String,
    pub stack_version: u64,
    pub authority_state: DesktopIntentAuthorityState,
    pub plan_digest: String,
    pub intents: Vec<DesktopIntent>,
    pub conflicts: Vec<DesktopIntentConflict>,
}

/// Bounded availability state returned for both current and legacy sessions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopIntentStackState {
    Available {
        schema_version: u16,
        stack: DesktopIntentStack,
    },
    HistoryUnavailable {
        schema_version: u16,
        safe_message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentDropPreviewRequest {
    pub intent_ref: DesktopIntentVersionRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentOperationFileSummary {
    pub normalized_relative_path: String,
    pub action: DesktopIntentOperationFileAction,
    pub artifact_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentVerificationImpactSummary {
    pub receipt_id: String,
    pub impact: DesktopIntentVerificationImpact,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentOperationPreview {
    pub schema_version: u16,
    pub operation_id: String,
    pub operation_kind: DesktopIntentOperationKind,
    pub stack_id: String,
    pub stack_version: u64,
    pub target_intents: Vec<DesktopIntentVersionRef>,
    pub target_is_leaf: bool,
    pub workspace_revision: u64,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    pub file_effects: Vec<DesktopIntentOperationFileSummary>,
    pub retained_intents: Vec<DesktopIntentVersionRef>,
    pub verification_impacts: Vec<DesktopIntentVerificationImpactSummary>,
    pub conflicts: Vec<DesktopIntentConflict>,
    pub preview_digest: String,
}

/// Exact renderer-to-host request. It intentionally contains no path or authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentDropRequest {
    pub operation_id: String,
    pub stack_version: u64,
    pub preview_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentOperationExecution {
    pub preview: DesktopIntentOperationPreview,
    pub resolution: DesktopIntentOperationResolution,
    pub mutation_batch_id: Option<String>,
    pub committed_operation_ids: Vec<String>,
    pub result_snapshot_id: Option<String>,
    pub error_code: Option<DesktopIntentOperationErrorCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopIntentDropCommandReceipt {
    pub command_id: String,
    pub client_id: String,
    pub session_id: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    pub execution: DesktopIntentOperationExecution,
    pub replayed: bool,
}
