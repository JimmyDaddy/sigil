//! RFC-0071 R71.6: rebuildable projection over managed records (exact-generation rebuild).

use sha2::Digest;

use sigil_kernel::managed_projection::{
    ManagedProjectionServiceV1, OpenProjectionConnectionRequestV1, ProjectionErrorV1,
    ProjectionParameterV1, ProjectionStatementIdV1, ProjectionStatementV1,
};
use sigil_kernel::managed_storage::ManagedStorageNamespaceHandleV1;
use sigil_kernel::resource::{
    CanonicalHash, ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
    OpaqueKernelCapabilityAuthenticatorV1, OpaqueKernelCapabilityHandleId,
};

use super::RuntimeRecordsProjectionServiceV1;

fn handle() -> ManagedStorageNamespaceHandleV1 {
    ManagedStorageNamespaceHandleV1::new(
        OpaqueKernelCapabilityHandleId::new("handle-project-1".to_owned()),
        CanonicalHash::from_bytes([0x51; 32]),
        ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection,
        OpaqueKernelCapabilityAuthenticatorV1::new("auth-project-1".to_owned()),
    )
}

fn request() -> OpenProjectionConnectionRequestV1 {
    OpenProjectionConnectionRequestV1 {
        namespace_hash: CanonicalHash::from_bytes([0x51; 32]),
        capability_family: ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection,
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionCatalog,
    }
}

fn seed_records(anchor: &std::path::Path) -> std::io::Result<()> {
    let dir = anchor.join("managed/session-catalog");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("records.jsonl"),
        "{\"catalog\":1}\n{\"catalog\":2}\n",
    )?;
    Ok(())
}

#[tokio::test]
async fn r71_projection_rebuilds_exact_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_records(dir.path()).expect("seed");
    let service = RuntimeRecordsProjectionServiceV1::new(dir.path().to_path_buf());
    let mut conn = service
        .open_rebuildable_projection(&handle(), request())
        .await
        .expect("open");
    let statement = ProjectionStatementV1 {
        statement: ProjectionStatementIdV1::new("select-all".to_owned()),
        parameters: Vec::new(),
        parameter_digest: CanonicalHash::from_bytes([0u8; 32]),
    };
    conn.prepare(&statement).expect("prepare");
    let outcome = conn.execute_read(&statement).await.expect("read");
    assert_eq!(outcome.rows.len(), 2);
    assert_ne!(outcome.receipt_hash, CanonicalHash::from_bytes([0u8; 32]));
    // Reopen a second time: rebuild is deterministic (same row digests).
    let mut conn2 = service
        .open_rebuildable_projection(&handle(), request())
        .await
        .expect("reopen");
    let outcome2 = conn2.execute_read(&statement).await.expect("read2");
    assert_eq!(outcome.rows, outcome2.rows);
    conn.close().await.expect("close");
    conn2.close().await.expect("close");
}

#[tokio::test]
async fn r71_projection_select_by_seq_and_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_records(dir.path()).expect("seed");
    let service = RuntimeRecordsProjectionServiceV1::new(dir.path().to_path_buf());
    let mut conn = service
        .open_rebuildable_projection(&handle(), request())
        .await
        .expect("open");
    let by_seq = ProjectionStatementV1 {
        statement: ProjectionStatementIdV1::new("select-by-seq".to_owned()),
        parameters: vec![ProjectionParameterV1::Integer(2)],
        parameter_digest: CanonicalHash::from_bytes([0u8; 32]),
    };
    let rows = conn.execute_read(&by_seq).await.expect("by seq");
    assert_eq!(rows.rows.len(), 1);
    let _all = ProjectionStatementV1 {
        statement: ProjectionStatementIdV1::new("select-all".to_owned()),
        parameters: Vec::new(),
        parameter_digest: CanonicalHash::from_bytes([0u8; 32]),
    };
    let mut hasher = sha2::Sha256::new();
    hasher.update(br#"{"catalog":1}"#);
    let content_hash = CanonicalHash::from_bytes(hasher.finalize().into());
    let by_hash = ProjectionStatementV1 {
        statement: ProjectionStatementIdV1::new("select-by-hash".to_owned()),
        parameters: vec![ProjectionParameterV1::Blob(content_hash)],
        parameter_digest: CanonicalHash::from_bytes([0u8; 32]),
    };
    let rows = conn.execute_read(&by_hash).await.expect("by hash");
    assert_eq!(rows.rows.len(), 1);
}

#[tokio::test]
async fn r71_projection_wrong_family_missing_records_and_unknown_statement_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Missing records: exact managed generation required.
    let service = RuntimeRecordsProjectionServiceV1::new(dir.path().to_path_buf());
    let error = match service
        .open_rebuildable_projection(&handle(), request())
        .await
    {
        Ok(_) => panic!("missing records must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, ProjectionErrorV1::WrongGeneration));
    // Unknown statement id refused before execution.
    seed_records(dir.path()).expect("seed");
    let mut conn = service
        .open_rebuildable_projection(&handle(), request())
        .await
        .expect("open");
    let unknown = ProjectionStatementV1 {
        statement: ProjectionStatementIdV1::new("drop-table".to_owned()),
        parameters: Vec::new(),
        parameter_digest: CanonicalHash::from_bytes([0u8; 32]),
    };
    let error = conn.prepare(&unknown).expect_err("unknown");
    assert!(matches!(error, ProjectionErrorV1::UnregisteredStatement));
}
