use anyhow::Result;

use crate::{
    CompletionRequest, FrozenProviderRequestMaterial, ModelMessage,
    PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION, ProviderRequestNonReconstructableReasonV1,
    ProviderRequestReconstructionDispositionV1, ProviderRequestSourceFrontierV1, ReasoningEffort,
};

fn request() -> CompletionRequest {
    CompletionRequest {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        messages: vec![ModelMessage::user("summarize the repository")],
        tools: Vec::new(),
        temperature: Some(0.2),
        max_tokens: Some(2048),
        reasoning_effort: Some(ReasoningEffort::High),
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: Some("tenant-secret-value".to_owned()),
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    }
}

#[test]
fn frozen_material_is_deterministic_for_one_session_and_preserves_the_exact_request() -> Result<()>
{
    let request = request();
    let first = FrozenProviderRequestMaterial::freeze("session-a", request.clone())?;
    let second = FrozenProviderRequestMaterial::freeze("session-a", request.clone())?;

    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_eq!(
        first.canonical_bytes_for_in_process_use(),
        second.canonical_bytes_for_in_process_use()
    );
    assert_eq!(
        first.request().messages[0].content.as_deref(),
        Some("summarize the repository")
    );
    assert_eq!(first.into_request().model_name, request.model_name);
    Ok(())
}

#[test]
fn frozen_material_binds_the_session_scope_and_every_request_field() -> Result<()> {
    let baseline_request = request();
    let baseline = FrozenProviderRequestMaterial::freeze("session-a", baseline_request.clone())?;
    let same = FrozenProviderRequestMaterial::freeze("session-a", baseline_request.clone())?;
    let other_scope = FrozenProviderRequestMaterial::freeze("session-b", baseline_request)?;

    let mut changed_request = request();
    changed_request.store = true;
    let changed = FrozenProviderRequestMaterial::freeze("session-a", changed_request)?;

    assert_eq!(
        baseline.canonical_bytes_for_in_process_use(),
        same.canonical_bytes_for_in_process_use()
    );
    assert_ne!(baseline.fingerprint(), other_scope.fingerprint());
    assert_ne!(baseline.fingerprint(), changed.fingerprint());
    Ok(())
}

#[test]
fn frozen_material_never_exposes_raw_request_content_in_debug_or_fingerprint() -> Result<()> {
    let frozen = FrozenProviderRequestMaterial::freeze("session-a", request())?;
    let rendered = format!("{frozen:?}");

    assert!(frozen.fingerprint().starts_with("hmac-sha256:"));
    assert!(!rendered.contains("tenant-secret-value"));
    assert!(!rendered.contains("summarize the repository"));
    assert!(!frozen.fingerprint().contains("tenant-secret-value"));
    Ok(())
}

#[test]
fn frozen_material_rejects_invalid_input_before_a_provider_can_send() {
    let mut invalid = request();
    invalid.temperature = Some(f32::NAN);

    let error = FrozenProviderRequestMaterial::freeze("session-a", invalid)
        .expect_err("non-finite temperature must not be frozen");
    assert!(error.to_string().contains("non-finite temperature"));
}

#[test]
fn frozen_material_includes_its_schema_version_in_canonical_bytes() -> Result<()> {
    let frozen = FrozenProviderRequestMaterial::freeze("session-a", request())?;
    let value: serde_json::Value =
        serde_json::from_slice(frozen.canonical_bytes_for_in_process_use())?;

    assert_eq!(
        value["schema_version"],
        serde_json::Value::from(PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION)
    );
    Ok(())
}

#[test]
fn cache_layout_observation_does_not_mutate_frozen_provider_request_material() -> Result<()> {
    let frozen = FrozenProviderRequestMaterial::freeze("session-a", request())?;
    let before = frozen.canonical_bytes_for_in_process_use().to_vec();
    let proof = frozen.cache_layout_proof(None)?;

    assert_eq!(frozen.canonical_bytes_for_in_process_use(), before);
    assert_eq!(
        frozen.request().messages[0].content.as_deref(),
        Some("summarize the repository")
    );
    assert!(!String::from_utf8(before)?.contains("cache_layout_proof"));
    proof.validate()?;
    Ok(())
}

#[test]
fn durable_request_envelope_verifies_reconstruction_without_persisting_exact_bytes() -> Result<()> {
    let request = request();
    let frozen = FrozenProviderRequestMaterial::freeze("session-a", request.clone())?;
    let layout = frozen.cache_layout_proof(None)?;
    let envelope = frozen.request_envelope(
        &layout,
        Some(ProviderRequestSourceFrontierV1 {
            session_id: "session-a".to_owned(),
            durable_end_offset: 128,
            stream_sequence: Some(7),
            event_id: Some("event-7".to_owned()),
            record_checksum: Some("checksum-7".to_owned()),
        }),
        "context-epoch:root",
    )?;

    assert_eq!(
        envelope.reconstruction_disposition,
        ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs
    );
    envelope.verify_reconstructed_request(&request)?;
    let encoded = serde_json::to_string(&envelope)?;
    assert!(!encoded.contains("summarize the repository"));
    assert!(!encoded.contains("tenant-secret-value"));

    let mut tampered = request.clone();
    tampered.messages[0].content = Some("different prompt".to_owned());
    assert!(envelope.verify_reconstructed_request(&tampered).is_err());

    let mut route_drift = request;
    route_drift.model_name = "different-model".to_owned();
    let route_error = envelope
        .verify_reconstructed_request(&route_drift)
        .expect_err("route drift must fail before accepting reconstructed material");
    assert!(route_error.to_string().contains("route"));

    let mut malformed = envelope.clone();
    malformed.process_local_material_fingerprint = "hmac-sha256:short".to_owned();
    assert!(malformed.validate().is_err());
    Ok(())
}

