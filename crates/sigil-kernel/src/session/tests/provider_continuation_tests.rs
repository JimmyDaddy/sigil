use std::fs;

use crate::{InputTokenEvidence, TokenMeasurementScope, ToolCall};
use anyhow::Result;

use super::*;

const SESSION_ID: &str = "session-provider-continuation";
const START_EVENT_ID: &str = "event-native-start";
const PHYSICAL_ATTEMPT_ID: &str = "native-attempt-1";

fn sha256(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn hmac(byte: char) -> String {
    format!("hmac-sha256:{}", byte.to_string().repeat(64))
}

fn profile(id: &str) -> VersionedProfileIdentity {
    VersionedProfileIdentity::from_content(id, 1, id.as_bytes())
}

fn physical_started() -> ProviderPhysicalAttemptStartedEntry {
    ProviderPhysicalAttemptStartedEntry {
        schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
        physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
        logical_run_id: "native-compaction-run-1".to_owned(),
        purpose: ProviderPhysicalAttemptPurpose::NativeCompaction,
        request_material_fingerprint: hmac('a'),
        provider_name: "test-provider".to_owned(),
        model_name: "test-model".to_owned(),
        started_at_unix_ms: 1,
    }
}

fn direct_event(
    event_type: DurableEventType,
    event_id: String,
    stream_sequence: u64,
    payload: serde_json::Value,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> Result<StoredEvent> {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known durable event has a class"),
        event_id,
        SESSION_ID.to_owned(),
        stream_sequence,
        payload,
    )?;
    event.correlation_id = correlation_id.map(str::to_owned);
    event.causation_id = causation_id.map(str::to_owned);
    event.record_checksum = event.compute_record_checksum()?;
    Ok(event)
}

fn observation() -> ProviderContinuationObservedEntry {
    observation_for_session(SESSION_ID)
}

fn observation_for_session(session_id: &str) -> ProviderContinuationObservedEntry {
    let observed_payload_integrity_tag = hmac('b');
    ProviderContinuationObservedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        observation_id: provider_continuation_observation_id(
            session_id,
            &hmac('c'),
            PHYSICAL_ATTEMPT_ID,
            0,
            &observed_payload_integrity_tag,
        ),
        physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
        response_item_ordinal: 0,
        observed_payload_integrity_tag,
        provider_name: "test-provider".to_owned(),
        provider_route_fingerprint: hmac('c'),
        model_name: "test-model".to_owned(),
        model_metadata_profile: profile("model-metadata"),
        wire_profile: profile("wire"),
        wire_protocol: "test-wire".to_owned(),
        wire_schema_version: "v1".to_owned(),
        provider_request_id: Some("request-1".to_owned()),
        provider_response_id: Some("response-1".to_owned()),
        observed_at_unix_ms: 2,
    }
}

fn artifact_candidate(
    candidate_id: String,
    source_event_id: String,
    observation_id: Option<String>,
) -> ProviderContinuationCandidateRecordedEntry {
    ProviderContinuationCandidateRecordedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        candidate_id: candidate_id.clone(),
        observation_id,
        candidate: ProviderContinuationCandidate::Artifact(ProviderCompactionArtifactRef {
            candidate_id: candidate_id.clone(),
            payload: ProviderContinuationPayloadIdentity {
                payload_id: provider_continuation_payload_id(
                    &candidate_id,
                    ProviderContinuationPayloadKind::Artifact,
                ),
                integrity: ProviderContinuationPayloadIntegrity::Sha256(sha256('d')),
                byte_size: 128,
            },
            artifact_id: format!("artifact-{candidate_id}"),
            provider_name: "test-provider".to_owned(),
            provider_route_fingerprint: hmac('c'),
            model_name: "test-model".to_owned(),
            model_metadata_profile: profile("model-metadata"),
            wire_profile: profile("wire"),
            wire_protocol: "test-wire".to_owned(),
            wire_schema_version: "v1".to_owned(),
            composition_profile: profile("composition"),
            artifact_kind: "compaction-result".to_owned(),
            composition_mode: ProviderArtifactComposition::ReplacementWindow,
            covers_through: CompactionCursor {
                session_id: SESSION_ID.to_owned(),
                through_stream_sequence: 1,
                through_event_id: START_EVENT_ID.to_owned(),
            },
            request_fingerprint: hmac('e'),
            sensitivity: ContextSensitivity::Repository,
        }),
        resolution_mode: ProviderContinuationResolutionMode::NativeOnly,
        activation_gate: ProviderContinuationActivationGate::Immediate,
        source_event_id,
        created_at_unix_ms: 3,
    }
}

fn observed_payload_source(
    observation: &ProviderContinuationObservedEntry,
    observed_event_id: String,
) -> ProviderContinuationPayloadSource {
    ProviderContinuationPayloadSource::ProviderObserved {
        observation_event_id: observed_event_id,
        observation_id: observation.observation_id.clone(),
    }
}

fn artifact_payload_lifecycle(
    candidate: &ProviderContinuationCandidateRecordedEntry,
    source: ProviderContinuationPayloadSource,
    state: ProviderContinuationPayloadLifecycleState,
) -> ProviderContinuationPayloadLifecycleEntry {
    let ProviderContinuationCandidate::Artifact(reference) = &candidate.candidate else {
        unreachable!("fixture builds an artifact candidate")
    };
    ProviderContinuationPayloadLifecycleEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        payload_id: reference.payload.payload_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        source,
        kind: ProviderContinuationPayloadKind::Artifact,
        storage_ref: ProviderContinuationPayloadStorageRef::Artifact {
            artifact_id: reference.artifact_id.clone(),
        },
        integrity: reference.payload.integrity.clone(),
        byte_size: reference.payload.byte_size,
        state,
        reason: match state {
            ProviderContinuationPayloadLifecycleState::Committed => None,
            ProviderContinuationPayloadLifecycleState::Invalidated => {
                Some("fixture invalidation".to_owned())
            }
            ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {
                Some("fixture orphan discovery".to_owned())
            }
            ProviderContinuationPayloadLifecycleState::Deleted => {
                Some("fixture deletion".to_owned())
            }
        },
    }
}

fn lifecycle_event(
    entry: &ProviderContinuationPayloadLifecycleEntry,
    stream_sequence: u64,
) -> Result<StoredEvent> {
    lifecycle_event_with_links(entry, stream_sequence, None, None)
}

fn lifecycle_event_with_links(
    entry: &ProviderContinuationPayloadLifecycleEntry,
    stream_sequence: u64,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
        provider_continuation_payload_lifecycle_event_id(&entry.payload_id, entry.state),
        stream_sequence,
        serde_json::to_value(entry)?,
        correlation_id,
        causation_id,
    )
}

