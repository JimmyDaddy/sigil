use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvidenceReceipt {
    pub receipt_id: ReceiptId,
    pub source_session_id: SessionId,
    pub source_event_id: EventId,
    pub source_event_type: String,
    pub scope: EvidenceScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_tool_call: Option<ToolCallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_revision: Option<WorkspaceRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<PolicyHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_id: Option<ChangesetId>,
    pub status: ReceiptStatus,
    #[serde(default)]
    pub artifact_refs: Vec<ArtifactId>,
    pub redaction_state: RedactionState,
    pub recorded_at_stream_sequence: u64,
}

impl EvidenceReceipt {
    /// Validates minimum cross-session receipt identity required by parent projections.
    ///
    /// # Errors
    ///
    /// Returns an error when the receipt only has a local sequence or lacks source identifiers.
    pub fn validate_source_identity(&self) -> Result<()> {
        if self.source_session_id.trim().is_empty() {
            bail!("evidence receipt is missing source_session_id");
        }
        if self.source_event_id.trim().is_empty() {
            bail!("evidence receipt is missing source_event_id");
        }
        if self.source_event_type.trim().is_empty() {
            bail!("evidence receipt is missing source_event_type");
        }
        if self.recorded_at_stream_sequence == 0 {
            bail!("evidence receipt stream sequence must be non-zero");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum EvidenceScope {
    Run(String),
    Workspace(WorkspaceId),
    Task(String),
    Step(String),
    Agent(String),
    Changeset(ChangesetId),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Succeeded,
    Failed,
    Skipped,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionState {
    None,
    Redacted,
    ContainsSensitiveMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationReceipt {
    pub receipt: EvidenceReceipt,
    pub binding: VerificationBinding,
    pub check_spec_id: CheckSpecId,
    pub check_status: ReceiptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub mutates_verification_scope: bool,
}

/// Durable control entry recording a verification receipt produced by a check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationRecordedEntry {
    pub receipt: VerificationReceipt,
}

/// Durable link from a verification receipt to its exact evidence scope and workspace snapshot.
///
/// Changeset fields remain absent unless both an applied changeset event and a matching durable
/// workspace-lineage event precede the receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationReceiptLinkRecorded {
    pub receipt_id: ReceiptId,
    pub receipt_event_id: EventId,
    pub scope: EvidenceScope,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_id: Option<ChangesetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_apply_event_id: Option<EventId>,
}

/// Durable location summary for a terminal failed, inconclusive, or errored check run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationFailureLocatorRecorded {
    pub check_run_id: VerificationCheckRunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_artifact_id: Option<ArtifactId>,
    pub summary: String,
}

pub type VerificationCheckRunId = String;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCheckRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Inconclusive,
    Errored,
}

impl VerificationCheckRunStatus {
    pub fn from_receipt_status(status: ReceiptStatus) -> Self {
        match status {
            ReceiptStatus::Succeeded => Self::Succeeded,
            ReceiptStatus::Failed => Self::Failed,
            ReceiptStatus::Skipped => Self::Skipped,
            ReceiptStatus::Inconclusive => Self::Inconclusive,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationCheckRunEntry {
    pub run_id: VerificationCheckRunId,
    pub scope: EvidenceScope,
    pub check_spec_id: CheckSpecId,
    pub check_spec_hash: String,
    pub status: VerificationCheckRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl VerificationCheckRunEntry {
    pub fn new(
        run_id: VerificationCheckRunId,
        scope: EvidenceScope,
        check_spec: &CheckSpec,
        status: VerificationCheckRunStatus,
    ) -> Self {
        Self {
            run_id,
            scope,
            check_spec_id: check_spec.check_spec_id.clone(),
            check_spec_hash: check_spec.check_spec_hash.clone(),
            status,
            receipt_id: None,
            source_event_id: None,
            timeout_ms: None,
            reason: None,
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: Option<u64>) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    pub fn with_terminal_receipt(mut self, receipt: &VerificationReceipt) -> Self {
        self.status = VerificationCheckRunStatus::from_receipt_status(receipt.check_status);
        self.receipt_id = Some(receipt.receipt.receipt_id.clone());
        self.source_event_id = Some(receipt.receipt.source_event_id.clone());
        self.reason = if let Some(reason) = receipt.failure_reason.clone() {
            Some(reason)
        } else if receipt.mutates_verification_scope {
            Some("check mutated verification scope".to_owned())
        } else {
            None
        };
        self
    }

    pub fn with_error(mut self, reason: impl Into<String>) -> Self {
        self.status = VerificationCheckRunStatus::Errored;
        self.reason = Some(reason.into());
        self
    }
}

pub fn verification_check_run_id(
    scope: &EvidenceScope,
    check_spec: &CheckSpec,
    policy_hash: Option<&str>,
    workspace_snapshot_id: Option<&str>,
    attempt_sequence: u64,
) -> Result<VerificationCheckRunId> {
    let scope =
        serde_json::to_string(scope).context("failed to encode verification check scope")?;
    let seed = format!(
        "{}:{}:{}:{}:{}:{}",
        scope,
        check_spec.check_spec_id,
        check_spec.check_spec_hash,
        policy_hash.unwrap_or("-"),
        workspace_snapshot_id.unwrap_or("-"),
        attempt_sequence
    );
    Ok(stable_event_uuid("sigil-verification-check-run", &seed))
}

impl VerificationReceipt {
    pub fn is_applicable_to(
        &self,
        check: &CheckSpec,
        current_snapshot_id: &WorkspaceSnapshotId,
        scope: &VerificationScope,
        trust_requirement: WorkspaceTrustRequirement,
        workspace_trust: WorkspaceTrust,
        sandbox_requirement: SandboxProfileRequirement,
    ) -> bool {
        self.check_spec_id == check.check_spec_id
            && self.binding.check_spec_hash == check.check_spec_hash
            && self.binding.workspace_snapshot_id == *current_snapshot_id
            && self.binding.verification_scope_hash == scope.scope_hash
            && self.receipt.workspace_snapshot_id.as_ref() == Some(current_snapshot_id)
            && !self.mutates_verification_scope
            && receipt_satisfies_execution_trust(self, trust_requirement, workspace_trust)
            && receipt_satisfies_sandbox_profile(self, sandbox_requirement)
    }
}

fn receipt_satisfies_execution_trust(
    receipt: &VerificationReceipt,
    trust_requirement: WorkspaceTrustRequirement,
    workspace_trust: WorkspaceTrust,
) -> bool {
    match trust_requirement {
        WorkspaceTrustRequirement::None => true,
        WorkspaceTrustRequirement::ApprovalOrSandbox => {
            workspace_trust == WorkspaceTrust::Trusted
                || receipt.binding.approval_event_id.is_some()
                || receipt.binding.sandbox_decision_id.is_some()
        }
        WorkspaceTrustRequirement::Trusted => workspace_trust == WorkspaceTrust::Trusted,
    }
}

pub(super) fn receipt_matches_current_context(
    receipt: &VerificationReceipt,
    check: &CheckSpec,
    current_snapshot_id: &WorkspaceSnapshotId,
    scope: &VerificationScope,
    trust_requirement: WorkspaceTrustRequirement,
    workspace_trust: WorkspaceTrust,
    sandbox_requirement: SandboxProfileRequirement,
) -> bool {
    receipt.check_spec_id == check.check_spec_id
        && receipt.binding.check_spec_hash == check.check_spec_hash
        && receipt.binding.workspace_snapshot_id == *current_snapshot_id
        && receipt.binding.verification_scope_hash == scope.scope_hash
        && receipt.receipt.workspace_snapshot_id.as_ref() == Some(current_snapshot_id)
        && receipt_satisfies_execution_trust(receipt, trust_requirement, workspace_trust)
        && receipt_satisfies_sandbox_profile(receipt, sandbox_requirement)
}

fn receipt_satisfies_sandbox_profile(
    receipt: &VerificationReceipt,
    requirement: SandboxProfileRequirement,
) -> bool {
    match requirement {
        SandboxProfileRequirement::None => true,
        SandboxProfileRequirement::ApprovalOrSandbox => {
            receipt.binding.approval_event_id.is_some()
                || receipt.binding.sandbox_decision_id.is_some()
                || receipt_has_matching_sandbox_backend(receipt, requirement)
        }
        SandboxProfileRequirement::Sandboxed => {
            receipt_has_matching_sandbox_backend(receipt, requirement)
        }
    }
}

fn receipt_has_matching_sandbox_backend(
    receipt: &VerificationReceipt,
    requirement: SandboxProfileRequirement,
) -> bool {
    let Some(backend) = receipt.binding.execution_backend else {
        return false;
    };
    let Some(capabilities) = receipt.binding.execution_backend_capabilities else {
        return false;
    };
    capabilities.supports_required_sandbox()
        && receipt_network_is_consistent_with_capabilities(
            &receipt.binding.execution_network,
            capabilities,
        )
        && receipt.binding.sandbox_profile_hash
            == sandbox_profile_hash_for_execution(
                requirement,
                backend,
                capabilities,
                &receipt.binding.execution_network,
            )
}

fn receipt_network_is_consistent_with_capabilities(
    network: &ExecutionNetworkReceipt,
    capabilities: ExecutionBackendCapabilities,
) -> bool {
    match network.policy {
        crate::ExecutionNetworkPolicy::Denied => capabilities.network_isolation,
        crate::ExecutionNetworkPolicy::Allowed => true,
        crate::ExecutionNetworkPolicy::Unsupported => !capabilities.network_isolation,
        crate::ExecutionNetworkPolicy::Unknown => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceMutationEvidence {
    pub event_id: EventId,
    pub source_event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_hint: Option<String>,
    pub scope_hash: VerificationScopeHash,
    pub recorded_at_stream_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub tool_effect: ToolEffect,
    pub unknown_dirty: bool,
}

impl WorkspaceMutationEvidence {
    pub fn from_detected_event(
        event_id: EventId,
        recorded_at_stream_sequence: u64,
        payload: WorkspaceMutationDetected,
    ) -> Self {
        let (source_label, recovery_hint) = unknown_mutation_source_context(&payload.tool_name);
        Self {
            event_id,
            source_event_type: DurableEventType::WorkspaceMutationDetected
                .as_str()
                .to_owned(),
            source_label,
            recovery_hint,
            scope_hash: payload.scope_hash,
            recorded_at_stream_sequence,
            from_workspace_snapshot_id: payload.from_workspace_snapshot_id,
            to_workspace_snapshot_id: payload.to_workspace_snapshot_id,
            tool_effect: payload.tool_effect,
            unknown_dirty: payload.unknown_dirty,
        }
    }

    pub fn invalidates_scope(&self, scope: &VerificationScope) -> bool {
        self.unknown_dirty || self.scope_hash == scope.scope_hash
    }

    pub(super) fn source_readiness_reason(&self) -> Option<ReadinessReason> {
        if !self.unknown_dirty {
            return None;
        }
        Some(ReadinessReason::WorkspaceMutationSource {
            event_id: self.event_id.clone(),
            source_label: self.source_label.clone()?,
            recovery_hint: self.recovery_hint.clone(),
        })
    }
}

fn unknown_mutation_source_context(tool_name: &str) -> (Option<String>, Option<String>) {
    if let Some(server_name) = tool_name.strip_prefix("mcp_server:") {
        return (
            Some(format!("MCP server {server_name}")),
            Some("refresh MCP or run check".to_owned()),
        );
    }
    (None, None)
}
