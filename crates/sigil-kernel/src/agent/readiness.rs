use std::collections::BTreeMap;

use anyhow::Result;

use crate::{
    CheckpointRestored, ExecutionMutationProfile, MutationCommitted, MutationReconciled,
    MutationResolution, WorkspaceMutationDetected,
    event::{DurableEventType, EventHandler, RunEvent},
    session::{
        ControlEntry, JsonlSessionStore, Session, SessionLogEntry, SessionStreamRecord,
        ToolExecutionStatus,
    },
    verification::{
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH, EvidenceScope, ReadinessEvaluatedEntry,
        ReadinessInput, RunStatus, VerificationPolicy, VerificationScope,
        WorkspaceMutationEvidence, WorkspaceTrust, build_workspace_snapshot_for_event,
        evaluate_readiness, stable_workspace_id,
    },
};

use super::{
    AgentRunOptions, AgentRunOutcome,
    tool_audit::{execution_mutation_profile_from_details, terminal_task_id_from_tool_metadata},
};

pub(super) fn append_agent_run_readiness(
    session: &mut Session,
    handler: &mut impl EventHandler,
    options: &AgentRunOptions,
    final_message_id: &str,
    outcome: &AgentRunOutcome,
) -> Result<()> {
    let entry = projected_agent_run_readiness(session, options, final_message_id, outcome)?;
    let control = ControlEntry::ReadinessEvaluated(entry);
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

/// Computes the readiness verdict that would be recorded for a final run answer.
///
/// This is provider-neutral and intentionally shares the same reducer used by the durable
/// post-final readiness entry so pre-final facts do not drift from the persisted verdict.
pub fn projected_agent_run_readiness(
    session: &Session,
    options: &AgentRunOptions,
    final_message_id: &str,
    outcome: &AgentRunOutcome,
) -> Result<ReadinessEvaluatedEntry> {
    let scope = EvidenceScope::Run(final_message_id.to_owned());
    let projection = session.verification_state_projection();
    let mut policy = projection
        .latest_policy(&scope)
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| {
            VerificationPolicy::no_checks_required(DEFAULT_TASK_VERIFICATION_SCOPE_HASH)
        });
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let mut mutations =
        agent_run_workspace_mutation_evidence(session, &policy.verification_scope, outcome)?;
    let policy_requires_snapshot = !policy.required_checks.is_empty()
        || policy.completion_criteria != crate::CompletionCriteria::NoChecksRequired;
    let has_recorded_workspace_mutation =
        !outcome.changed_files.is_empty() || !mutations.is_empty();
    let snapshot = if has_recorded_workspace_mutation || policy_requires_snapshot {
        let source_stream_sequence = session.next_stream_sequence_hint()?;
        Some(build_workspace_snapshot_for_event(
            &options.workspace_root,
            workspace_id.clone(),
            &policy.verification_scope,
            0,
            final_message_id.to_owned(),
            source_stream_sequence,
        )?)
    } else {
        None
    };
    if let Some(snapshot) = snapshot.as_ref()
        && let Some(unknown_dirty) = snapshot.unknown_dirty_evidence.clone()
    {
        mutations.push(unknown_dirty);
    }
    let has_workspace_mutation = has_recorded_workspace_mutation || !mutations.is_empty();
    if !has_workspace_mutation {
        policy.required_checks.clear();
        policy.completion_criteria = crate::CompletionCriteria::NoChecksRequired;
        policy.allow_unverified_completion = true;
    }
    let policy_hash = policy.stable_hash()?;
    let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
    input.workspace_trust = projection
        .workspace_trust
        .get(&workspace_id)
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    input.current_workspace_snapshot_id = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.workspace_snapshot_id.clone());
    input.workspace_knowledge = if let Some(snapshot) = snapshot.as_ref()
        && snapshot.workspace_knowledge.is_unknown_dirty()
    {
        snapshot.workspace_knowledge.clone()
    } else if mutations.iter().any(|mutation| mutation.unknown_dirty) {
        crate::WorkspaceKnowledge::UnknownDirty
    } else if has_workspace_mutation {
        crate::WorkspaceKnowledge::Dirty(1)
    } else if let Some(snapshot) = snapshot.as_ref() {
        snapshot.workspace_knowledge.clone()
    } else {
        crate::WorkspaceKnowledge::Clean(0)
    };
    input.mutations = mutations;
    input.verification_receipts = projection
        .receipts
        .values()
        .filter(|entry| entry.receipt.receipt.scope == scope)
        .map(|entry| entry.receipt.clone())
        .collect();
    input.final_assistant_event_id = Some(final_message_id.to_owned());

    let workspace_snapshot_id = input.current_workspace_snapshot_id.clone();
    Ok(ReadinessEvaluatedEntry {
        scope,
        evaluation: evaluate_readiness(&input),
        policy_hash: Some(policy_hash),
        workspace_snapshot_id,
    })
}

