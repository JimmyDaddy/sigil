use super::*;
use sigil_kernel::RootConfig;

fn config() -> RootConfig {
    RootConfig::parse_persisted(
        r#"config_version = 2

[agent]
connection = "local"
model = "local-model"

[workspace]
root = "."
"#,
    )
    .expect("valid root config")
}

fn capsule(value: &str) -> OpaqueRegistrationCapsuleId {
    OpaqueRegistrationCapsuleId::new(value.to_owned())
}

#[test]
fn r71_configuration_bootstrap_and_versioned_replace_return_closed_receipts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sigil.toml");
    let service = AuthorityBorrowedConfigurationServiceV1::new(&path);
    let first = service
        .publish(BorrowedConfigurationRequestV1 {
            schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
            capsule_id: capsule("configuration-bootstrap"),
            operation: BorrowedConfigurationOperationV1::Bootstrap,
            expected_current_hash: None,
            config: config(),
        })
        .expect("bootstrap");
    assert!(first.previous_identity.is_none());
    assert!(first.committed_version > 0);
    let previous = fs::read(&path).expect("persisted config");
    let mut next = config();
    next.agent.model = "next-model".to_owned();
    let second = service
        .publish(BorrowedConfigurationRequestV1 {
            schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
            capsule_id: capsule("configuration-replace"),
            operation: BorrowedConfigurationOperationV1::VersionedReplace,
            expected_current_hash: Some(digest_bytes(&previous)),
            config: next,
        })
        .expect("replace");
    assert_eq!(second.previous_version, Some(first.committed_version));
    assert_ne!(first.committed_identity, second.committed_identity);
    assert_eq!(
        RootConfig::load(&path).expect("reload").agent.model,
        "next-model"
    );
}

#[test]
fn r71_configuration_capsule_replay_and_drift_fail_before_publish() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sigil.toml");
    let service = AuthorityBorrowedConfigurationServiceV1::new(&path);
    service
        .publish(BorrowedConfigurationRequestV1 {
            schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
            capsule_id: capsule("configuration-replay"),
            operation: BorrowedConfigurationOperationV1::Bootstrap,
            expected_current_hash: None,
            config: config(),
        })
        .expect("bootstrap");
    let previous = fs::read(&path).expect("persisted config");
    fs::write(&path, b"externally changed").expect("drift");
    let error = service
        .publish(BorrowedConfigurationRequestV1 {
            schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
            capsule_id: capsule("configuration-drift"),
            operation: BorrowedConfigurationOperationV1::VersionedReplace,
            expected_current_hash: Some(digest_bytes(&previous)),
            config: config(),
        })
        .expect_err("drift");
    assert_eq!(error, BorrowedConfigurationErrorV1::IdentityDrift);
    let replay = service
        .publish(BorrowedConfigurationRequestV1 {
            schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
            capsule_id: capsule("configuration-replay"),
            operation: BorrowedConfigurationOperationV1::VersionedReplace,
            expected_current_hash: Some(digest_bytes(&previous)),
            config: config(),
        })
        .expect_err("replay");
    assert_eq!(replay, BorrowedConfigurationErrorV1::CapsuleReplay);
}
