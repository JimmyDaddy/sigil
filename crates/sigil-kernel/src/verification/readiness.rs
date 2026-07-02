use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "reason", content = "event_id")]
pub enum VerificationStaleReason {
    WorkspaceChanged(EventId),
    CheckSpecChanged(EventId),
    PolicyChanged(EventId),
    EnvironmentChanged(EventId),
    SandboxChanged(EventId),
    TrustChanged(EventId),
    UnknownDirty(EventId),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationStaleCause {
    pub reason: VerificationStaleReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationSkipDecision {
    pub event_id: EventId,
    pub reason: String,
}

/// Durable control entry recording a workspace trust decision relevant to verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceTrustDecisionEntry {
    pub workspace_id: WorkspaceId,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    pub trust: WorkspaceTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessInput {
    pub run_status: RunStatus,
    pub projection_mode: ReadinessProjectionMode,
    pub policy: VerificationPolicy,
    pub workspace_trust: WorkspaceTrust,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_trust_approval_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_trust_sandbox_decision_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub workspace_knowledge: WorkspaceKnowledge,
    #[serde(default)]
    pub verification_receipts: Vec<VerificationReceipt>,
    #[serde(default)]
    pub mutations: Vec<WorkspaceMutationEvidence>,
    #[serde(default)]
    pub stale_causes: Vec<VerificationStaleCause>,
    #[serde(default)]
    pub pending_checks: Vec<CheckSpecId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_decision: Option<VerificationSkipDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_assistant_event_id: Option<EventId>,
    #[serde(default)]
    pub recovered_tool_error_event_ids: Vec<EventId>,
}

impl ReadinessInput {
    pub fn new_run(run_status: RunStatus, policy: VerificationPolicy) -> Self {
        Self {
            run_status,
            projection_mode: ReadinessProjectionMode::NewRun,
            policy,
            workspace_trust: WorkspaceTrust::Unknown,
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
            current_workspace_snapshot_id: None,
            workspace_knowledge: WorkspaceKnowledge::Clean(0),
            verification_receipts: Vec::new(),
            mutations: Vec::new(),
            stale_causes: Vec::new(),
            pending_checks: Vec::new(),
            skip_decision: None,
            final_assistant_event_id: None,
            recovered_tool_error_event_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessProjectionMode {
    NewRun,
    LegacyProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessEvaluation {
    pub run_status: RunStatus,
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
    #[serde(default)]
    pub reasons: Vec<ReadinessReason>,
    #[serde(default)]
    pub required_actions: Vec<RequiredAction>,
}

/// Durable control entry recording a system-computed readiness verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ReadinessEvaluatedEntry {
    pub scope: EvidenceScope,
    pub evaluation: ReadinessEvaluation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<PolicyHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}

/// Materialized verification view reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerificationStateProjection {
    pub check_specs: BTreeMap<(EvidenceScope, CheckSpecId), CheckSpecRecordedEntry>,
    pub policies: BTreeMap<EvidenceScope, VerificationPolicyChangedEntry>,
    pub check_runs: BTreeMap<VerificationCheckRunId, VerificationCheckRunEntry>,
    pub receipts: BTreeMap<ReceiptId, VerificationRecordedEntry>,
    pub readiness: BTreeMap<EvidenceScope, ReadinessEvaluatedEntry>,
    pub child_receipt_links: Vec<ChildVerificationReceiptLinked>,
    pub workspace_trust: BTreeMap<WorkspaceId, WorkspaceTrustDecisionEntry>,
}

impl VerificationStateProjection {
    /// Replays session entries into a verification projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            projection.apply_control_entry(control);
        }
        projection
    }

    pub fn latest_policy(&self, scope: &EvidenceScope) -> Option<&VerificationPolicyChangedEntry> {
        self.policies.get(scope)
    }

    pub fn latest_readiness(&self, scope: &EvidenceScope) -> Option<&ReadinessEvaluatedEntry> {
        self.readiness.get(scope)
    }

    pub fn receipt(&self, receipt_id: &str) -> Option<&VerificationRecordedEntry> {
        self.receipts.get(receipt_id)
    }

    pub fn check_run(&self, run_id: &str) -> Option<&VerificationCheckRunEntry> {
        self.check_runs.get(run_id)
    }

    pub fn check_spec(
        &self,
        scope: &EvidenceScope,
        check_spec_id: &str,
    ) -> Option<&CheckSpecRecordedEntry> {
        self.check_specs
            .get(&(scope.clone(), check_spec_id.to_owned()))
    }

    pub fn check_specs_for_scopes(
        &self,
        scopes_by_precedence: &[EvidenceScope],
    ) -> Vec<&CheckSpecRecordedEntry> {
        let mut selected = BTreeMap::<CheckSpecId, &CheckSpecRecordedEntry>::new();
        for scope in scopes_by_precedence.iter().rev() {
            for ((entry_scope, check_spec_id), entry) in &self.check_specs {
                if entry_scope == scope {
                    selected.insert(check_spec_id.clone(), entry);
                }
            }
        }
        selected.into_values().collect()
    }

    pub fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::CheckSpecRecorded(entry) => {
                self.check_specs.insert(
                    (
                        entry.scope.clone(),
                        entry.trusted_check.check_spec.check_spec_id.clone(),
                    ),
                    entry.clone(),
                );
            }
            ControlEntry::VerificationPolicyChanged(entry) => {
                self.policies.insert(entry.scope.clone(), entry.clone());
            }
            ControlEntry::VerificationCheckRun(entry) => {
                self.check_runs.insert(entry.run_id.clone(), entry.clone());
            }
            ControlEntry::VerificationRecorded(entry) => {
                self.receipts
                    .insert(entry.receipt.receipt.receipt_id.clone(), entry.clone());
            }
            ControlEntry::ReadinessEvaluated(entry) => {
                self.readiness.insert(entry.scope.clone(), entry.clone());
            }
            ControlEntry::ChildVerificationReceiptLinked(entry) => {
                self.child_receipt_links.push(entry.clone());
            }
            ControlEntry::WorkspaceTrustDecision(entry) => {
                self.workspace_trust
                    .insert(entry.workspace_id.clone(), entry.clone());
            }
            _ => {}
        }
    }