fn target_identity() -> ProviderContinuationTargetExecutionIdentity {
    ProviderContinuationTargetExecutionIdentity {
        provider_name: "test-provider".to_owned(),
        provider_route_fingerprint: hmac('c'),
        model_name: "test-model".to_owned(),
        model_metadata_profile: profile("model-metadata"),
        wire_profile: profile("wire"),
        wire_protocol: "test-wire".to_owned(),
        wire_schema_version: "v1".to_owned(),
        composition_profile: profile("composition"),
        token_measurement_profile: profile("target-token-measurement"),
        hosted_parity_profile: None,
    }
}

fn target_binding(
    identity: &ProviderContinuationTargetExecutionIdentity,
) -> TokenMeasurementBinding {
    TokenMeasurementBinding {
        schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: identity.provider_name.clone(),
        model_name: identity.model_name.clone(),
        wire_profile: identity.wire_profile.clone(),
        token_measurement_profile: identity.token_measurement_profile.clone(),
        hosted_parity_profile: identity.hosted_parity_profile.clone(),
    }
}

fn target_evidence(
    fingerprint: String,
    identity: &ProviderContinuationTargetExecutionIdentity,
) -> ProviderContinuationTargetTokenEvidence {
    ProviderContinuationTargetTokenEvidence {
        tokens: 200,
        material_fingerprint: fingerprint,
        binding: target_binding(identity),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    }
}

fn native_only_resolution_plan(
    observed: &ProviderContinuationObservedEntry,
    candidate: &ProviderContinuationCandidateRecordedEntry,
) -> ProviderObservedResolutionPlanRecordedEntry {
    let identity = target_identity();
    let ProviderContinuationCandidate::Artifact(reference) = &candidate.candidate else {
        unreachable!("fixture builds an artifact candidate")
    };
    ProviderObservedResolutionPlanRecordedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        resolution_plan_id: provider_observed_resolution_plan_id(
            &observed.observation_id,
            &candidate.candidate_id,
        ),
        observation_id: observed.observation_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        source_event_id: provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
        resolution_mode: ProviderContinuationResolutionMode::NativeOnly,
        lineage: ProviderObservedResolutionPlanLineage {
            parent_compaction_id: None,
            branch_id: None,
            valid_for_snapshot: None,
            base_projection_revision: "resolution-plan-r1".to_owned(),
            fold_candidate_fingerprint: hmac('f'),
            folded_through: reference.covers_through.clone(),
            retained_event_ids: vec![START_EVENT_ID.to_owned()],
            protected_event_ids: Vec::new(),
        },
        execution_identity: identity.clone(),
        semantic_compressor: None,
        target_budget: ProviderContinuationEffectiveCompactionBudget {
            target_request: EffectiveTokenBudget {
                schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                budget_profile: profile("target-budget"),
                context_window_tokens: 1_000,
                requested_output_tokens: 100,
                safety_buffer_tokens: 100,
            },
            minimum_savings_tokens: 50,
            minimum_savings_ratio_ppm: 200_000,
        },
        before_input: ProviderContinuationBeforeInputTokenCount::ConservativeLowerBound(
            target_evidence(reference.request_fingerprint.clone(), &identity),
        ),
        native_after_input: Some(
            ProviderContinuationAfterInputTokenCount::ConservativeUpperBound(target_evidence(
                hmac('a'),
                &identity,
            )),
        ),
        semantic_compressor_primary_fit: None,
        recorded_at_unix_ms: 4,
    }
}

fn semantic_compressor_identity() -> ProviderContinuationSemanticCompressorIdentity {
    ProviderContinuationSemanticCompressorIdentity {
        provider_name: "semantic-provider".to_owned(),
        provider_route_fingerprint: hmac('a'),
        model_name: "semantic-model".to_owned(),
        model_metadata_profile: profile("semantic-model-metadata"),
        wire_profile: profile("semantic-wire"),
        token_measurement_profile: profile("semantic-token-measurement"),
        hosted_parity_profile: None,
        request_budget_profile: profile("semantic-budget"),
        prompt_profile: profile("semantic-prompt"),
        checkpoint_schema_profile: profile("semantic-checkpoint"),
        validator_profile: profile("semantic-validator"),
    }
}

fn hybrid_resolution_plan(
    observed: &ProviderContinuationObservedEntry,
    candidate: &ProviderContinuationCandidateRecordedEntry,
) -> ProviderObservedResolutionPlanRecordedEntry {
    let mut plan = native_only_resolution_plan(observed, candidate);
    let semantic_identity = semantic_compressor_identity();
    let semantic_binding = TokenMeasurementBinding {
        schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: semantic_identity.provider_name.clone(),
        model_name: semantic_identity.model_name.clone(),
        wire_profile: semantic_identity.wire_profile.clone(),
        token_measurement_profile: semantic_identity.token_measurement_profile.clone(),
        hosted_parity_profile: None,
    };
    plan.resolution_mode = ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint;
    plan.native_after_input = None;
    plan.semantic_compressor = Some(semantic_identity.clone());
    plan.semantic_compressor_primary_fit = Some(ProviderContinuationSemanticCompressorRequestFit {
        material_fingerprint: hmac('b'),
        proof: RequestFitProof {
            schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            input: InputTokenEvidence::ConservativeUpperBound {
                tokens_upper_bound: 300,
                material_fingerprint: hmac('b'),
                measurement_scope: TokenMeasurementScope::RenderedSemanticCompressorInput,
                binding: semantic_binding,
            },
            budget: EffectiveTokenBudget {
                schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
                budget_profile: semantic_identity.request_budget_profile,
                context_window_tokens: 1_000,
                requested_output_tokens: 100,
                safety_buffer_tokens: 100,
            },
        },
    });
    plan
}

fn resolution_plan_event(
    plan: &ProviderObservedResolutionPlanRecordedEntry,
    stream_sequence: u64,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderObservedResolutionPlanRecorded,
        provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id),
        stream_sequence,
        serde_json::to_value(plan)?,
        Some(START_EVENT_ID),
        Some(&plan.source_event_id),
    )
}

fn completed_native_terminal_event(
    observed: &ProviderContinuationObservedEntry,
    candidate: &ProviderContinuationCandidateRecordedEntry,
    stream_sequence: u64,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-native-terminal".to_owned(),
        stream_sequence,
        serde_json::to_value(ProviderPhysicalAttemptTerminalEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
            request_material_fingerprint: hmac('a'),
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            rejection: None,
            provider_request_id: None,
            provider_response_id: None,
            durable_output_event_ids: vec![
                provider_continuation_observed_event_id(&observed.observation_id),
                provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
            ],
            durable_side_effect_event_ids: Vec::new(),
            finished_at_unix_ms: stream_sequence,
        })?,
        Some(START_EVENT_ID),
        Some(&provider_continuation_candidate_recorded_event_id(
            &candidate.candidate_id,
        )),
    )
}

fn assistant_tool_call_event(
    event_id: &str,
    stream_sequence: u64,
    causation_id: &str,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::AssistantMessageRecorded,
        event_id.to_owned(),
        stream_sequence,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::Assistant(ModelMessage::assistant(
                None,
                vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "write_file".to_owned(),
                    args_json: r#"{\"path\":\"README.md\"}"#.to_owned(),
                }],
            )),
        }),
        Some(START_EVENT_ID),
        Some(causation_id),
    )
}

