use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, ReceiptStatus, SessionLogEntry, SessionStreamRecord,
    TaskVerificationFeedbackStatusV1, TaskVerificationFeedbackV1, ToolApprovalAuditAction,
    ToolArtifactCompleteness, ToolCall, ToolExecutionEntry, ToolExecutionStatus,
    ToolPolicyCompletenessV1, ToolResultRecordedV3, ToolSourceCompletenessV1,
    ToolStorageCompletenessV1, ToolSubjectKind, ToolSubjectScope, VerificationReceipt,
};

/// Observed facts in one durable session stream, not a whole Task tree or semantic quality score.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelEvalActivityMetrics {
    pub scope: &'static str,
    /// Child streams are not traversed by this projection.
    pub observed_child_thread_starts: usize,
    pub reads: ModelEvalReadMetrics,
    pub decisions: ModelEvalDecisionMetrics,
    pub repairs: ModelEvalRepairMetrics,
}

/// Repeated output observations do not establish unnecessary reads or absence of intervening edits.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelEvalReadMetrics {
    pub completed_read_events: usize,
    pub qualified_reads: usize,
    pub canonical_subject_reads: usize,
    /// Exact managed workspace/logical selector bindings, not canonical leaf identities.
    pub managed_logical_subject_reads: usize,
    pub excluded_completed_read_events: usize,
    /// Null when no read has the complete exact-call/range/artifact binding.
    pub repeated_output_reads: Option<usize>,
    /// Bytes of persistence-safe output artifacts, never physical file I/O bytes.
    pub repeated_persisted_output_bytes: Option<u64>,
}

/// Accepted decisions carry no trustworthy human-versus-automation provenance.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelEvalDecisionMetrics {
    pub tool_decision_events: usize,
    pub unique_accepted_tool_decisions: usize,
    pub user_input_decision_events: usize,
    pub unique_accepted_user_input_decisions: usize,
    pub unmatched_or_conflicting_decision_events: usize,
}

/// Exact selected repair followed by a comparable failed check; not an ineffective-edit classifier.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelEvalRepairMetrics {
    pub selected_repairs: usize,
    pub repairs_with_next_feedback: usize,
    pub comparable_recheck_pairs: usize,
    /// Number of comparable feedback pairs sharing at least one still-failed check.
    pub failed_rechecks_after_repair: Option<usize>,
    pub repairs_without_next_feedback: usize,
    pub excluded_recheck_pairs: usize,
    pub unmatched_repair_selections: usize,
}

