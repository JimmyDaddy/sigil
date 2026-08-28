use super::*;

#[test]
fn authority_provider_lease_blocks_gc_for_active_tool() {
    let temp = tempfile::tempdir().expect("temp");
    let control = authority_scratch_control(temp.path().join("scratch"));
    control
        .ensure_session_scratch(
            Some("session-a"),
            &sigil_tools_builtin::ScratchQuota {
                per_session_bytes: 1024,
                workspace_hard_bytes: 4096,
            },
        )
        .expect("provision");
    let _lease = control.namespaces.acquire("session-a").expect("lease");
    let report = control
        .gc_scratch_namespaces(
            &sigil_tools_builtin::ScratchGcConfig { ttl_ms: 0 },
            u64::MAX,
        )
        .expect("gc");
    assert_eq!(report.skipped_leased, 1);
    assert_eq!(report.deleted, 0);
}