fn tool_result_event(
    event_id: &str,
    stream_sequence: u64,
    tool_call_id: &str,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ToolResultRecorded,
        event_id.to_owned(),
        stream_sequence,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::ToolResult(ModelMessage::tool(
                tool_call_id,
                "tool completed",
            )),
        }),
        None,
        None,
    )
}

fn resolution_plan_event_with_causation(
    plan: &ProviderObservedResolutionPlanRecordedEntry,
    stream_sequence: u64,
    causation_id: &str,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderObservedResolutionPlanRecorded,
        provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id),
        stream_sequence,
        serde_json::to_value(plan)?,
        Some(START_EVENT_ID),
        Some(causation_id),
    )
}

fn completed_native_terminal_with_tool_call_event(
    observed: &ProviderContinuationObservedEntry,
    candidate: &ProviderContinuationCandidateRecordedEntry,
    tool_call_event_id: &str,
    stream_sequence: u64,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-native-terminal".to_owned(),
        stream_sequence,
        serde_json::to_value(ProviderPhysicalAttemptTerminalEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
            request_material_fingerprint: hmac('a'),
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            rejection: None,
            provider_request_id: None,
            provider_response_id: None,
            durable_output_event_ids: vec![
                provider_continuation_observed_event_id(&observed.observation_id),
                tool_call_event_id.to_owned(),
                provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
            ],
            durable_side_effect_event_ids: Vec::new(),
            finished_at_unix_ms: stream_sequence,
        })?,
        Some(START_EVENT_ID),
        Some(&provider_continuation_candidate_recorded_event_id(
            &candidate.candidate_id,
        )),
    )
}

fn observed_candidate_records_awaiting_tool_closure() -> Result<(
    Vec<SessionStreamRecord>,
    ProviderContinuationObservedEntry,
    ProviderContinuationCandidateRecordedEntry,
    ProviderToolCallClosureRef,
)> {
    const TOOL_CALL_EVENT_ID: &str = "event-native-tool-call";

    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    let observed_event = direct_event(
        DurableEventType::ProviderContinuationObserved,
        observed_event_id.clone(),
        2,
        serde_json::to_value(&observed)?,
        Some(START_EVENT_ID),
        Some(START_EVENT_ID),
    )?;
    let tool_call = ProviderToolCallClosureRef {
        tool_call_id: "call-1".to_owned(),
        tool_call_event_id: TOOL_CALL_EVENT_ID.to_owned(),
    };
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let mut candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id.clone(),
        Some(observed.observation_id.clone()),
    );
    candidate.activation_gate = ProviderContinuationActivationGate::AwaitingToolClosure {
        tool_calls: vec![tool_call.clone()],
        lease_expires_at_unix_ms: 100,
    };
    let manifest = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(&observed, observed_event_id),
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        5,
        serde_json::to_value(&candidate)?,
        Some(START_EVENT_ID),
        Some(TOOL_CALL_EVENT_ID),
    )?;
    Ok((
        vec![
            SessionStreamRecord::Stored(start),
            SessionStreamRecord::Stored(observed_event),
            SessionStreamRecord::Stored(assistant_tool_call_event(
                TOOL_CALL_EVENT_ID,
                3,
                &provider_continuation_observed_event_id(&observed.observation_id),
            )?),
            SessionStreamRecord::Stored(lifecycle_event(&manifest, 4)?),
            SessionStreamRecord::Stored(candidate_event),
        ],
        observed,
        candidate,
        tool_call,
    ))
}

fn tool_closure_entry(
    candidate: &ProviderContinuationCandidateRecordedEntry,
    tool_call: ProviderToolCallClosureRef,
    tool_result_event_id: &str,
    closed_at_unix_ms: u64,
) -> ProviderContinuationToolClosureRecordedEntry {
    ProviderContinuationToolClosureRecordedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        candidate_id: candidate.candidate_id.clone(),
        tool_call,
        tool_result_event_id: tool_result_event_id.to_owned(),
        closed_at_unix_ms,
    }
}

fn tool_closure_event(
    entry: &ProviderContinuationToolClosureRecordedEntry,
    stream_sequence: u64,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderContinuationToolClosureRecorded,
        provider_continuation_tool_closure_recorded_event_id(
            &entry.candidate_id,
            &entry.tool_call.tool_call_event_id,
        ),
        stream_sequence,
        serde_json::to_value(entry)?,
        Some(START_EVENT_ID),
        Some(&entry.tool_result_event_id),
    )
}

fn candidate_invalidation_entry(
    observed: &ProviderContinuationObservedEntry,
    candidate: &ProviderContinuationCandidateRecordedEntry,
    basis: ProviderContinuationCandidateInvalidationBasis,
) -> ProviderContinuationCandidateInvalidatedEntry {
    ProviderContinuationCandidateInvalidatedEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        candidate_id: candidate.candidate_id.clone(),
        observation_id: observed.observation_id.clone(),
        source_event_id: provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
        basis,
        reason: ProviderContinuationCandidateInvalidationReason::FrozenEvidenceRejected,
        invalidated_at_unix_ms: 10,
    }
}

fn candidate_invalidation_event(
    entry: &ProviderContinuationCandidateInvalidatedEntry,
    stream_sequence: u64,
    causation_id: &str,
) -> Result<StoredEvent> {
    direct_event(
        DurableEventType::ProviderContinuationCandidateInvalidated,
        provider_continuation_candidate_invalidated_event_id(&entry.candidate_id),
        stream_sequence,
        serde_json::to_value(entry)?,
        Some(START_EVENT_ID),
        Some(causation_id),
    )
}

fn observed_candidate_records() -> Result<(
    Vec<SessionStreamRecord>,
    ProviderContinuationObservedEntry,
    ProviderContinuationCandidateRecordedEntry,
)> {
    observed_candidate_records_with_mode(ProviderContinuationResolutionMode::NativeOnly)
}

fn observed_candidate_records_with_mode(
    resolution_mode: ProviderContinuationResolutionMode,
) -> Result<(
    Vec<SessionStreamRecord>,
    ProviderContinuationObservedEntry,
    ProviderContinuationCandidateRecordedEntry,
)> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    let observed_event = direct_event(
        DurableEventType::ProviderContinuationObserved,
        observed_event_id.clone(),
        2,
        serde_json::to_value(&observed)?,
        Some(START_EVENT_ID),
        Some(START_EVENT_ID),
    )?;
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let mut candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id.clone(),
        Some(observed.observation_id.clone()),
    );
    candidate.resolution_mode = resolution_mode;
    let manifest = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(&observed, observed_event_id.clone()),
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        4,
        serde_json::to_value(&candidate)?,
        Some(START_EVENT_ID),
        Some(&observed_event_id),
    )?;
    Ok((
        vec![
            SessionStreamRecord::Stored(start),
            SessionStreamRecord::Stored(observed_event),
            SessionStreamRecord::Stored(lifecycle_event(&manifest, 3)?),
            SessionStreamRecord::Stored(candidate_event),
        ],
        observed,
        candidate,
    ))
}

