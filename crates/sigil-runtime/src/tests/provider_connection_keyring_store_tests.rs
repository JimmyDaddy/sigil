use super::*;
use crate::provider_connections::PreparedCredential;

#[test]
fn keyring_record_roundtrip_binds_identity_family_auth_and_generation() {
    let id = CredentialId::random();
    let record = ProviderCredentialRecord::new(
        id.clone(),
        &PreparedCredential::api_key(ProviderFamily::OpenAi, "keyring-secret-canary"),
    );
    let encoded = encode_record(&record).expect("record should encode");
    let decoded = decode_record(&id, encoded.as_slice()).expect("record should decode");
    assert_eq!(decoded.credential_id, id);
    assert_eq!(decoded.provider_family, ProviderFamily::OpenAi);
    assert_eq!(decoded.auth_kind, CredentialAuthKind::ApiKey);
    assert_eq!(decoded.generation_id, record.generation_id);
    assert_eq!(decoded.secret().expose_secret(), "keyring-secret-canary");
    assert!(!format!("{decoded:?}").contains("keyring-secret-canary"));
}

#[test]
fn keyring_record_rejects_wrong_identity_version_and_oversized_secret() {
    let id = CredentialId::random();
    let record = ProviderCredentialRecord::new(
        id.clone(),
        &PreparedCredential::api_key(ProviderFamily::DeepSeek, "secret"),
    );
    let encoded = encode_record(&record).expect("record should encode");
    let wrong_id = CredentialId::random();
    let mismatch =
        decode_record(&wrong_id, encoded.as_slice()).expect_err("wrong identity should fail");
    assert_eq!(
        mismatch.code,
        ProviderCredentialErrorCode::CredentialRecordMismatch.as_str()
    );

    let mut wire: serde_json::Value =
        serde_json::from_slice(encoded.as_slice()).expect("wire json");
    wire["version"] = serde_json::json!(99);
    let future = serde_json::to_vec(&wire).expect("future wire");
    assert_eq!(
        decode_record(&id, &future)
            .expect_err("future version should fail")
            .code,
        ProviderCredentialErrorCode::CredentialRecordInvalid.as_str()
    );

    let oversized = ProviderCredentialRecord::new(
        CredentialId::random(),
        &PreparedCredential::api_key(
            ProviderFamily::DeepSeek,
            "x".repeat(CREDENTIAL_RECORD_MAX_BYTES),
        ),
    );
    assert_eq!(
        encode_record(&oversized)
            .expect_err("oversized record should fail")
            .code,
        ProviderCredentialErrorCode::CredentialStoreRejected.as_str()
    );
}

#[test]
fn keyring_platform_failures_fail_closed_instead_of_triggering_file_fallback() {
    let platform_failure = keyring::Error::PlatformFailure(Box::new(std::io::Error::other(
        "authorization or interaction failure",
    )));
    assert_eq!(
        map_keyring_read_error(&platform_failure).code,
        ProviderCredentialErrorCode::CredentialStoreRejected.as_str()
    );
    assert_eq!(
        map_keyring_write_error(&platform_failure).code,
        ProviderCredentialErrorCode::CredentialStoreRejected.as_str()
    );
    assert_eq!(
        map_keyring_delete_error(&platform_failure).code,
        ProviderCredentialErrorCode::CredentialStoreRejected.as_str()
    );

    let unavailable = keyring::Error::NoStorageAccess(Box::new(std::io::Error::other(
        "native store unavailable",
    )));
    assert_eq!(
        map_keyring_write_error(&unavailable).code,
        ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn silent_macos_authentication_failures_are_non_interactive_unavailability() {
    for code in [
        MACOS_ERR_NOT_AVAILABLE,
        MACOS_ERR_READ_ONLY,
        MACOS_ERR_AUTH_FAILED,
        MACOS_ERR_NO_SUCH_KEYCHAIN,
        MACOS_ERR_INVALID_KEYCHAIN,
        MACOS_ERR_INTERACTION_NOT_ALLOWED,
    ] {
        assert_eq!(
            map_silent_macos_error(security_framework::base::Error::from_code(code)).code,
            ProviderCredentialErrorCode::CredentialStoreUnavailable.as_str()
        );
    }

    assert_eq!(
        map_silent_macos_error(security_framework::base::Error::from_code(-50)).code,
        ProviderCredentialErrorCode::CredentialStoreRejected.as_str()
    );
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
    target_os = "linux"
))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_credential_operations_wait_for_the_active_operation() {
    let (first_started_tx, first_started_rx) = std::sync::mpsc::channel();
    let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
    let first = tokio::spawn(run_keyring_task(move || {
        first_started_tx
            .send(())
            .expect("first task start should be observed");
        release_first_rx
            .recv()
            .expect("first task should be released");
        Ok::<_, ProviderCredentialError>("first")
    }));
    first_started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("first task should acquire the operation lock");

    let (second_started_tx, second_started_rx) = std::sync::mpsc::channel();
    let second = tokio::spawn(run_keyring_task(move || {
        second_started_tx
            .send(())
            .expect("second task start should be observed");
        Ok::<_, ProviderCredentialError>("second")
    }));

    assert!(
        second_started_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "a concurrent native operation must wait instead of being rejected"
    );
    release_first_tx
        .send(())
        .expect("first task release should be delivered");

    assert_eq!(
        first
            .await
            .expect("first join should complete")
            .expect("first operation should succeed"),
        "first"
    );
    second_started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("second operation should start after the first finishes");
    assert_eq!(
        second
            .await
            .expect("second join should complete")
            .expect("second operation should succeed"),
        "second"
    );
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
#[tokio::test]
#[ignore = "requires the native desktop credential store"]
async fn native_provider_credential_store_roundtrip_and_cleanup() {
    let store = SystemProviderCredentialStore;
    let id = CredentialId::random();
    let secret = format!("sigil-rfc0056-native-roundtrip-{id}");
    let record = ProviderCredentialRecord::new(
        id.clone(),
        &PreparedCredential::api_key(ProviderFamily::Custom, secret.clone()),
    );

    let roundtrip = async {
        store.store(&record).await?;
        store.load(&id).await
    }
    .await;
    let cleanup = store.delete(&id).await;
    let after_cleanup = store.load(&id).await;

    assert!(
        cleanup.is_ok(),
        "native credential cleanup failed: {:?}",
        cleanup.err()
    );
    assert!(
        matches!(after_cleanup, Ok(None)),
        "native credential remained readable after cleanup"
    );
    let loaded = roundtrip
        .expect("native credential store roundtrip should succeed")
        .expect("stored native credential should exist");
    assert_eq!(loaded.credential_id, id);
    assert_eq!(loaded.provider_family, ProviderFamily::Custom);
    assert_eq!(loaded.auth_kind, CredentialAuthKind::ApiKey);
    assert_eq!(loaded.secret().expose_secret(), secret);
}