pub(super) fn observe_activity(
    records: &[SessionStreamRecord],
) -> Result<ModelEvalActivityMetrics> {
    let mut result = ModelEvalActivityMetrics {
        scope: "observed_session_stream",
        ..ModelEvalActivityMetrics::default()
    };
    let Some(first) = records.first() else {
        return Ok(result);
    };
    let scope = &first.stored_event().session_id;
    ensure!(
        records
            .iter()
            .all(|record| record.stored_event().session_id == *scope),
        "activity observations span different session streams"
    );
    let entries = records
        .iter()
        .map(SessionStreamRecord::session_log_entry)
        .collect::<Result<Vec<_>>>()?;
    // Call IDs are not globally unique across provider turns. An ambiguous ID is excluded, not
    // joined to whichever occurrence happens to be last in the transcript.
    let mut calls: BTreeMap<&str, Vec<(usize, &ToolCall)>> = BTreeMap::new();
    let mut executions: BTreeMap<&str, Vec<(usize, &ToolExecutionEntry)>> = BTreeMap::new();
    let mut outputs: BTreeMap<&str, Vec<(usize, &ToolResultRecordedV3)>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Some(SessionLogEntry::Assistant(message)) => {
                for call in &message.tool_calls {
                    calls.entry(&call.id).or_default().push((index, call));
                }
            }
            Some(SessionLogEntry::ToolResultV3(output)) => {
                outputs
                    .entry(&output.call_id)
                    .or_default()
                    .push((index, output));
            }
            Some(SessionLogEntry::Control(ControlEntry::ToolExecution(execution))) => {
                executions
                    .entry(&execution.call_id)
                    .or_default()
                    .push((index, execution));
                if execution.tool_name == "read_file"
                    && execution.status == ToolExecutionStatus::Completed
                {
                    result.reads.completed_read_events += 1;
                }
            }
            Some(SessionLogEntry::Control(ControlEntry::AgentThreadStarted(_))) => {
                result.observed_child_thread_starts += 1;
            }
            _ => {}
        }
    }
    let mut seen = BTreeSet::new();
    let mut repeats = 0;
    let mut bytes = 0_u64;
    for (call_id, audits) in executions {
        let Some([(call_index, call)]) = calls.get(call_id).map(Vec::as_slice) else {
            continue;
        };
        let Some([(output_index, output)]) = outputs.get(call_id).map(Vec::as_slice) else {
            continue;
        };
        let [(start_index, start), (end_index, end)] = audits.as_slice() else {
            continue;
        };
        if *call_index >= *start_index
            || start_index >= end_index
            || *start_index >= *output_index
            || start.status != ToolExecutionStatus::Started
            || end.status != ToolExecutionStatus::Completed
            || start.tool_name != "read_file"
            || end.tool_name != "read_file"
            || call.name != "read_file"
            || start.subjects != end.subjects
            || start.metadata.details.get("call") != end.metadata.details.get("call")
        {
            continue;
        }
        let Some(read) = read_fingerprint(call, start, end, output, scope) else {
            continue;
        };
        result.reads.qualified_reads += 1;
        match read.identity {
            ReadIdentity::CanonicalSubject => result.reads.canonical_subject_reads += 1,
            ReadIdentity::ManagedLogicalSubject => result.reads.managed_logical_subject_reads += 1,
        }
        if !seen.insert(read.fingerprint) {
            repeats += 1;
            bytes = bytes
                .checked_add(read.persisted_bytes)
                .context("activity persisted output byte count overflow")?;
        }
    }
    result.reads.excluded_completed_read_events =
        result.reads.completed_read_events - result.reads.qualified_reads;
    if result.reads.qualified_reads > 0 {
        result.reads.repeated_output_reads = Some(repeats);
        result.reads.repeated_persisted_output_bytes = Some(bytes);
    }
    observe_decisions(&entries, scope, &mut result.decisions)?;
    observe_repairs(&entries, scope, &mut result.repairs);
    Ok(result)
}

enum ReadIdentity {
    CanonicalSubject,
    ManagedLogicalSubject,
}

struct ReadObservation {
    identity: ReadIdentity,
    fingerprint: String,
    persisted_bytes: u64,
}

