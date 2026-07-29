use super::*;

#[test]
fn refresh_batch_preserves_overflow_for_later_worker_passes() {
    let mut pending = (0..(MAX_MCP_REFRESHES_PER_PASS * 3))
        .map(|ordinal| format!("server-{ordinal:02}"))
        .collect::<BTreeSet<_>>();

    let first = take_pending_mcp_refresh_batch(&mut pending);

    assert_eq!(first.len(), MAX_MCP_REFRESHES_PER_PASS);
    assert_eq!(
        pending.len(),
        MAX_MCP_REFRESHES_PER_PASS * 2,
        "unprocessed servers must stay pending for a later urgent-check boundary"
    );
}