fn store_observed_candidate(
    store: &JsonlSessionStore,
    include_completed_terminal: bool,
) -> Result<(
    ProviderContinuationObservedEntry,
    ProviderContinuationCandidateRecordedEntry,
)> {
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderPhysicalAttemptStarted,
            serde_json::to_value(physical_started())?,
            START_EVENT_ID.to_owned(),
            Some(START_EVENT_ID.to_owned()),
            None,
            |_| Ok(true),
        )?
        .expect("physical attempt start should append");
    let session_id = JsonlSessionStore::read_event_records(store.path())?
        .first()
        .expect("physical attempt start has a session")
        .session_id()
        .to_owned();
    let observed = observation_for_session(&session_id);
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderContinuationObserved,
            serde_json::to_value(&observed)?,
            observed_event_id.clone(),
            Some(START_EVENT_ID.to_owned()),
            Some(START_EVENT_ID.to_owned()),
            |_| Ok(true),
        )?
        .expect("provider observation should append");
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let mut candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id.clone(),
        Some(observed.observation_id.clone()),
    );
    let ProviderContinuationCandidate::Artifact(reference) = &mut candidate.candidate else {
        unreachable!("fixture builds an artifact candidate")
    };
    reference.covers_through.session_id = session_id;
    let manifest = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(&observed, observed_event_id.clone()),
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
            serde_json::to_value(&manifest)?,
            provider_continuation_payload_lifecycle_event_id(&manifest.payload_id, manifest.state),
            None,
            None,
            |_| Ok(true),
        )?
        .expect("payload manifest should append");
    let candidate_event_id = provider_continuation_candidate_recorded_event_id(&candidate_id);
    store
        .append_event_if_with_identity(
            DurableEventType::ProviderContinuationCandidateRecorded,
            serde_json::to_value(&candidate)?,
            candidate_event_id.clone(),
            Some(START_EVENT_ID.to_owned()),
            Some(observed_event_id.clone()),
            |_| Ok(true),
        )?
        .expect("provider candidate should append");
    if include_completed_terminal {
        store
            .append_event_if_with_identity(
                DurableEventType::ProviderPhysicalAttemptTerminal,
                serde_json::to_value(ProviderPhysicalAttemptTerminalEntry {
                    schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
                    physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
                    request_material_fingerprint: hmac('a'),
                    outcome: ProviderPhysicalAttemptOutcome::Completed,
                    rejection: None,
                    provider_request_id: None,
                    provider_response_id: None,
                    durable_output_event_ids: vec![observed_event_id, candidate_event_id.clone()],
                    durable_side_effect_event_ids: Vec::new(),
                    finished_at_unix_ms: 5,
                })?,
                "event-native-terminal".to_owned(),
                Some(START_EVENT_ID.to_owned()),
                Some(candidate_event_id),
                |_| Ok(true),
            )?
            .expect("provider terminal should append");
    }
    Ok((observed, candidate))
}

#[test]
fn continuation_projection_accepts_one_observation_and_inactive_candidate() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    let observed_event = direct_event(
        DurableEventType::ProviderContinuationObserved,
        observed_event_id.clone(),
        2,
        serde_json::to_value(&observed)?,
        Some(START_EVENT_ID),
        Some(START_EVENT_ID),
    )?;
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id.clone(),
        Some(observed.observation_id.clone()),
    );
    let manifest = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(&observed, observed_event_id),
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    let manifest_event = lifecycle_event(&manifest, 3)?;
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        4,
        serde_json::to_value(&candidate)?,
        None,
        None,
    )?;

    let projection = ProviderContinuationProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(observed_event),
        SessionStreamRecord::Stored(manifest_event),
        SessionStreamRecord::Stored(candidate_event),
    ])?;

    assert!(projection.observation(&observed.observation_id).is_some());
    let state = projection
        .candidate(&candidate_id)
        .expect("candidate is recorded but remains inactive");
    assert_eq!(
        state.entry.candidate.payload_kind(),
        ProviderContinuationPayloadKind::Artifact
    );
    assert_eq!(
        projection.retention_pins(),
        vec![ProviderContinuationRetentionPin {
            payload_id: manifest.payload_id,
            candidate_id,
            kind: ProviderContinuationRetentionPinKind::CandidatePending,
        }]
    );
    Ok(())
}

#[test]
fn continuation_projection_accepts_one_frozen_provider_observed_native_plan() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 5,
    )?));

    let projection = ProviderContinuationProjection::from_records(&records)?;
    let state = projection
        .resolution_plan(&plan.resolution_plan_id)
        .expect("native-only resolution plan is durable but still inactive");
    assert_eq!(state.entry, plan);
    assert_eq!(state.session_id, SESSION_ID);
    assert_eq!(
        projection
            .resolution_plan_for_candidate(&candidate.candidate_id)
            .expect("candidate has exactly one resolution plan")
            .event_id,
        provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id)
    );
    assert_eq!(
        projection.retention_pins()[0].kind,
        ProviderContinuationRetentionPinKind::CandidatePending
    );
    Ok(())
}

#[test]
fn continuation_projection_accepts_hybrid_plan_before_any_semantic_provider_io() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records_with_mode(
        ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint,
    )?;
    let plan = hybrid_resolution_plan(&observed, &candidate);
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 5,
    )?));

    let projection = ProviderContinuationProjection::from_records(&records)?;
    let state = projection
        .resolution_plan_for_candidate(&candidate.candidate_id)
        .expect("hybrid plan is durable before semantic I/O");
    assert_eq!(
        state.entry.resolution_mode,
        ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint
    );
    assert!(state.entry.semantic_compressor.is_some());
    assert!(state.entry.semantic_compressor_primary_fit.is_some());
    assert!(state.entry.native_after_input.is_none());
    Ok(())
}

#[test]
fn tool_closure_gate_waits_until_its_durable_matching_closure() -> Result<()> {
    let (mut records, observed, candidate, tool_call) =
        observed_candidate_records_awaiting_tool_closure()?;

    assert_eq!(
        ProviderContinuationActivationEvaluator::from_records_at(&records, 100)?,
        vec![ProviderContinuationActivationState::AwaitingToolClosure {
            candidate_id: candidate.candidate_id.clone(),
            pending_tool_calls: vec![tool_call.clone()],
            lease_expires_at_unix_ms: 100,
        }]
    );

    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_with_tool_call_event(
            &observed,
            &candidate,
            &tool_call.tool_call_event_id,
            6,
        )?,
    ));
    records.push(SessionStreamRecord::Stored(tool_result_event(
        "event-tool-result",
        7,
        &tool_call.tool_call_id,
    )?));
    let closure = tool_closure_entry(&candidate, tool_call, "event-tool-result", 10);
    records.push(SessionStreamRecord::Stored(tool_closure_event(
        &closure, 8,
    )?));

    assert_eq!(
        ProviderContinuationActivationEvaluator::from_records_at(&records, 100)?,
        vec![ProviderContinuationActivationState::Ready {
            candidate_id: candidate.candidate_id,
        }]
    );
    Ok(())
}