    pub fn apply_control(&mut self, control: &ControlEntry) {
        self.apply_control_entry(control);
    }
}

/// JSON-friendly persisted form of `VerificationStateProjection`.
///
/// The runtime projection uses map keys that are not ideal JSON object keys. The persisted snapshot
/// keeps the same materialized facts as ordered entry vectors so a projection store can be rebuilt
/// from JSONL and reloaded without reparsing the full session stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationStateProjectionSnapshot {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_specs: Vec<CheckSpecRecordedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<VerificationPolicyChangedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check_runs: Vec<VerificationCheckRunEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<VerificationRecordedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness: Vec<ReadinessEvaluatedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_receipt_links: Vec<ChildVerificationReceiptLinked>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_trust: Vec<WorkspaceTrustDecisionEntry>,
}

impl From<&VerificationStateProjection> for VerificationStateProjectionSnapshot {
    fn from(projection: &VerificationStateProjection) -> Self {
        Self {
            check_specs: projection.check_specs.values().cloned().collect(),
            policies: projection.policies.values().cloned().collect(),
            check_runs: projection.check_runs.values().cloned().collect(),
            receipts: projection.receipts.values().cloned().collect(),
            readiness: projection.readiness.values().cloned().collect(),
            child_receipt_links: projection.child_receipt_links.clone(),
            workspace_trust: projection.workspace_trust.values().cloned().collect(),
        }
    }
}