fn agent_run_workspace_mutation_evidence(
    session: &Session,
    scope: &VerificationScope,
    outcome: &AgentRunOutcome,
) -> Result<Vec<WorkspaceMutationEvidence>> {
    let Some(path) = session.store_path() else {
        return Ok(Vec::new());
    };
    let records = JsonlSessionStore::read_event_records(path)?;
    let mut prepared_tool_calls = BTreeMap::<String, Option<String>>::new();
    for record in &records {
        let SessionStreamRecord::Stored(event) = record;
        if DurableEventType::from_event_type(&event.event_type)
            == Some(DurableEventType::MutationPrepared)
            && let Ok(payload) =
                serde_json::from_value::<crate::MutationPrepared>(event.payload.clone())
        {
            prepared_tool_calls.insert(payload.operation_id, payload.tool_call_id);
        }
    }

    let mut evidence = records
        .iter()
        .filter_map(|record| {
            let SessionStreamRecord::Stored(event) = record;
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationCommitted) => {
                    let payload =
                        serde_json::from_value::<MutationCommitted>(event.payload.clone()).ok()?;
                    let tool_call_id = prepared_tool_calls
                        .get(&payload.operation_id)
                        .and_then(Clone::clone)?;
                    if !outcome.tool_call_ids.contains(&tool_call_id) {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id.clone(),
                        source_event_type: DurableEventType::MutationCommitted.as_str().to_owned(),
                        source_label: None,
                        recovery_hint: None,
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: Some(payload.workspace_snapshot_id),
                        tool_effect: crate::ToolEffect::WorkspaceWrite,
                        unknown_dirty: false,
                    })
                }
                Some(DurableEventType::MutationReconciled) => {
                    let payload =
                        serde_json::from_value::<MutationReconciled>(event.payload.clone()).ok()?;
                    let tool_call_id = prepared_tool_calls
                        .get(&payload.operation_id)
                        .and_then(Clone::clone)?;
                    if !outcome.tool_call_ids.contains(&tool_call_id) {
                        return None;
                    }
                    let unknown_dirty = payload.resolution == MutationResolution::MarkUnknownDirty;
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id.clone(),
                        source_event_type: DurableEventType::MutationReconciled.as_str().to_owned(),
                        source_label: None,
                        recovery_hint: None,
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: payload.workspace_snapshot_id,
                        tool_effect: if unknown_dirty {
                            crate::ToolEffect::Unknown
                        } else {
                            crate::ToolEffect::WorkspaceWrite
                        },
                        unknown_dirty,
                    })
                }
                Some(DurableEventType::CheckpointRestored) => {
                    let payload =
                        serde_json::from_value::<CheckpointRestored>(event.payload.clone()).ok()?;
                    if !payload
                        .tool_call_id
                        .as_ref()
                        .is_some_and(|call_id| outcome.tool_call_ids.contains(call_id))
                    {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id.clone(),
                        source_event_type: DurableEventType::CheckpointRestored.as_str().to_owned(),
                        source_label: None,
                        recovery_hint: None,
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: Some(payload.workspace_snapshot_id),
                        tool_effect: crate::ToolEffect::WorkspaceWrite,
                        unknown_dirty: false,
                    })
                }
                Some(DurableEventType::WorkspaceMutationDetected) => {
                    let payload =
                        serde_json::from_value::<WorkspaceMutationDetected>(event.payload.clone())
                            .ok()?;
                    if let Some(call_id) = payload.tool_call_id.as_ref() {
                        if !outcome.tool_call_ids.contains(call_id) {
                            return None;
                        }
                    } else if !payload.unknown_dirty {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence::from_detected_event(
                        event.event_id.clone(),
                        event.stream_sequence,
                        payload,
                    ))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    evidence.extend(active_terminal_mutation_evidence(&records, scope));
    evidence.sort_by_key(|entry| entry.recorded_at_stream_sequence);
    Ok(evidence)
}

