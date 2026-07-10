use super::*;

pub(super) fn durable_workspace_mutation_evidence(
    session: &Session,
    task_id: &TaskId,
    scope: &VerificationScope,
    tool_call_ids: &[String],
    latest_successful_verification_sequence: u64,
) -> Result<Vec<WorkspaceMutationEvidence>> {
    let Some(path) = session.store_path() else {
        return Ok(Vec::new());
    };
    let records = JsonlSessionStore::read_event_records(path)?;
    let baseline_sequence = latest_successful_verification_sequence.max(
        task_started_stream_sequence(&records, task_id)
            .unwrap_or(0)
            .saturating_sub(1),
    );
    let mut prepared_tool_calls = BTreeMap::<String, Option<String>>::new();
    for record in &records {
        let SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        if DurableEventType::from_event_type(&event.event_type)
            == Some(DurableEventType::MutationPrepared)
            && let Ok(payload) = serde_json::from_value::<MutationPrepared>(event.payload.clone())
        {
            prepared_tool_calls.insert(payload.operation_id, payload.tool_call_id);
        }
    }
    let running_evidence = running_execution_mutation_evidence(&records, scope);
    let mut evidence = records
        .into_iter()
        .filter_map(|record| {
            let SessionStreamRecord::Stored(event) = record else {
                return None;
            };
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationCommitted) => {
                    let payload =
                        serde_json::from_value::<MutationCommitted>(event.payload.clone()).ok()?;
                    if !mutation_matches_tool_call(
                        &payload.operation_id,
                        &prepared_tool_calls,
                        tool_call_ids,
                        event.stream_sequence,
                        baseline_sequence,
                    ) {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
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
                    if !mutation_matches_tool_call(
                        &payload.operation_id,
                        &prepared_tool_calls,
                        tool_call_ids,
                        event.stream_sequence,
                        baseline_sequence,
                    ) {
                        return None;
                    }
                    let unknown_dirty = payload.resolution == MutationResolution::MarkUnknownDirty;
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
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
                    if !mutation_detection_matches_filter(
                        payload.tool_call_id.as_deref(),
                        tool_call_ids,
                        event.stream_sequence,
                        baseline_sequence,
                    ) {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
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
                    if let Ok(payload) =
                        serde_json::from_value::<WorkspaceMutationDetected>(event.payload.clone())
                    {
                        if !mutation_detection_matches_filter(
                            payload.tool_call_id.as_deref(),
                            tool_call_ids,
                            event.stream_sequence,
                            baseline_sequence,
                        ) {
                            return None;
                        }
                        return Some(WorkspaceMutationEvidence::from_detected_event(
                            event.event_id,
                            event.stream_sequence,
                            payload,
                        ));
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::WorkspaceMutationDetected
                            .as_str()
                            .to_owned(),
                        source_label: None,
                        recovery_hint: None,
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: None,
                        tool_effect: crate::ToolEffect::Unknown,
                        unknown_dirty: true,
                    })
                }
                Some(DurableEventType::ChildChangesetMerged)
                | Some(DurableEventType::AgentMergeApplied) => {
                    if event.stream_sequence <= baseline_sequence {
                        return None;
                    }
                    Some(merge_workspace_mutation_evidence(&event, scope))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    evidence.extend(running_evidence);
    evidence.sort_by_key(|entry| entry.recorded_at_stream_sequence);
    Ok(evidence)
}

#[derive(Debug, Clone)]
struct RunningExecutionProfile {
    profile: ExecutionMutationProfile,
    event_id: String,
    stream_sequence: u64,
}

#[derive(Debug, Clone)]
struct ActiveTerminalTask {
    event_id: String,
    stream_sequence: u64,
}

pub(super) fn running_execution_mutation_evidence(
    records: &[SessionStreamRecord],
    scope: &VerificationScope,
) -> Vec<WorkspaceMutationEvidence> {
    let mut open_profiles = BTreeMap::<String, RunningExecutionProfile>::new();
    let mut terminal_profiles = BTreeMap::<String, RunningExecutionProfile>::new();
    let mut active_terminals = BTreeMap::<String, ActiveTerminalTask>::new();

    for record in records {
        let (entry, event_id, stream_sequence) = match record {
            SessionStreamRecord::Legacy { entry, event, .. } => (
                (**entry).clone(),
                event.event_id.clone(),
                event.stream_sequence,
            ),
            SessionStreamRecord::Stored(event) => {
                let Some(entry) = session_entry_from_event(event) else {
                    continue;
                };
                (entry, event.event_id.clone(), event.stream_sequence)
            }
        };
        match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) => {
                if execution.status == ToolExecutionStatus::Started {
                    if let Some(profile) =
                        execution_mutation_profile_from_metadata(&execution.metadata)
                    {
                        open_profiles.insert(
                            execution.call_id.clone(),
                            RunningExecutionProfile {
                                profile,
                                event_id: event_id.clone(),
                                stream_sequence,
                            },
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
                    active_terminals.insert(
                        task_id,
                        ActiveTerminalTask {
                            event_id: event_id.clone(),
                            stream_sequence,
                        },
                    );
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

    let mut emitted_call_ids = BTreeSet::<String>::new();
    let mut evidence = Vec::new();
    for (call_id, running) in open_profiles {
        if !running.profile.effect.may_mutate_workspace() {
            continue;
        }
        emitted_call_ids.insert(call_id);
        evidence.push(running_profile_evidence(
            &running,
            scope,
            "running_tool_execution",
        ));
    }

    for (task_id, active) in active_terminals {
        let Some(running) = terminal_profiles.get(&task_id) else {
            continue;
        };
        if emitted_call_ids.contains(&running.profile.tool_call_id)
            || !running.profile.effect.may_mutate_workspace()
        {
            continue;
        }
        let mut terminal_running = running.clone();
        terminal_running.event_id = active.event_id.clone();
        terminal_running.stream_sequence = active.stream_sequence;
        evidence.push(running_profile_evidence(
            &terminal_running,
            scope,
            "running_terminal_task",
        ));
    }

    evidence
}

fn running_profile_evidence(
    running: &RunningExecutionProfile,
    scope: &VerificationScope,
    source_event_type: &str,
) -> WorkspaceMutationEvidence {
    let scope_hash = if running.profile.scan_scope_hash.is_empty() {
        scope.scope_hash.clone()
    } else {
        running.profile.scan_scope_hash.clone()
    };
    WorkspaceMutationEvidence {
        event_id: running.event_id.clone(),
        source_event_type: source_event_type.to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash,
        recorded_at_stream_sequence: running.stream_sequence,
        from_workspace_snapshot_id: running.profile.pre_execution_snapshot_id.clone(),
        to_workspace_snapshot_id: None,
        tool_effect: running.profile.effect,
        unknown_dirty: true,
    }
}

pub(super) fn session_entry_from_event(event: &StoredEvent) -> Option<SessionLogEntry> {
    event
        .payload
        .get("session_log_entry")
        .cloned()
        .and_then(|value| serde_json::from_value::<SessionLogEntry>(value).ok())
}

pub(super) fn execution_mutation_profile_from_metadata(
    metadata: &ToolResultMeta,
) -> Option<ExecutionMutationProfile> {
    metadata
        .details
        .get("execution_mutation_profile")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(super) fn terminal_task_id_from_tool_metadata(metadata: &ToolResultMeta) -> Option<String> {
    metadata
        .details
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

pub(super) fn merge_workspace_mutation_evidence(
    event: &StoredEvent,
    scope: &VerificationScope,
) -> WorkspaceMutationEvidence {
    let from_workspace_snapshot_id = first_payload_string(
        &event.payload,
        &[
            "from_workspace_snapshot_id",
            "parent_workspace_snapshot_before_id",
            "before_workspace_snapshot_id",
        ],
    );
    let to_workspace_snapshot_id = first_payload_string(
        &event.payload,
        &[
            "to_workspace_snapshot_id",
            "parent_workspace_snapshot_after_id",
            "parent_workspace_snapshot_id",
            "workspace_snapshot_id",
        ],
    );
    WorkspaceMutationEvidence {
        event_id: event.event_id.clone(),
        source_event_type: event.event_type.clone(),
        source_label: None,
        recovery_hint: None,
        scope_hash: scope.scope_hash.clone(),
        recorded_at_stream_sequence: event.stream_sequence,
        from_workspace_snapshot_id,
        unknown_dirty: to_workspace_snapshot_id.is_none(),
        to_workspace_snapshot_id,
        tool_effect: crate::ToolEffect::WorkspaceWrite,
    }
}

pub(super) fn first_payload_string(payload: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(|value| value.as_str()))
        .map(str::to_owned)
}

pub(super) fn mutation_matches_tool_call(
    operation_id: &str,
    prepared_tool_calls: &BTreeMap<String, Option<String>>,
    tool_call_ids: &[String],
    stream_sequence: u64,
    baseline_sequence: u64,
) -> bool {
    prepared_tool_calls
        .get(operation_id)
        .and_then(|tool_call_id| tool_call_id.as_ref())
        .is_some_and(|tool_call_id| tool_call_ids.contains(tool_call_id))
        || stream_sequence > baseline_sequence
}

pub(super) fn mutation_detection_matches_filter(
    tool_call_id: Option<&str>,
    tool_call_ids: &[String],
    stream_sequence: u64,
    baseline_sequence: u64,
) -> bool {
    tool_call_id.is_some_and(|call_id| tool_call_ids.iter().any(|current| current == call_id))
        || stream_sequence > baseline_sequence
}

pub(super) fn task_started_stream_sequence(
    records: &[SessionStreamRecord],
    task_id: &TaskId,
) -> Option<u64> {
    records.iter().find_map(|record| {
        let entry = match record {
            SessionStreamRecord::Legacy { entry, .. } => (**entry).clone(),
            SessionStreamRecord::Stored(event) => {
                let payload = event.payload.get("session_log_entry")?.clone();
                serde_json::from_value::<crate::SessionLogEntry>(payload).ok()?
            }
        };
        let crate::SessionLogEntry::Control(ControlEntry::TaskRun(task)) = entry else {
            return None;
        };
        (task.task_id == *task_id).then_some(record.stream_sequence())
    })
}

pub(super) fn changed_files_mutation_evidence(
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    scope_hash: &str,
    from_workspace_snapshot_id: Option<&str>,
    recorded_at_stream_sequence: u64,
) -> WorkspaceMutationEvidence {
    WorkspaceMutationEvidence {
        event_id: format!(
            "task-step-mutation:{}:{}",
            request.task_id.as_str(),
            step.step_id.as_str()
        ),
        source_event_type: "task_step_changed_files".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: scope_hash.to_owned(),
        recorded_at_stream_sequence,
        from_workspace_snapshot_id: from_workspace_snapshot_id.map(str::to_owned),
        to_workspace_snapshot_id: None,
        tool_effect: crate::ToolEffect::WorkspaceWrite,
        unknown_dirty: false,
    }
}

pub(super) fn durable_mutation_replay_failed_evidence(
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    scope_hash: &str,
    recorded_at_stream_sequence: u64,
) -> WorkspaceMutationEvidence {
    WorkspaceMutationEvidence {
        event_id: format!(
            "task-step-durable-mutation-replay-failed:{}:{}",
            request.task_id.as_str(),
            step.step_id.as_str()
        ),
        source_event_type: "durable_mutation_replay_failed".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: scope_hash.to_owned(),
        recorded_at_stream_sequence,
        from_workspace_snapshot_id: None,
        to_workspace_snapshot_id: None,
        tool_effect: crate::ToolEffect::Unknown,
        unknown_dirty: true,
    }
}