#[test]
fn tool_closure_gate_expires_without_mutating_or_renewing_its_lease() -> Result<()> {
    let (records, _, candidate, tool_call) = observed_candidate_records_awaiting_tool_closure()?;

    assert_eq!(
        ProviderContinuationActivationEvaluator::from_records_at(&records, 101)?,
        vec![ProviderContinuationActivationState::LeaseExpired {
            candidate_id: candidate.candidate_id,
            pending_tool_calls: vec![tool_call],
            lease_expires_at_unix_ms: 100,
        }]
    );
    Ok(())
}

#[test]
fn resolution_plan_requires_all_tool_closures_and_the_latest_closure_causation() -> Result<()> {
    let (mut records, observed, candidate, tool_call) =
        observed_candidate_records_awaiting_tool_closure()?;
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_with_tool_call_event(
            &observed,
            &candidate,
            &tool_call.tool_call_event_id,
            6,
        )?,
    ));

    let plan = native_only_resolution_plan(&observed, &candidate);
    let missing_closure_error = ProviderContinuationProjection::from_records(&[
        records[0].clone(),
        records[1].clone(),
        records[2].clone(),
        records[3].clone(),
        records[4].clone(),
        records[5].clone(),
        SessionStreamRecord::Stored(resolution_plan_event(&plan, 7)?),
    ])
    .expect_err("an awaiting candidate cannot receive a plan before all tool closures");
    assert!(
        missing_closure_error
            .to_string()
            .contains("missing closure for tool call")
    );

    records.push(SessionStreamRecord::Stored(tool_result_event(
        "event-tool-result",
        7,
        &tool_call.tool_call_id,
    )?));
    let closure = tool_closure_entry(
        &candidate,
        tool_call,
        "event-tool-result",
        candidate.created_at_unix_ms,
    );
    let closure_event_id = provider_continuation_tool_closure_recorded_event_id(
        &closure.candidate_id,
        &closure.tool_call.tool_call_event_id,
    );
    records.push(SessionStreamRecord::Stored(tool_closure_event(
        &closure, 8,
    )?));
    records.push(SessionStreamRecord::Stored(
        resolution_plan_event_with_causation(&plan, 9, &closure_event_id)?,
    ));

    let projection = ProviderContinuationProjection::from_records(&records)?;
    assert!(
        projection
            .resolution_plan(&plan.resolution_plan_id)
            .is_some()
    );
    Ok(())
}

#[test]
fn tool_closure_rejects_mismatched_result_and_expired_receipt() -> Result<()> {
    let (mut records, observed, candidate, tool_call) =
        observed_candidate_records_awaiting_tool_closure()?;
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_with_tool_call_event(
            &observed,
            &candidate,
            &tool_call.tool_call_event_id,
            6,
        )?,
    ));
    records.push(SessionStreamRecord::Stored(tool_result_event(
        "event-tool-result",
        7,
        "call-2",
    )?));
    let mismatched = tool_closure_entry(&candidate, tool_call.clone(), "event-tool-result", 10);
    records.push(SessionStreamRecord::Stored(tool_closure_event(
        &mismatched,
        8,
    )?));
    let mismatch_error = ProviderContinuationProjection::from_records(&records)
        .expect_err("a closure must bind the matching tool result id");
    assert!(
        mismatch_error
            .to_string()
            .contains("result does not match its tool call")
    );

    let (mut records, observed, candidate, tool_call) =
        observed_candidate_records_awaiting_tool_closure()?;
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_with_tool_call_event(
            &observed,
            &candidate,
            &tool_call.tool_call_event_id,
            6,
        )?,
    ));
    records.push(SessionStreamRecord::Stored(tool_result_event(
        "event-tool-result",
        7,
        &tool_call.tool_call_id,
    )?));
    let expired = tool_closure_entry(&candidate, tool_call, "event-tool-result", 101);
    records.push(SessionStreamRecord::Stored(tool_closure_event(
        &expired, 8,
    )?));
    let expiry_error = ProviderContinuationProjection::from_records(&records)
        .expect_err("a closure receipt must be inside its absolute lease");
    assert!(
        expiry_error
            .to_string()
            .contains("outside its candidate lease")
    );
    Ok(())
}

#[test]
fn resolution_plan_coordinator_appends_once_and_consumes_the_strict_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, true)?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    let coordinator = ProviderObservedResolutionPlanCoordinator::new(store.clone());
    let event_id = provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id);

    assert_eq!(
        coordinator.append_or_reconcile(plan.clone())?,
        ProviderObservedResolutionPlanPersistence::Recorded {
            event_id: event_id.clone(),
        }
    );
    let after_first_append = fs::read(store.path())?;
    assert_eq!(
        coordinator.append_or_reconcile(plan)?,
        ProviderObservedResolutionPlanPersistence::AlreadyPresent { event_id }
    );
    assert_eq!(after_first_append, fs::read(store.path())?);
    Ok(())
}

#[test]
fn resolution_plan_coordinator_reconciles_exact_and_absent_acknowledgements() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, true)?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    let event_id = provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id);
    store.inject_writer_fault(SessionWriterFault::BeforeSync)?;

    assert_eq!(
        ProviderObservedResolutionPlanCoordinator::new(store.clone()).append_or_reconcile(plan)?,
        ProviderObservedResolutionPlanPersistence::ExactPresentAfterAckFailure { event_id }
    );
    assert_eq!(
        store
            .provider_continuation_projection()?
            .resolution_plans()
            .count(),
        1
    );

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, true)?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    store.inject_writer_fault(SessionWriterFault::BeforeWrite)?;

    assert_eq!(
        ProviderObservedResolutionPlanCoordinator::new(store.clone()).append_or_reconcile(plan)?,
        ProviderObservedResolutionPlanPersistence::ConfirmedAbsentAfterAckFailure
    );
    assert!(
        store
            .provider_continuation_projection()?
            .resolution_plans()
            .next()
            .is_none()
    );
    Ok(())
}

#[test]
fn resolution_plan_coordinator_refuses_to_append_before_a_completed_source_terminal() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, false)?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    let error = ProviderObservedResolutionPlanCoordinator::new(store.clone())
        .append_or_reconcile(plan)
        .expect_err("plan append requires a durable completed source terminal");
    assert!(error.to_string().contains("source terminal is not durable"));
    assert!(
        store
            .provider_continuation_projection()?
            .resolution_plans()
            .next()
            .is_none()
    );
    Ok(())
}

