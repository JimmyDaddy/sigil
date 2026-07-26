use super::*;
use crate::provider_connections::{PreparedCredential, ProviderFamily};

#[tokio::test]
async fn file_store_roundtrip_rotates_and_deletes_under_private_permissions() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("sigil").join(CREDENTIAL_FILE_NAME);
    let store = FileProviderCredentialStore::new(&path);
    let id = CredentialId::random();
    let record = ProviderCredentialRecord::new(
        id.clone(),
        &PreparedCredential::api_key(ProviderFamily::OpenAi, "file-secret-canary"),
    );

    store.store(&record).await.expect("store credential");
    let loaded = store
        .load(&id)
        .await
        .expect("load credential")
        .expect("credential exists");
    assert_eq!(loaded.credential_id, id);
    assert_eq!(loaded.provider_family, ProviderFamily::OpenAi);
    assert_eq!(loaded.secret().expose_secret(), "file-secret-canary");
    assert!(store.delete(&id).await.expect("delete credential"));
    assert!(store.load(&id).await.expect("load deleted").is_none());
    assert!(
        private_path_permissions_are_restricted(path.parent().expect("credential parent"))
            .expect("parent permissions should be inspectable")
    );
    assert!(
        private_path_permissions_are_restricted(&path)
            .expect("file permissions should be inspectable")
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(path.parent().expect("credential parent"))
                .expect("parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn file_store_serializes_concurrent_copy_on_write_updates() {
    let temp = tempfile::tempdir().expect("temp directory");
    let path = temp.path().join("sigil").join(CREDENTIAL_FILE_NAME);
    let store = FileProviderCredentialStore::new(&path);
    let records = (0..16)
        .map(|index| {
            ProviderCredentialRecord::new(
                CredentialId::random(),
                &PreparedCredential::api_key(
                    ProviderFamily::DeepSeek,
                    format!("concurrent-secret-{index}"),
                ),
            )
        })
        .collect::<Vec<_>>();
    let tasks = records
        .into_iter()
        .map(|record| {
            let store = store.clone();
            tokio::spawn(async move {
                let result = store.store(&record).await;
                (record, result)
            })
        })
        .collect::<Vec<_>>();

    for task in tasks {
        let (record, result) = task.await.expect("credential task should join");
        result.expect("credential should store");
        let loaded = store
            .load(&record.credential_id)
            .await
            .expect("credential should load")
            .expect("credential should exist");
        assert_eq!(loaded.generation_id, record.generation_id);
    }
}

#[tokio::test]
async fn file_store_rejects_malformed_state_without_overwriting_it() {
    let temp = tempfile::tempdir().expect("temp directory");
    let parent = temp.path().join("sigil");
    fs::create_dir(&parent).expect("credential parent");
    let path = parent.join(CREDENTIAL_FILE_NAME);
    fs::write(&path, b"{malformed").expect("malformed fixture");
    let before = fs::read(&path).expect("malformed fixture readable");
    let store = FileProviderCredentialStore::new(&path);
    let record = ProviderCredentialRecord::new(
        CredentialId::random(),
        &PreparedCredential::api_key(ProviderFamily::OpenAi, "never-published"),
    );

    let error = store
        .store(&record)
        .await
        .expect_err("malformed store must fail closed");
    assert_eq!(
        error.code,
        ProviderCredentialErrorCode::CredentialRecordInvalid.as_str()
    );
    assert_eq!(fs::read(path).expect("fixture remains readable"), before);
}

#[cfg(unix)]
#[tokio::test]
async fn file_store_rejects_a_symlink_destination() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temp directory");
    let parent = temp.path().join("sigil");
    fs::create_dir(&parent).expect("credential parent");
    let outside = temp.path().join("outside");
    fs::write(&outside, b"outside").expect("outside fixture");
    let path = parent.join(CREDENTIAL_FILE_NAME);
    symlink(&outside, &path).expect("credential symlink");
    let store = FileProviderCredentialStore::new(&path);
    let id = CredentialId::random();
    let record = ProviderCredentialRecord::new(
        id,
        &PreparedCredential::api_key(ProviderFamily::DeepSeek, "never-written"),
    );

    assert!(store.store(&record).await.is_err());
    assert_eq!(
        fs::read_to_string(outside).expect("outside readable"),
        "outside"
    );
}