#[test]
fn request_envelope_marks_process_local_overlays_and_in_memory_sessions_honestly() -> Result<()> {
    let mut overlay_request = request();
    overlay_request.deterministic_materialization = false;
    let frozen = FrozenProviderRequestMaterial::freeze("session-a", overlay_request)?;
    let layout = frozen.cache_layout_proof(None)?;
    let durable = frozen.request_envelope(
        &layout,
        Some(ProviderRequestSourceFrontierV1 {
            session_id: "session-a".to_owned(),
            durable_end_offset: 0,
            stream_sequence: None,
            event_id: None,
            record_checksum: None,
        }),
        "context-epoch:root",
    )?;
    assert_eq!(
        durable.reconstruction_disposition,
        ProviderRequestReconstructionDispositionV1::ProcessLocalOverlayRequired
    );
    assert_eq!(
        durable.non_reconstructable_reasons,
        vec![ProviderRequestNonReconstructableReasonV1::ExactMessageOverlay]
    );
    durable.verify_exact_process_local_request(&frozen)?;
    let other_scope = FrozenProviderRequestMaterial::freeze("session-b", frozen.request().clone())?;
    assert!(
        durable
            .verify_exact_process_local_request(&other_scope)
            .is_err(),
        "process-local material authority must remain bound to its exact session scope"
    );

    let in_memory = frozen.request_envelope(&layout, None, "context-epoch:root")?;
    assert_eq!(
        in_memory.reconstruction_disposition,
        ProviderRequestReconstructionDispositionV1::InMemoryOnly
    );
    assert!(
        in_memory
            .non_reconstructable_reasons
            .contains(&ProviderRequestNonReconstructableReasonV1::InMemorySession)
    );
    Ok(())
}

#[test]
fn request_envelope_accepts_only_exact_durable_opaque_carrier_bindings() -> Result<()> {
    let mut request = request();
    let response_handle = crate::ResponseHandle {
        provider_name: "deepseek".to_owned(),
        response_id: "response-1".to_owned(),
        continuation_cursor: Some("cursor-1".to_owned()),
    };
    let continuation_state = crate::ProviderContinuationState {
        provider_name: "deepseek".to_owned(),
        state_kind: "deepseek.reasoning_replay".to_owned(),
        message_id: Some(request.messages[0].id.clone()),
        opaque_blob: serde_json::json!({"reasoning_content": "durable reasoning"}),
    };
    request.previous_response_handle = Some(response_handle.clone());
    request.continuation_states = vec![continuation_state.clone()];
    let frozen = FrozenProviderRequestMaterial::freeze("session-a", request)?;
    let layout = frozen.cache_layout_proof(None)?;
    let frontier = Some(ProviderRequestSourceFrontierV1 {
        session_id: "session-a".to_owned(),
        durable_end_offset: 128,
        stream_sequence: Some(7),
        event_id: Some("event-7".to_owned()),
        record_checksum: Some("checksum-7".to_owned()),
    });

    let unproven = frozen.request_envelope(&layout, frontier.clone(), "context-epoch:root")?;
    assert_eq!(
        unproven.reconstruction_disposition,
        ProviderRequestReconstructionDispositionV1::ProcessLocalOverlayRequired
    );
    assert_eq!(
        unproven.non_reconstructable_reasons,
        vec![
            ProviderRequestNonReconstructableReasonV1::PreviousResponseHandle,
            ProviderRequestNonReconstructableReasonV1::ProviderContinuationState,
        ]
    );

    let proven = frozen.request_envelope_with_durable_runtime_state(
        &layout,
        frontier.clone(),
        "context-epoch:root",
        Some(&response_handle),
        std::slice::from_ref(&continuation_state),
    )?;
    assert_eq!(
        proven.reconstruction_disposition,
        ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs
    );
    assert!(proven.non_reconstructable_reasons.is_empty());

    let mismatched = frozen.request_envelope_with_durable_runtime_state(
        &layout,
        frontier,
        "context-epoch:root",
        Some(&response_handle),
        &[],
    )?;
    assert_eq!(
        mismatched.non_reconstructable_reasons,
        vec![ProviderRequestNonReconstructableReasonV1::ProviderContinuationState]
    );
    Ok(())
}