fn read_fingerprint(
    call: &ToolCall,
    started: &ToolExecutionEntry,
    execution: &ToolExecutionEntry,
    output: &ToolResultRecordedV3,
    scope: &str,
) -> Option<ReadObservation> {
    let args: serde_json::Value = serde_json::from_str(&call.args_json).ok()?;
    let path = args.get("path")?.as_str()?;
    let offset = match args.get("offset") {
        Some(value) => value.as_u64()?,
        None => 0,
    };
    let requested_limit = match args.get("limit") {
        Some(value) => Some(value.as_u64()?),
        None => None,
    };
    let path_hash = format!("{:x}", Sha256::digest(path.as_bytes()));
    if execution
        .metadata
        .details
        .pointer("/call/path_sha256")?
        .as_str()?
        != path_hash
        || execution.error.is_some()
        || execution.metadata.truncated
        || output.tool_name != "read_file"
        || output.call_id != call.id
        || output.facts.error.is_some()
        || output.facts.status != "ok"
    {
        return None;
    }
    let [subject] = execution.subjects.as_slice() else {
        return None;
    };
    if subject.kind != ToolSubjectKind::Path {
        return None;
    }
    let (identity, subject_binding) = if let Some(canonical) = &subject.canonical_path_sha256 {
        (
            ReadIdentity::CanonicalSubject,
            serde_json::json!(["canonical_subject", canonical]),
        )
    } else {
        // Builtin managed read_file deliberately keeps physical resolution in RA. Its durable
        // subject is the logical selector; the result's existing receipt binds that selector
        // and operation to the borrowed workspace. Never relabel this as a leaf inode identity.
        if subject.scope != ToolSubjectScope::Workspace
            || subject.identity_sha256 != sigil_kernel::stable_event_hash(path.as_bytes())
            || started
                .metadata
                .details
                .get("permission_plan_hash")?
                .as_str()?
                .is_empty()
        {
            return None;
        }
        sigil_kernel::managed_file_access::ManagedFileLogicalPathV1::new(path).ok()?;
        let receipt: sigil_kernel::managed_execution::BorrowedResourceAccessReceiptV1 =
            serde_json::from_value(
                output
                    .facts
                    .tool_specific
                    .get("managed_access_receipt")?
                    .clone(),
            )
            .ok()?;
        let root_identity = receipt.identity_before?;
        let zero = sigil_kernel::resource::CanonicalHash::from_bytes([0; 32]);
        if receipt.effect_settlement != sigil_kernel::recovery::EffectSettlementV1::Applied
            || receipt.subject_ref.as_str().is_empty()
            || [
                root_identity,
                receipt.subject_binding_hash,
                receipt.operation_digest,
                receipt.granted_access_hash,
                receipt.borrowed_effect_frontier_hash,
                receipt.receipt_hash,
            ]
            .contains(&zero)
        {
            return None;
        }
        (
            ReadIdentity::ManagedLogicalSubject,
            serde_json::json!([
                "managed_workspace_logical_selector",
                subject.identity_sha256,
                receipt.subject_ref,
                root_identity,
                receipt.subject_binding_hash,
                receipt.operation_digest,
            ]),
        )
    };
    let descriptor = output.artifact.descriptor()?;
    if descriptor.validate().is_err()
        || descriptor.tool_call_id != call.id
        || descriptor.tool_name != call.name
        || descriptor.session_scope_id_hash != sigil_kernel::stable_event_hash(scope.as_bytes())
        || !matches!(descriptor.completeness, ToolArtifactCompleteness::Complete)
        || output.capture_completeness.source != ToolSourceCompletenessV1::Complete
        || output.capture_completeness.policy != ToolPolicyCompletenessV1::Preserved
        || output.capture_completeness.storage != ToolStorageCompletenessV1::Complete
    {
        return None;
    }
    // Use the actual applied cap, plus the typed requested range. Persisted body hashes and byte
    // counts already describe the safe output; no artifact body or current filesystem is read.
    let fingerprint = serde_json::json!([
        subject_binding,
        offset,
        requested_limit,
        execution.metadata.limit_lines?,
        execution.metadata.limit_bytes?,
        descriptor.content_sha256,
        descriptor.persisted_bytes,
    ])
    .to_string();
    Some(ReadObservation {
        identity,
        fingerprint,
        persisted_bytes: descriptor.persisted_bytes,
    })
}

