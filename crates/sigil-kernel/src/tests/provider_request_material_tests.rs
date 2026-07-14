use anyhow::Result;

use crate::{
    CompletionRequest, FrozenProviderRequestMaterial, ModelMessage,
    PROVIDER_REQUEST_MATERIAL_SCHEMA_VERSION, ReasoningEffort,
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