#[test]
fn candidate_invalidation_requires_a_source_terminal_and_blocks_payload_cleanup_until_recorded()
-> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_event(&observed, &candidate, 5)?,
    ));
    let invalidated = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(
            &observed,
            provider_continuation_observed_event_id(&observed.observation_id),
        ),
        ProviderContinuationPayloadLifecycleState::Invalidated,
    );
    let missing_terminal_error = ProviderContinuationProjection::from_records(&[
        records[0].clone(),
        records[1].clone(),
        records[2].clone(),
        records[3].clone(),
        SessionStreamRecord::Stored(lifecycle_event_with_links(
            &invalidated,
            5,
            Some(START_EVENT_ID),
            Some(&provider_continuation_candidate_recorded_event_id(
                &candidate.candidate_id,
            )),
        )?),
    ])
    .expect_err("candidate payload cleanup cannot bypass a source-valid invalidation terminal");
    assert!(
        missing_terminal_error
            .to_string()
            .contains("requires a source-valid invalidation")
    );

    let invalidation = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::SourceOnly,
    );
    let invalidation_event_id =
        provider_continuation_candidate_invalidated_event_id(&candidate.candidate_id);
    records.push(SessionStreamRecord::Stored(candidate_invalidation_event(
        &invalidation,
        6,
        &provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
    )?));
    records.push(SessionStreamRecord::Stored(lifecycle_event_with_links(
        &invalidated,
        7,
        Some(START_EVENT_ID),
        Some(&invalidation_event_id),
    )?));

    let projection = ProviderContinuationProjection::from_records(&records)?;
    assert_eq!(
        projection
            .candidate_invalidation(&candidate.candidate_id)
            .expect("invalidation remains auditable")
            .entry,
        invalidation
    );
    assert_eq!(
        projection.retention_pins(),
        vec![ProviderContinuationRetentionPin {
            payload_id: candidate.candidate.payload().payload_id.clone(),
            candidate_id: candidate.candidate_id,
            kind: ProviderContinuationRetentionPinKind::CleanupPending,
        }]
    );
    Ok(())
}

#[test]
fn candidate_invalidation_requires_plan_matching_evidence_once_a_plan_is_durable() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_event(&observed, &candidate, 5)?,
    ));
    let plan = native_only_resolution_plan(&observed, &candidate);
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 6,
    )?));

    let source_only = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::SourceOnly,
    );
    let source_only_error = ProviderContinuationProjection::from_records(&[
        records[0].clone(),
        records[1].clone(),
        records[2].clone(),
        records[3].clone(),
        records[4].clone(),
        records[5].clone(),
        SessionStreamRecord::Stored(candidate_invalidation_event(
            &source_only,
            7,
            &provider_continuation_candidate_recorded_event_id(&candidate.candidate_id),
        )?),
    ])
    .expect_err("a durable plan prevents source-only invalidation");
    assert!(
        source_only_error
            .to_string()
            .contains("cannot claim source-only evidence")
    );

    let plan_matching = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::ResolutionPlan {
            resolution_plan_id: plan.resolution_plan_id.clone(),
        },
    );
    records.push(SessionStreamRecord::Stored(candidate_invalidation_event(
        &plan_matching,
        7,
        &provider_observed_resolution_plan_recorded_event_id(&plan.resolution_plan_id),
    )?));
    let projection = ProviderContinuationProjection::from_records(&records)?;
    assert_eq!(
        projection
            .candidate_invalidation(&candidate.candidate_id)
            .expect("plan-matching invalidation should project")
            .entry,
        plan_matching
    );
    Ok(())
}

#[test]
fn invalidation_coordinator_appends_once_and_reconciles_acknowledgement_failures() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, true)?;
    let entry = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::SourceOnly,
    );
    let event_id = provider_continuation_candidate_invalidated_event_id(&candidate.candidate_id);
    let coordinator = ProviderContinuationCandidateInvalidationCoordinator::new(store.clone());

    assert_eq!(
        coordinator.append_or_reconcile(entry.clone())?,
        ProviderContinuationCandidateInvalidationPersistence::Recorded {
            event_id: event_id.clone(),
        }
    );
    assert_eq!(
        coordinator.append_or_reconcile(entry)?,
        ProviderContinuationCandidateInvalidationPersistence::AlreadyPresent { event_id }
    );

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, true)?;
    let entry = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::SourceOnly,
    );
    let event_id = provider_continuation_candidate_invalidated_event_id(&candidate.candidate_id);
    store.inject_writer_fault(SessionWriterFault::BeforeSync)?;
    assert_eq!(
        ProviderContinuationCandidateInvalidationCoordinator::new(store.clone())
            .append_or_reconcile(entry)?,
        ProviderContinuationCandidateInvalidationPersistence::ExactPresentAfterAckFailure {
            event_id,
        }
    );

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let (observed, candidate) = store_observed_candidate(&store, true)?;
    let entry = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::SourceOnly,
    );
    store.inject_writer_fault(SessionWriterFault::BeforeWrite)?;
    assert_eq!(
        ProviderContinuationCandidateInvalidationCoordinator::new(store.clone())
            .append_or_reconcile(entry)?,
        ProviderContinuationCandidateInvalidationPersistence::ConfirmedAbsentAfterAckFailure
    );
    assert!(
        store
            .provider_continuation_projection()?
            .candidate_invalidation(&candidate.candidate_id)
            .is_none()
    );
    Ok(())
}

#[test]
fn frozen_admission_waits_for_the_durable_source_terminal() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 5,
    )?));

    assert_eq!(
        ProviderObservedResolutionAdmissionEvaluator::from_records(&records)?,
        vec![
            ProviderObservedResolutionAdmission::AwaitingSourceAttemptTerminal {
                resolution_plan_id: plan.resolution_plan_id,
                candidate_id: candidate.candidate_id,
                physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
            }
        ]
    );
    Ok(())
}

#[test]
fn frozen_admission_proves_native_only_fit_and_savings_without_payload_io() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    let mut plan = native_only_resolution_plan(&observed, &candidate);
    let Some(ProviderContinuationAfterInputTokenCount::ConservativeUpperBound(evidence)) =
        &mut plan.native_after_input
    else {
        unreachable!("fixture is native-only with upper-bound after evidence")
    };
    evidence.tokens = 100;
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_event(&observed, &candidate, 5)?,
    ));
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 6,
    )?));

    assert_eq!(
        ProviderObservedResolutionAdmissionEvaluator::from_records(&records)?,
        vec![ProviderObservedResolutionAdmission::NativeOnlyReady {
            resolution_plan_id: plan.resolution_plan_id,
            candidate_id: candidate.candidate_id,
            guaranteed_savings_tokens: 100,
        }]
    );
    Ok(())
}

#[test]
fn frozen_admission_authorizes_hybrid_semantic_stage_without_sending_it() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records_with_mode(
        ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint,
    )?;
    let plan = hybrid_resolution_plan(&observed, &candidate);
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_event(&observed, &candidate, 5)?,
    ));
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 6,
    )?));

    assert_eq!(
        ProviderObservedResolutionAdmissionEvaluator::from_records(&records)?,
        vec![
            ProviderObservedResolutionAdmission::HybridSemanticCheckpointAuthorized {
                resolution_plan_id: plan.resolution_plan_id,
                candidate_id: candidate.candidate_id,
            }
        ]
    );
    Ok(())
}