fn active_terminal_mutation_evidence(
    records: &[SessionStreamRecord],
    scope: &VerificationScope,
) -> Vec<WorkspaceMutationEvidence> {
    let mut open_profiles = BTreeMap::<String, (ExecutionMutationProfile, String, u64)>::new();
    let mut terminal_profiles = BTreeMap::<String, (ExecutionMutationProfile, String, u64)>::new();
    let mut active_terminals = BTreeMap::<String, (String, u64)>::new();

    for record in records {
        let SessionStreamRecord::Stored(event) = record;
        let Some(entry) = session_entry_from_stored_event(event) else {
            continue;
        };
        match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) => {
                if execution.status == ToolExecutionStatus::Started {
                    if let Some(profile) =
                        execution_mutation_profile_from_details(&execution.metadata)
                    {
                        open_profiles.insert(
                            execution.call_id.clone(),
                            (profile, event.event_id.clone(), event.stream_sequence),
                        );
                    }
                    continue;
                }
                if let Some(task_id) = terminal_task_id_from_tool_metadata(&execution.metadata)
                    && let Some(profile) = open_profiles.get(&execution.call_id)
                {
                    terminal_profiles.insert(task_id, profile.clone());
                }
                open_profiles.remove(&execution.call_id);
            }
            SessionLogEntry::Control(ControlEntry::TerminalTask(entry)) => {
                let task_id = entry.handle.task_id.as_str().to_owned();
                if entry.status.is_active() {
                    active_terminals
                        .insert(task_id, (event.event_id.clone(), event.stream_sequence));
                } else {
                    active_terminals.remove(&task_id);
                }
            }
            SessionLogEntry::User(_)
            | SessionLogEntry::Assistant(_)
            | SessionLogEntry::ToolResult(_)
            | SessionLogEntry::Control(_) => {}
        }
    }

    let mut evidence = open_profiles
        .into_values()
        .filter_map(|(profile, event_id, stream_sequence)| {
            profile.effect.may_mutate_workspace().then(|| {
                running_execution_evidence(
                    profile,
                    event_id,
                    stream_sequence,
                    scope,
                    "running_tool_execution",
                )
            })
        })
        .collect::<Vec<_>>();
    for (task_id, (event_id, stream_sequence)) in active_terminals {
        let Some((profile, _, _)) = terminal_profiles.get(&task_id) else {
            continue;
        };
        if profile.effect.may_mutate_workspace() {
            evidence.push(running_execution_evidence(
                profile.clone(),
                event_id,
                stream_sequence,
                scope,
                "running_terminal_task",
            ));
        }
    }
    evidence
}

fn running_execution_evidence(
    profile: ExecutionMutationProfile,
    event_id: String,
    stream_sequence: u64,
    scope: &VerificationScope,
    source_event_type: &str,
) -> WorkspaceMutationEvidence {
    let scope_hash = if profile.scan_scope_hash.is_empty() {
        scope.scope_hash.clone()
    } else {
        profile.scan_scope_hash
    };
    WorkspaceMutationEvidence {
        event_id,
        source_event_type: source_event_type.to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash,
        recorded_at_stream_sequence: stream_sequence,
        from_workspace_snapshot_id: profile.pre_execution_snapshot_id,
        to_workspace_snapshot_id: None,
        tool_effect: profile.effect,
        unknown_dirty: true,
    }
}

fn session_entry_from_stored_event(event: &crate::StoredEvent) -> Option<SessionLogEntry> {
    let value = event.payload.get("session_log_entry")?.clone();
    serde_json::from_value(value).ok()
}
