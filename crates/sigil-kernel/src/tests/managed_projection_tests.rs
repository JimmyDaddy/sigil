use super::*;

#[test]
fn r71_projection_authorizer_rejects_attach_and_vacuum_into() {
    assert_eq!(
        classify_unsafe_projection("ATTACH DATABASE '/etc/passwd' AS x"),
        Some(UnsafeProjectionClassV1::AttachDetach)
    );
    assert_eq!(
        classify_unsafe_projection("VACUUM INTO '/tmp/out.sqlite'"),
        Some(UnsafeProjectionClassV1::VacuumInto)
    );
}

#[test]
fn r71_projection_authorizer_allows_benign_reads() {
    assert_eq!(
        classify_unsafe_projection("SELECT id FROM sessions LIMIT 10"),
        None
    );
}

#[test]
fn r71_projection_registry_is_closed() {
    let registry = registered_projection_statements();
    assert!(registry.contains(&ProjectionStatementIdV1::new(
        "catalog.list_sessions".to_owned()
    )));
    assert_eq!(registry.len(), 4);
}