#[test]
fn frozen_admission_rejects_native_plan_without_guaranteed_savings() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    let plan = native_only_resolution_plan(&observed, &candidate);
    records.push(SessionStreamRecord::Stored(
        completed_native_terminal_event(&observed, &candidate, 5)?,
    ));
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 6,
    )?));

    assert_eq!(
        ProviderObservedResolutionAdmissionEvaluator::from_records(&records)?,
        vec![ProviderObservedResolutionAdmission::Rejected {
            resolution_plan_id: plan.resolution_plan_id,
            candidate_id: candidate.candidate_id,
            reason: ProviderObservedResolutionAdmissionRejection::NoGuaranteedSavings,
        }]
    );
    Ok(())
}

#[test]
fn continuation_projection_rejects_provider_observed_plan_for_initiated_candidate() -> Result<()> {
    let start = direct_event(
        DurableEventType::CompactionStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(CompactionStartedEntry {
            attempt_id: "initiated-attempt-1".to_owned(),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "projection-r1".to_owned(),
            started_at_unix_ms: 1,
        })?,
        Some(START_EVENT_ID),
        None,
    )?;
    let candidate_id = provider_continuation_candidate_id_from_initiated(
        SESSION_ID,
        START_EVENT_ID,
        "initiated-attempt-1",
    );
    let candidate = artifact_candidate(candidate_id.clone(), START_EVENT_ID.to_owned(), None);
    let manifest = artifact_payload_lifecycle(
        &candidate,
        ProviderContinuationPayloadSource::Initiated {
            started_event_id: START_EVENT_ID.to_owned(),
            attempt_id: "initiated-attempt-1".to_owned(),
        },
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        3,
        serde_json::to_value(&candidate)?,
        None,
        None,
    )?;
    let synthetic_observation_id = "observation-is-not-a-source".to_owned();
    let mut plan = native_only_resolution_plan(
        &ProviderContinuationObservedEntry {
            schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
            observation_id: synthetic_observation_id.clone(),
            physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
            response_item_ordinal: 0,
            observed_payload_integrity_tag: hmac('b'),
            provider_name: "test-provider".to_owned(),
            provider_route_fingerprint: hmac('c'),
            model_name: "test-model".to_owned(),
            model_metadata_profile: profile("model-metadata"),
            wire_profile: profile("wire"),
            wire_protocol: "test-wire".to_owned(),
            wire_schema_version: "v1".to_owned(),
            provider_request_id: None,
            provider_response_id: None,
            observed_at_unix_ms: 2,
        },
        &candidate,
    );
    plan.resolution_plan_id =
        provider_observed_resolution_plan_id(&synthetic_observation_id, &candidate_id);
    let error = ProviderContinuationProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(lifecycle_event(&manifest, 2)?),
        SessionStreamRecord::Stored(candidate_event),
        SessionStreamRecord::Stored(resolution_plan_event(&plan, 4)?),
    ])
    .expect_err("initiated candidates must not receive provider-observed plans");
    assert!(
        error
            .to_string()
            .contains("cannot bind an initiated continuation candidate")
    );
    Ok(())
}

#[test]
fn continuation_projection_rejects_resolution_plan_with_mismatched_before_evidence() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    let mut plan = native_only_resolution_plan(&observed, &candidate);
    let ProviderContinuationBeforeInputTokenCount::ConservativeLowerBound(evidence) =
        &mut plan.before_input
    else {
        unreachable!("fixture is conservative lower bound")
    };
    evidence.material_fingerprint = hmac('a');
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 5,
    )?));

    let error = ProviderContinuationProjection::from_records(&records)
        .expect_err("before evidence must bind the candidate request material");
    assert!(
        error
            .to_string()
            .contains("before evidence does not match its candidate")
    );
    Ok(())
}

#[test]
fn continuation_projection_rejects_native_plan_without_native_after_evidence() -> Result<()> {
    let (mut records, observed, candidate) = observed_candidate_records()?;
    let mut plan = native_only_resolution_plan(&observed, &candidate);
    plan.native_after_input = None;
    records.push(SessionStreamRecord::Stored(resolution_plan_event(
        &plan, 5,
    )?));

    let error = ProviderContinuationProjection::from_records(&records)
        .expect_err("native-only plan requires frozen native-after evidence");
    assert!(
        error
            .to_string()
            .contains("mode and evidence options are inconsistent")
    );
    Ok(())
}

#[test]
fn continuation_projection_rejects_candidate_without_its_observation() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let candidate = artifact_candidate(
        candidate_id.clone(),
        provider_continuation_observed_event_id(&observed.observation_id),
        Some(observed.observation_id),
    );
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        2,
        serde_json::to_value(candidate)?,
        None,
        None,
    )?;

    let error = ProviderContinuationProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(candidate_event),
    ])
    .expect_err("candidate without a durable observation must fail closed");
    assert!(error.to_string().contains("unknown observation"));
    Ok(())
}

#[test]
fn continuation_projection_rejects_wrong_candidate_payload_identity() -> Result<()> {
    let start = direct_event(
        DurableEventType::CompactionStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(CompactionStartedEntry {
            attempt_id: "initiated-attempt-1".to_owned(),
            fallback_parent: CompactionFallbackParent::Root,
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "projection-r1".to_owned(),
            started_at_unix_ms: 1,
        })?,
        Some(START_EVENT_ID),
        None,
    )?;
    let candidate_id = provider_continuation_candidate_id_from_initiated(
        SESSION_ID,
        START_EVENT_ID,
        "initiated-attempt-1",
    );
    let mut candidate = artifact_candidate(candidate_id.clone(), START_EVENT_ID.to_owned(), None);
    let ProviderContinuationCandidate::Artifact(reference) = &mut candidate.candidate else {
        unreachable!("fixture builds an artifact candidate")
    };
    reference.payload.payload_id = "wrong-payload-id".to_owned();
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        2,
        serde_json::to_value(candidate)?,
        None,
        None,
    )?;

    let error = ProviderContinuationProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(candidate_event),
    ])
    .expect_err("candidate payload identity must be deterministic");
    assert!(error.to_string().contains("payload id does not match"));
    Ok(())
}

#[test]
fn continuation_projection_rejects_candidate_without_committed_payload_manifest() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    let observed_event = direct_event(
        DurableEventType::ProviderContinuationObserved,
        observed_event_id.clone(),
        2,
        serde_json::to_value(&observed)?,
        Some(START_EVENT_ID),
        Some(START_EVENT_ID),
    )?;
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id,
        Some(observed.observation_id),
    );
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        3,
        serde_json::to_value(candidate)?,
        None,
        None,
    )?;

    let error = ProviderContinuationProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(observed_event),
        SessionStreamRecord::Stored(candidate_event),
    ])
    .expect_err("candidate without a committed payload manifest must fail closed");
    assert!(
        error
            .to_string()
            .contains("missing committed payload manifest")
    );
    Ok(())
}

