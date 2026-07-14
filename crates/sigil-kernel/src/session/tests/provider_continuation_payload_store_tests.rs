use std::fs;

use anyhow::Result;

use super::*;

const SESSION_ID: &str = "session-encrypted-continuation";

fn artifact_manifest(payload: &[u8]) -> ProviderContinuationPayloadLifecycleEntry {
    manifest(
        "artifact-candidate-1",
        ProviderContinuationPayloadKind::Artifact,
        ProviderContinuationPayloadStorageRef::Artifact {
            artifact_id: "artifact-ref-1".to_owned(),
        },
        ProviderContinuationPayloadIntegrity::Sha256(sha256_digest(payload)),
        payload.len() as u64,
    )
}

fn handle_manifest(payload: &[u8]) -> ProviderContinuationPayloadLifecycleEntry {
    manifest(
        "handle-candidate-1",
        ProviderContinuationPayloadKind::HandleState,
        ProviderContinuationPayloadStorageRef::SensitiveState {
            state_id: "handle-state-ref-1".to_owned(),
            key_slot_id: PROVIDER_CONTINUATION_SESSION_KEY_SLOT_ID.to_owned(),
        },
        ProviderContinuationPayloadIntegrity::KeyedMac(format!("hmac-sha256:{}", "a".repeat(64))),
        payload.len() as u64,
    )
}

fn manifest(
    candidate_id: &str,
    kind: ProviderContinuationPayloadKind,
    storage_ref: ProviderContinuationPayloadStorageRef,
    integrity: ProviderContinuationPayloadIntegrity,
    byte_size: u64,
) -> ProviderContinuationPayloadLifecycleEntry {
    ProviderContinuationPayloadLifecycleEntry {
        schema_version: PROVIDER_CONTINUATION_SCHEMA_VERSION,
        payload_id: provider_continuation_payload_id(candidate_id, kind),
        candidate_id: candidate_id.to_owned(),
        source: ProviderContinuationPayloadSource::Initiated {
            started_event_id: "native-compaction-start-1".to_owned(),
            attempt_id: "native-compaction-attempt-1".to_owned(),
        },
        kind,
        storage_ref,
        integrity,
        byte_size,
        state: ProviderContinuationPayloadLifecycleState::Committed,
        reason: None,
    }
}

fn payload_store(
    root: &std::path::Path,
) -> ProviderContinuationPayloadStore<InMemoryProviderContinuationSessionKeyStore> {
    ProviderContinuationPayloadStore::new(
        root,
        SESSION_ID,
        InMemoryProviderContinuationSessionKeyStore::default(),
    )
    .expect("fixture session id is valid")
}

#[test]
fn encrypted_payload_store_stages_finalizes_and_reads_artifact_without_plaintext() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = payload_store(&temp.path().join("payloads"));
    let payload = b"private provider-native continuation payload";
    let manifest = artifact_manifest(payload);

    assert_eq!(
        store.stage(&manifest, payload)?,
        ProviderContinuationPayloadStageResult::Staged
    );
    let staged_path = store.stage_path(&manifest)?;
    let encrypted = fs::read(&staged_path)?;
    assert!(
        !encrypted
            .windows(payload.len())
            .any(|window| window == payload)
    );
    assert_eq!(
        store.finalize(&manifest)?,
        ProviderContinuationPayloadFinalizeResult::Finalized
    );
    assert!(!staged_path.exists());
    assert_eq!(store.read_finalized(&manifest)?.as_slice(), payload);
    assert_eq!(
        store.stage(&manifest, payload)?,
        ProviderContinuationPayloadStageResult::AlreadyFinalized
    );

    let error = store
        .stage(&manifest, b"a different provider-native payload value")
        .expect_err("one manifest cannot be reused for different payload bytes");
    assert!(error.to_string().contains("size does not match"));
    Ok(())
}

#[test]
fn encrypted_payload_store_fails_closed_for_manifest_tamper_and_missing_key() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("payloads");
    let store = payload_store(&root);
    let payload = b"payload-bound-to-manifest";
    let manifest = artifact_manifest(payload);
    store.stage(&manifest, payload)?;
    store.finalize(&manifest)?;

    let mut tampered = manifest.clone();
    tampered.integrity = ProviderContinuationPayloadIntegrity::Sha256(sha256_digest(b"other"));
    let error = store
        .read_finalized(&tampered)
        .expect_err("ciphertext must be authenticated against the immutable manifest");
    assert!(error.to_string().contains("authenticated decryption"));

    let key_unavailable = payload_store(&root);
    let error = key_unavailable
        .read_finalized(&manifest)
        .expect_err("a fresh key store cannot open an existing encrypted payload");
    assert!(error.to_string().contains("session key is unavailable"));
    Ok(())
}

#[test]
fn encrypted_payload_store_rejects_artifact_hash_mismatch_before_writing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = payload_store(&temp.path().join("payloads"));
    let manifest = artifact_manifest(b"expected artifact bytes");

    let error = store
        .stage(&manifest, b"tampered artifact bytes")
        .expect_err("artifact bytes must match the manifest digest before staging");
    assert!(error.to_string().contains("hash does not match"));
    assert!(!store.stage_path(&manifest)?.exists());
    Ok(())
}

#[test]
fn encrypted_payload_store_encrypts_handle_state_with_the_session_key_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = payload_store(&temp.path().join("payloads"));
    let payload = b"opaque-server-handle-state";
    let manifest = handle_manifest(payload);

    store.stage(&manifest, payload)?;
    let encrypted = fs::read(store.stage_path(&manifest)?)?;
    assert!(
        !encrypted
            .windows(payload.len())
            .any(|window| window == payload)
    );
    store.finalize(&manifest)?;
    assert_eq!(store.read_finalized(&manifest)?.as_slice(), payload);
    Ok(())
}