impl From<VerificationStateProjectionSnapshot> for VerificationStateProjection {
    fn from(snapshot: VerificationStateProjectionSnapshot) -> Self {
        let mut projection = Self::default();
        for entry in snapshot.check_specs {
            projection.apply_control_entry(&ControlEntry::CheckSpecRecorded(entry));
        }
        for entry in snapshot.policies {
            projection.apply_control_entry(&ControlEntry::VerificationPolicyChanged(entry));
        }
        for entry in snapshot.check_runs {
            projection.apply_control_entry(&ControlEntry::VerificationCheckRun(entry));
        }
        for entry in snapshot.receipts {
            projection.apply_control_entry(&ControlEntry::VerificationRecorded(entry));
        }
        for entry in snapshot.readiness {
            projection.apply_control_entry(&ControlEntry::ReadinessEvaluated(entry));
        }
        for entry in snapshot.child_receipt_links {
            projection.apply_control_entry(&ControlEntry::ChildVerificationReceiptLinked(entry));
        }
        for entry in snapshot.workspace_trust {
            projection.apply_control_entry(&ControlEntry::WorkspaceTrustDecision(entry));
        }
        projection
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "reason", content = "details")]
pub enum ReadinessReason {
    LegacyEvidenceUnavailable,
    NoVerificationRequired,
    FinalAssistantTextIgnored {
        event_id: EventId,
    },
    RecoveredToolError {
        event_id: EventId,
    },
    WorkspaceTrustUnsatisfied,
    PendingCheckReducedForTerminalRun {
        check_spec_id: CheckSpecId,
    },
    MissingRequiredCheck {
        check_spec_id: CheckSpecId,
    },
    VerificationPassed {
        receipt_id: ReceiptId,
    },
    VerificationFailed {
        receipt_id: ReceiptId,
    },
    VerificationSkipped {
        event_id: EventId,
    },
    VerificationStale(VerificationStaleCause),
    WorkspaceMutationSource {
        event_id: EventId,
        source_label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery_hint: Option<String>,
    },
    WorkspaceUnknownDirty {
        event_id: Option<EventId>,
    },
    CheckMutatedVerificationScope {
        check_spec_id: CheckSpecId,
    },
    ReceiptScopeMismatch {
        receipt_id: ReceiptId,
    },
    ReceiptSnapshotMismatch {
        receipt_id: ReceiptId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "action", content = "details")]
pub enum RequiredAction {
    RunCheck { check_spec_id: CheckSpecId },
    ApproveCheckExecution { check_spec_id: CheckSpecId },
    TrustWorkspace,
    ResolveUnknownDirty,
    ReRunNonWritingCheck { check_spec_id: CheckSpecId },
    ReviewVerificationFailure { receipt_id: ReceiptId },
    ProvideVerificationConfig,
}