fn observe_decisions(
    entries: &[Option<SessionLogEntry>],
    scope: &str,
    counts: &mut ModelEvalDecisionMetrics,
) -> Result<()> {
    // Resolve all request identities before attributing any decision. A conflicting reuse of
    // one request ID invalidates even a decision that appeared before the later request.
    let mut tool_requests: BTreeMap<&str, Option<(usize, String)>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(SessionLogEntry::Control(ControlEntry::ToolApproval(approval))) = entry else {
            continue;
        };
        if approval.action == ToolApprovalAuditAction::Requested
            && approval.identity.session_id == scope
        {
            let identity = serde_json::to_string(&approval.identity)?;
            tool_requests
                .entry(&approval.identity.approval_request_id)
                .and_modify(|previous| {
                    if previous
                        .as_ref()
                        .is_some_and(|(_, recorded)| recorded != &identity)
                    {
                        *previous = None;
                    }
                })
                .or_insert(Some((index, identity)));
        }
    }
    let mut input_requests = BTreeMap::new();
    let mut accepted_tools = BTreeMap::new();
    let mut accepted_inputs = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(entry) = entry else { continue };
        match entry {
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval)) => {
                let id = &approval.identity.approval_request_id;
                let identity = serde_json::to_string(&approval.identity)?;
                if approval.action == ToolApprovalAuditAction::DecisionAccepted {
                    counts.tool_decision_events += 1;
                    let receipt = approval.decision_receipt.as_ref();
                    if approval.identity.session_id != scope
                        || !tool_requests.get(id.as_str()).is_some_and(|request| {
                            request.as_ref().is_some_and(|(requested_at, recorded)| {
                                *requested_at < index && recorded == &identity
                            })
                        })
                        || receipt.is_none_or(|receipt| receipt.approval_request_id != *id)
                    {
                        counts.unmatched_or_conflicting_decision_events += 1;
                    } else {
                        record_decision(
                            &mut accepted_tools,
                            id,
                            serde_json::to_string(&receipt)?,
                            counts,
                        );
                    }
                }
            }
            SessionLogEntry::Control(ControlEntry::UserInputRequested(request)) => {
                if request.request.identity.session_scope_id.as_str() == scope {
                    input_requests.insert(
                        request.request_hash.clone(),
                        serde_json::to_string(&request.request.identity)?,
                    );
                }
            }
            SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(decision)) => {
                counts.user_input_decision_events += 1;
                if decision.identity.session_scope_id.as_str() != scope
                    || input_requests.get(&decision.request_hash)
                        != Some(&serde_json::to_string(&decision.identity)?)
                {
                    counts.unmatched_or_conflicting_decision_events += 1;
                } else {
                    record_decision(
                        &mut accepted_inputs,
                        &decision.request_hash,
                        serde_json::to_string(decision)?,
                        counts,
                    );
                }
            }
            _ => {}
        }
    }
    counts.unique_accepted_tool_decisions = accepted_tools
        .values()
        .filter(|value| value.is_some())
        .count();
    counts.unique_accepted_user_input_decisions = accepted_inputs
        .values()
        .filter(|value| value.is_some())
        .count();
    Ok(())
}

fn record_decision<'a>(
    decisions: &mut BTreeMap<&'a str, Option<String>>,
    id: &'a str,
    value: String,
    counts: &mut ModelEvalDecisionMetrics,
) {
    if let Some(previous) = decisions.get_mut(id) {
        if previous.as_ref() != Some(&value) {
            *previous = None;
            counts.unmatched_or_conflicting_decision_events += 1;
        }
    } else {
        decisions.insert(id, Some(value));
    }
}

fn feedback_key(feedback: &TaskVerificationFeedbackV1) -> (String, String, String) {
    (
        feedback.task_id.as_str().to_owned(),
        feedback.admission_id.clone(),
        feedback.attempt_id.clone(),
    )
}

type RepairSelection<'a> = (usize, usize, &'a TaskVerificationFeedbackV1);