#[test]
fn continuation_payload_lifecycle_pins_cleanup_until_deleted() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    let observed_event = direct_event(
        DurableEventType::ProviderContinuationObserved,
        observed_event_id.clone(),
        2,
        serde_json::to_value(&observed)?,
        Some(START_EVENT_ID),
        Some(START_EVENT_ID),
    )?;
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id.clone(),
        Some(observed.observation_id.clone()),
    );
    let source = observed_payload_source(&observed, observed_event_id);
    let manifest = artifact_payload_lifecycle(
        &candidate,
        source.clone(),
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    let invalidated = artifact_payload_lifecycle(
        &candidate,
        source.clone(),
        ProviderContinuationPayloadLifecycleState::Invalidated,
    );
    let deleted = artifact_payload_lifecycle(
        &candidate,
        source,
        ProviderContinuationPayloadLifecycleState::Deleted,
    );
    let candidate_event = direct_event(
        DurableEventType::ProviderContinuationCandidateRecorded,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        4,
        serde_json::to_value(&candidate)?,
        None,
        None,
    )?;
    let source_terminal = direct_event(
        DurableEventType::ProviderPhysicalAttemptTerminal,
        "event-native-terminal".to_owned(),
        5,
        serde_json::to_value(ProviderPhysicalAttemptTerminalEntry {
            schema_version: PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
            physical_attempt_id: PHYSICAL_ATTEMPT_ID.to_owned(),
            request_material_fingerprint: hmac('a'),
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            rejection: None,
            provider_request_id: None,
            provider_response_id: None,
            durable_output_event_ids: vec![provider_continuation_observed_event_id(
                &observed.observation_id,
            )],
            durable_side_effect_event_ids: Vec::new(),
            finished_at_unix_ms: 5,
        })?,
        Some(START_EVENT_ID),
        Some(&provider_continuation_observed_event_id(
            &observed.observation_id,
        )),
    )?;
    let invalidation = candidate_invalidation_entry(
        &observed,
        &candidate,
        ProviderContinuationCandidateInvalidationBasis::SourceOnly,
    );
    let invalidation_event = candidate_invalidation_event(
        &invalidation,
        6,
        &provider_continuation_candidate_recorded_event_id(&candidate_id),
    )?;
    let mut records = vec![
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(observed_event),
        SessionStreamRecord::Stored(lifecycle_event(&manifest, 3)?),
        SessionStreamRecord::Stored(candidate_event),
        SessionStreamRecord::Stored(source_terminal),
        SessionStreamRecord::Stored(invalidation_event),
        SessionStreamRecord::Stored(lifecycle_event_with_links(
            &invalidated,
            7,
            Some(START_EVENT_ID),
            Some(&provider_continuation_candidate_invalidated_event_id(
                &candidate_id,
            )),
        )?),
    ];

    let cleanup_projection = ProviderContinuationProjection::from_records(&records)?;
    assert_eq!(
        cleanup_projection.retention_pins(),
        vec![ProviderContinuationRetentionPin {
            payload_id: manifest.payload_id.clone(),
            candidate_id: candidate_id.clone(),
            kind: ProviderContinuationRetentionPinKind::CleanupPending,
        }]
    );

    records.push(SessionStreamRecord::Stored(lifecycle_event(&deleted, 8)?));
    let deleted_projection = ProviderContinuationProjection::from_records(&records)?;
    assert!(deleted_projection.retention_pins().is_empty());
    assert_eq!(
        deleted_projection
            .payload(&manifest.payload_id)
            .expect("manifest remains auditable after deletion")
            .latest_lifecycle
            .state,
        ProviderContinuationPayloadLifecycleState::Deleted
    );
    Ok(())
}

#[test]
fn continuation_payload_lifecycle_rejects_transition_before_manifest() -> Result<()> {
    let start = direct_event(
        DurableEventType::ProviderPhysicalAttemptStarted,
        START_EVENT_ID.to_owned(),
        1,
        serde_json::to_value(physical_started())?,
        Some(START_EVENT_ID),
        None,
    )?;
    let observed = observation();
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    let observed_event = direct_event(
        DurableEventType::ProviderContinuationObserved,
        observed_event_id.clone(),
        2,
        serde_json::to_value(&observed)?,
        Some(START_EVENT_ID),
        Some(START_EVENT_ID),
    )?;
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let candidate = artifact_candidate(
        candidate_id,
        observed_event_id,
        Some(observed.observation_id.clone()),
    );
    let invalidated = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(
            &observed,
            provider_continuation_observed_event_id(&observed.observation_id),
        ),
        ProviderContinuationPayloadLifecycleState::Invalidated,
    );

    let error = ProviderContinuationProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(observed_event),
        SessionStreamRecord::Stored(lifecycle_event(&invalidated, 3)?),
    ])
    .expect_err("payload lifecycle cannot transition before a committed manifest");
    assert!(error.to_string().contains("unknown payload"));
    Ok(())
}

#[test]
fn continuation_store_projection_is_read_only() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append_event_if_with_identity(
        DurableEventType::ProviderPhysicalAttemptStarted,
        serde_json::to_value(physical_started())?,
        START_EVENT_ID.to_owned(),
        Some(START_EVENT_ID.to_owned()),
        None,
        |_| Ok(true),
    )?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let session_id = records
        .first()
        .expect("physical attempt start was appended")
        .session_id()
        .to_owned();
    let observed = observation_for_session(&session_id);
    let observed_event_id = provider_continuation_observed_event_id(&observed.observation_id);
    store.append_event_if_with_identity(
        DurableEventType::ProviderContinuationObserved,
        serde_json::to_value(&observed)?,
        observed_event_id.clone(),
        Some(START_EVENT_ID.to_owned()),
        Some(START_EVENT_ID.to_owned()),
        |_| Ok(true),
    )?;
    let candidate_id =
        provider_continuation_candidate_id_from_observation(&observed.observation_id);
    let candidate = artifact_candidate(
        candidate_id.clone(),
        observed_event_id.clone(),
        Some(observed.observation_id.clone()),
    );
    let manifest = artifact_payload_lifecycle(
        &candidate,
        observed_payload_source(&observed, observed_event_id),
        ProviderContinuationPayloadLifecycleState::Committed,
    );
    store.append_event_if_with_identity(
        DurableEventType::ProviderContinuationPayloadLifecycleRecorded,
        serde_json::to_value(&manifest)?,
        provider_continuation_payload_lifecycle_event_id(&manifest.payload_id, manifest.state),
        None,
        None,
        |_| Ok(true),
    )?;
    store.append_event_if_with_identity(
        DurableEventType::ProviderContinuationCandidateRecorded,
        serde_json::to_value(candidate)?,
        provider_continuation_candidate_recorded_event_id(&candidate_id),
        None,
        None,
        |_| Ok(true),
    )?;

    let before = fs::read(store.path())?;
    let projection = store.provider_continuation_projection()?;
    let after = fs::read(store.path())?;

    assert!(projection.candidate(&candidate_id).is_some());
    assert_eq!(before, after);
    Ok(())
}