/// Computes a verification verdict from typed evidence.
pub fn evaluate_readiness(input: &ReadinessInput) -> ReadinessEvaluation {
    let mut reasons = Vec::new();
    let mut required_actions = Vec::new();

    if let Some(event_id) = &input.final_assistant_event_id {
        reasons.push(ReadinessReason::FinalAssistantTextIgnored {
            event_id: event_id.clone(),
        });
    }
    for event_id in &input.recovered_tool_error_event_ids {
        reasons.push(ReadinessReason::RecoveredToolError {
            event_id: event_id.clone(),
        });
    }

    if input.projection_mode == ReadinessProjectionMode::LegacyProjection {
        reasons.push(ReadinessReason::LegacyEvidenceUnavailable);
        return evaluation(
            input.run_status,
            VerificationVerdict::NotEvaluated,
            reasons,
            required_actions,
        );
    }

    if !input.policy.workspace_trust_requirement.is_satisfied(
        input.workspace_trust,
        input.workspace_trust_approval_event_id.as_ref(),
        input.workspace_trust_sandbox_decision_id.as_ref(),
    ) {
        reasons.push(ReadinessReason::WorkspaceTrustUnsatisfied);
        required_actions.push(RequiredAction::TrustWorkspace);
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Missing,
            reasons,
            required_actions,
        );
    }

    if !input.pending_checks.is_empty() {
        if input.run_status.is_terminal() {
            for check_spec_id in &input.pending_checks {
                reasons.push(ReadinessReason::PendingCheckReducedForTerminalRun {
                    check_spec_id: check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::RunCheck {
                    check_spec_id: check_spec_id.clone(),
                });
            }
            return finalize_new_run(
                input.run_status,
                VerificationVerdict::Inconclusive,
                reasons,
                required_actions,
            );
        }
        return evaluation(
            input.run_status,
            VerificationVerdict::Pending,
            reasons,
            required_actions,
        );
    }

    if input.policy.required_checks.is_empty() {
        if has_relevant_mutation(input) {
            return missing_for_mutation(input, reasons, required_actions);
        }
        reasons.push(ReadinessReason::NoVerificationRequired);
        return evaluation(
            input.run_status,
            VerificationVerdict::NotApplicable,
            reasons,
            required_actions,
        );
    }

    if let Some(skip) = &input.skip_decision
        && input.policy.allow_unverified_completion
    {
        reasons.push(ReadinessReason::VerificationSkipped {
            event_id: skip.event_id.clone(),
        });
        return evaluation(
            input.run_status,
            VerificationVerdict::Skipped,
            reasons,
            required_actions,
        );
    }

    let prior_passed = input
        .verification_receipts
        .iter()
        .any(|receipt| receipt.check_status == ReceiptStatus::Succeeded);
    if input.workspace_knowledge.is_unknown_dirty() {
        let unknown_dirty_mutation = input
            .mutations
            .iter()
            .find(|mutation| mutation.unknown_dirty);
        if let Some(reason) =
            unknown_dirty_mutation.and_then(WorkspaceMutationEvidence::source_readiness_reason)
        {
            reasons.push(reason);
        }
        let event_id = unknown_dirty_mutation.map(|mutation| mutation.event_id.clone());
        reasons.push(ReadinessReason::WorkspaceUnknownDirty {
            event_id: event_id.clone(),
        });
        required_actions.push(RequiredAction::ResolveUnknownDirty);
        let verdict = if event_id.is_some() {
            if prior_passed {
                VerificationVerdict::Stale
            } else {
                VerificationVerdict::Inconclusive
            }
        } else {
            VerificationVerdict::Inconclusive
        };
        if verdict == VerificationVerdict::Stale {
            reasons.push(ReadinessReason::VerificationStale(VerificationStaleCause {
                reason: VerificationStaleReason::UnknownDirty(
                    input
                        .mutations
                        .iter()
                        .find(|mutation| mutation.unknown_dirty)
                        .map(|mutation| mutation.event_id.clone())
                        .unwrap_or_else(|| "unknown_dirty".to_owned()),
                ),
                from_workspace_snapshot_id: None,
                to_workspace_snapshot_id: None,
            }));
        }
        return finalize_new_run(input.run_status, verdict, reasons, required_actions);
    }

    if let Some(stale_cause) = latest_stale_cause(input) {
        reasons.push(ReadinessReason::VerificationStale(stale_cause));
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Stale,
            reasons,
            required_actions,
        );
    }

    let Some(current_snapshot_id) = &input.current_workspace_snapshot_id else {
        required_actions.push(RequiredAction::RunCheck {
            check_spec_id: input.policy.required_checks[0].check_spec_id.clone(),
        });
        reasons.push(ReadinessReason::MissingRequiredCheck {
            check_spec_id: input.policy.required_checks[0].check_spec_id.clone(),
        });
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Missing,
            reasons,
            required_actions,
        );
    };

    let mut any_passed = false;
    let mut first_failed: Option<ReceiptId> = None;
    for check in &input.policy.required_checks {
        let receipts = input
            .verification_receipts
            .iter()
            .filter(|receipt| receipt.check_spec_id == check.check_spec_id)
            .collect::<Vec<_>>();
        let current_receipt = receipts
            .iter()
            .copied()
            .filter(|receipt| {
                receipt_matches_current_context(
                    receipt,
                    check,
                    current_snapshot_id,
                    &input.policy.verification_scope,
                    input.policy.workspace_trust_requirement,
                    input.workspace_trust,
                    input.policy.sandbox_profile,
                )
            })
            .max_by_key(|receipt| receipt.receipt.recorded_at_stream_sequence);

        match current_receipt.map(|receipt| (receipt.check_status, receipt)) {
            Some((ReceiptStatus::Succeeded, receipt)) if !receipt.mutates_verification_scope => {
                any_passed = true;
                reasons.push(ReadinessReason::VerificationPassed {
                    receipt_id: receipt.receipt.receipt_id.clone(),
                });
            }
            Some((ReceiptStatus::Succeeded, _receipt)) => {
                reasons.push(ReadinessReason::CheckMutatedVerificationScope {
                    check_spec_id: check.check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::ReRunNonWritingCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Missing,
                        reasons,
                        required_actions,
                    );
                }
            }
            Some((ReceiptStatus::Failed, receipt)) => {
                reasons.push(ReadinessReason::VerificationFailed {
                    receipt_id: receipt.receipt.receipt_id.clone(),
                });
                first_failed.get_or_insert_with(|| receipt.receipt.receipt_id.clone());
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    required_actions.push(RequiredAction::ReviewVerificationFailure {
                        receipt_id: receipt.receipt.receipt_id.clone(),
                    });
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Failed,
                        reasons,
                        required_actions,
                    );
                }
            }
            Some((ReceiptStatus::Skipped | ReceiptStatus::Inconclusive, receipt)) => {
                reasons.push(ReadinessReason::MissingRequiredCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::RunCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                if receipt.check_status == ReceiptStatus::Inconclusive
                    && input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks
                {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Inconclusive,
                        reasons,
                        required_actions,
                    );
                }
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Missing,
                        reasons,
                        required_actions,
                    );
                }
            }
            None => {
                if let Some(receipt) = receipts.first() {
                    if receipt.binding.verification_scope_hash
                        != input.policy.verification_scope.scope_hash
                    {
                        reasons.push(ReadinessReason::ReceiptScopeMismatch {
                            receipt_id: receipt.receipt.receipt_id.clone(),
                        });
                    } else if receipt.binding.workspace_snapshot_id != *current_snapshot_id {
                        reasons.push(ReadinessReason::ReceiptSnapshotMismatch {
                            receipt_id: receipt.receipt.receipt_id.clone(),
                        });
                    }
                }
                reasons.push(ReadinessReason::MissingRequiredCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                required_actions.push(RequiredAction::RunCheck {
                    check_spec_id: check.check_spec_id.clone(),
                });
                if input.policy.completion_criteria == CompletionCriteria::AllRequiredChecks {
                    return finalize_new_run(
                        input.run_status,
                        VerificationVerdict::Missing,
                        reasons,
                        required_actions,
                    );
                }
            }
        }
    }

    match input.policy.completion_criteria {
        CompletionCriteria::NoChecksRequired => {
            reasons.push(ReadinessReason::NoVerificationRequired);
            evaluation(
                input.run_status,
                VerificationVerdict::NotApplicable,
                reasons,
                required_actions,
            )
        }
        CompletionCriteria::AnyRequiredCheck if any_passed => evaluation(
            input.run_status,
            VerificationVerdict::Passed,
            reasons,
            required_actions,
        ),
        CompletionCriteria::AllRequiredChecks if any_passed => evaluation(
            input.run_status,
            VerificationVerdict::Passed,
            reasons,
            required_actions,
        ),
        CompletionCriteria::AnyRequiredCheck | CompletionCriteria::AllRequiredChecks => {
            if let Some(receipt_id) = first_failed {
                required_actions.push(RequiredAction::ReviewVerificationFailure {
                    receipt_id: receipt_id.clone(),
                });
                return finalize_new_run(
                    input.run_status,
                    VerificationVerdict::Failed,
                    reasons,
                    required_actions,
                );
            }
            if required_actions.is_empty() {
                required_actions.push(RequiredAction::ProvideVerificationConfig);
            }
            finalize_new_run(
                input.run_status,
                VerificationVerdict::Missing,
                reasons,
                required_actions,
            )
        }
    }
}