fn observe_repairs(
    entries: &[Option<SessionLogEntry>],
    scope: &str,
    counts: &mut ModelEvalRepairMetrics,
) {
    let mut receipts = BTreeMap::new();
    let mut pending = BTreeMap::new();
    let mut selected = BTreeSet::new();
    let mut repairs: BTreeMap<_, Vec<RepairSelection<'_>>> = BTreeMap::new();
    let mut failed_pairs = 0;
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            Some(SessionLogEntry::Control(ControlEntry::VerificationRecorded(record))) => {
                let id = &record.receipt.receipt.receipt_id;
                // Ambiguous receipt identity is not usable even when the last record looks valid.
                if receipts.contains_key(id) {
                    receipts.insert(id, None);
                } else {
                    receipts.insert(id, Some((index, &record.receipt)));
                }
            }
            Some(SessionLogEntry::Control(ControlEntry::TaskVerificationFeedbackV1(feedback)))
                if feedback.validate().is_ok() =>
            {
                let key = feedback_key(feedback);
                match feedback.status {
                    TaskVerificationFeedbackStatusV1::Pending => {
                        if pending.contains_key(&feedback.feedback_id) {
                            continue;
                        }
                        if let Some(selections) = repairs.remove(&key) {
                            counts.repairs_with_next_feedback += selections.len();
                            if let [(pending_index, repair_index, previous)] = selections.as_slice()
                            {
                                let before =
                                    failed_checks(previous, &receipts, scope, None, *pending_index);
                                let after = failed_checks(
                                    feedback,
                                    &receipts,
                                    scope,
                                    Some(*repair_index),
                                    index,
                                );
                                if let (Some(before), Some(after)) = (before, after) {
                                    if previous.policy_hash == feedback.policy_hash
                                        && !before.is_disjoint(&after)
                                    {
                                        counts.comparable_recheck_pairs += 1;
                                        failed_pairs += 1;
                                    } else {
                                        counts.excluded_recheck_pairs += 1;
                                    }
                                } else {
                                    counts.excluded_recheck_pairs += 1;
                                }
                            } else {
                                counts.excluded_recheck_pairs += selections.len();
                            }
                        }
                        pending.insert(&feedback.feedback_id, (index, feedback));
                    }
                    TaskVerificationFeedbackStatusV1::Repair => {
                        if !selected.insert(&feedback.feedback_id) {
                            continue;
                        }
                        let pending_index = pending.get(&feedback.feedback_id).and_then(
                            |(pending_index, previous): &(usize, &TaskVerificationFeedbackV1)| {
                                (previous.task_id == feedback.task_id
                                    && previous.admission_id == feedback.admission_id
                                    && previous.attempt_id == feedback.attempt_id
                                    && previous.receipt_ids == feedback.receipt_ids
                                    && previous.policy_hash == feedback.policy_hash
                                    && previous.workspace_snapshot_id
                                        == feedback.workspace_snapshot_id)
                                    .then_some(*pending_index)
                            },
                        );
                        if let Some(pending_index) = pending_index {
                            counts.selected_repairs += 1;
                            repairs
                                .entry(key)
                                .or_default()
                                .push((pending_index, index, feedback));
                        } else {
                            counts.unmatched_repair_selections += 1;
                        }
                    }
                    TaskVerificationFeedbackStatusV1::Blocked => {}
                }
            }
            _ => {}
        }
    }
    counts.repairs_without_next_feedback =
        counts.selected_repairs - counts.repairs_with_next_feedback;
    if counts.comparable_recheck_pairs > 0 {
        counts.failed_rechecks_after_repair = Some(failed_pairs);
    }
}

fn failed_checks(
    feedback: &TaskVerificationFeedbackV1,
    receipts: &BTreeMap<&String, Option<(usize, &VerificationReceipt)>>,
    scope: &str,
    after: Option<usize>,
    before: usize,
) -> Option<BTreeSet<String>> {
    let mut checks = BTreeSet::new();
    for id in &feedback.receipt_ids {
        let (index, receipt) = receipts.get(id)?.as_ref()?;
        if *index >= before
            || after.is_some_and(|after| *index <= after)
            || receipt.receipt.source_session_id != scope
            || receipt.receipt.scope
                != sigil_kernel::EvidenceScope::Task(feedback.task_id.as_str().to_owned())
            || receipt.receipt.policy_hash.as_deref() != Some(feedback.policy_hash.as_str())
            || receipt.binding.workspace_snapshot_id != feedback.workspace_snapshot_id
            || receipt.receipt.workspace_snapshot_id.as_deref()
                != Some(feedback.workspace_snapshot_id.as_str())
            || receipt.check_status != ReceiptStatus::Failed
            || receipt.receipt.status != ReceiptStatus::Failed
        {
            return None;
        }
        checks.insert(
            serde_json::json!([
                receipt.check_spec_id,
                receipt.binding.check_spec_hash,
                receipt.binding.verification_scope_hash,
                receipt.binding.workspace_id,
                receipt.binding.environment_fingerprint,
                receipt.binding.sandbox_profile_hash,
                receipt.binding.workspace_trust_snapshot_id,
            ])
            .to_string(),
        );
    }
    Some(checks)
}

#[cfg(test)]
#[path = "../tests/model_eval_activity_tests.rs"]
mod tests;