fn has_relevant_mutation(input: &ReadinessInput) -> bool {
    matches!(
        input.workspace_knowledge,
        WorkspaceKnowledge::Dirty(_) | WorkspaceKnowledge::UnknownDirty
    ) || input
        .mutations
        .iter()
        .any(|mutation| mutation.invalidates_scope(&input.policy.verification_scope))
}

fn missing_for_mutation(
    input: &ReadinessInput,
    mut reasons: Vec<ReadinessReason>,
    mut required_actions: Vec<RequiredAction>,
) -> ReadinessEvaluation {
    if input.workspace_knowledge.is_unknown_dirty() {
        let unknown_dirty_mutation = input
            .mutations
            .iter()
            .find(|mutation| mutation.unknown_dirty);
        if let Some(reason) =
            unknown_dirty_mutation.and_then(WorkspaceMutationEvidence::source_readiness_reason)
        {
            reasons.push(reason);
        }
        reasons.push(ReadinessReason::WorkspaceUnknownDirty {
            event_id: unknown_dirty_mutation.map(|mutation| mutation.event_id.clone()),
        });
        required_actions.push(RequiredAction::ResolveUnknownDirty);
        return finalize_new_run(
            input.run_status,
            VerificationVerdict::Inconclusive,
            reasons,
            required_actions,
        );
    }
    required_actions.push(RequiredAction::ProvideVerificationConfig);
    finalize_new_run(
        input.run_status,
        VerificationVerdict::Missing,
        reasons,
        required_actions,
    )
}

fn latest_stale_cause(input: &ReadinessInput) -> Option<VerificationStaleCause> {
    if let Some(cause) = input.stale_causes.last() {
        return Some(cause.clone());
    }
    let latest_pass_sequence = input
        .verification_receipts
        .iter()
        .filter(|receipt| receipt.check_status == ReceiptStatus::Succeeded)
        .map(|receipt| receipt.receipt.recorded_at_stream_sequence)
        .max()?;
    input
        .mutations
        .iter()
        .filter(|mutation| {
            mutation.recorded_at_stream_sequence > latest_pass_sequence
                && mutation.invalidates_scope(&input.policy.verification_scope)
        })
        .max_by_key(|mutation| mutation.recorded_at_stream_sequence)
        .map(|mutation| VerificationStaleCause {
            reason: if mutation.unknown_dirty {
                VerificationStaleReason::UnknownDirty(mutation.event_id.clone())
            } else {
                VerificationStaleReason::WorkspaceChanged(mutation.event_id.clone())
            },
            from_workspace_snapshot_id: mutation.from_workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: mutation.to_workspace_snapshot_id.clone(),
        })
}

pub(super) fn finalize_new_run(
    run_status: RunStatus,
    verdict: VerificationVerdict,
    mut reasons: Vec<ReadinessReason>,
    required_actions: Vec<RequiredAction>,
) -> ReadinessEvaluation {
    let verdict = if run_status.is_terminal() && verdict == VerificationVerdict::Pending {
        reasons.push(ReadinessReason::PendingCheckReducedForTerminalRun {
            check_spec_id: "unknown".to_owned(),
        });
        VerificationVerdict::Inconclusive
    } else if run_status.is_terminal() && verdict == VerificationVerdict::NotEvaluated {
        VerificationVerdict::Missing
    } else {
        verdict
    };
    evaluation(run_status, verdict, reasons, required_actions)
}

fn evaluation(
    run_status: RunStatus,
    verification_verdict: VerificationVerdict,
    reasons: Vec<ReadinessReason>,
    required_actions: Vec<RequiredAction>,
) -> ReadinessEvaluation {
    ReadinessEvaluation {
        run_status,
        verification_verdict,
        visible_state: VisibleCompletionState::derive(run_status, verification_verdict),
        reasons,
        required_actions,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChildVerificationReceiptLinked {
    pub parent_session_id: SessionId,
    pub child_session_id: SessionId,
    pub child_receipt_id: ReceiptId,
    pub child_event_id: EventId,
    pub child_workspace_id: WorkspaceId,
    pub child_workspace_snapshot_id: WorkspaceSnapshotId,
    pub policy_hash: PolicyHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_id: Option<ChangesetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_event_id: Option<EventId>,
}

impl ChildVerificationReceiptLinked {
    /// Validates that parent projections can trace child evidence across session boundaries.
    ///
    /// # Errors
    ///
    /// Returns an error when mandatory child source identifiers are missing.
    pub fn validate(&self) -> Result<()> {
        if self.parent_session_id.trim().is_empty()
            || self.child_session_id.trim().is_empty()
            || self.child_receipt_id.trim().is_empty()
            || self.child_event_id.trim().is_empty()
            || self.child_workspace_id.trim().is_empty()
            || self.child_workspace_snapshot_id.trim().is_empty()
            || self.policy_hash.trim().is_empty()
        {
            bail!("child verification receipt link is missing required identity");
        }
        Ok(())
    }
}
