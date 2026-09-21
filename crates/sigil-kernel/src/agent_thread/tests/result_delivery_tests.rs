use super::*;

fn receipt(offset: usize, length: usize, total: usize) -> AgentThreadResultDeliveredEntry {
    AgentThreadResultDeliveredEntry {
        thread_id: AgentThreadId::new("result-pages").expect("valid thread id"),
        call_id: format!("read-{offset}-{length}"),
        output_hash: "result-hash".to_owned(),
        offset_chars: offset,
        returned_chars: length,
        total_chars: total,
        truncated: offset.saturating_add(length) < total,
        delivered_at_ms: None,
    }
}

#[test]
fn delivery_coverage_is_independent_of_page_order() {
    for offsets in [
        [0, 10, 20],
        [0, 20, 10],
        [10, 0, 20],
        [10, 20, 0],
        [20, 0, 10],
        [20, 10, 0],
    ] {
        let mut coverage = AgentResultDeliveryCoverage::default();
        for offset in offsets {
            coverage.record(&receipt(offset, 10, 30));
        }
        assert_eq!(coverage.contiguous_chars(), 30, "order: {offsets:?}");
        assert!(
            coverage.fully_delivered_receipt().is_some(),
            "order: {offsets:?}"
        );
        assert_eq!(coverage.ranges, vec![(0, 30)]);
    }
}

#[test]
fn delivery_coverage_keeps_gaps_until_an_overlapping_page_fills_them() -> anyhow::Result<()> {
    let mut coverage = AgentResultDeliveryCoverage::default();
    coverage.record(&receipt(20, 10, 30));
    coverage.record(&receipt(0, 10, 30));
    coverage.record(&receipt(0, 10, 30));
    assert_eq!(coverage.contiguous_chars(), 10);
    assert!(coverage.fully_delivered_receipt().is_none());
    assert_eq!(coverage.ranges, vec![(0, 10), (20, 30)]);

    let serialized = serde_json::to_string(&coverage)?;
    let mut restored: AgentResultDeliveryCoverage = serde_json::from_str(&serialized)?;
    restored.record(&receipt(8, 15, 30));
    assert_eq!(restored.contiguous_chars(), 30);
    assert_eq!(
        restored
            .fully_delivered_receipt()
            .map(|page| page.call_id.as_str()),
        Some("read-20-10")
    );
    Ok(())
}

#[test]
fn empty_and_out_of_bounds_pages_do_not_fabricate_body_coverage() {
    let mut coverage = AgentResultDeliveryCoverage::default();
    coverage.record(&receipt(50, 0, 30));
    assert_eq!(coverage.contiguous_chars(), 0);
    assert!(coverage.fully_delivered_receipt().is_none());

    let mut empty = AgentResultDeliveryCoverage::default();
    empty.record(&receipt(0, 0, 0));
    assert_eq!(empty.contiguous_chars(), 0);
    assert!(empty.fully_delivered_receipt().is_some());
}

#[test]
fn context_boundary_and_result_replacement_reset_page_coverage() -> anyhow::Result<()> {
    let mut session = crate::Session::new("delivery", "model");
    let page = receipt(0, 30, 30);
    session.append_control(ControlEntry::AgentThreadResultDelivered(page.clone()))?;
    let after_delivery = session.entries().len();
    assert!(
        session
            .agent_result_delivery_since(&page.thread_id, &page.output_hash, 0)
            .fully_delivered_receipt()
            .is_some()
    );
    assert!(
        session
            .agent_result_delivery_since(&page.thread_id, &page.output_hash, after_delivery)
            .fully_delivered_receipt()
            .is_none()
    );
    assert!(
        session
            .agent_result_delivery_since(&page.thread_id, "different-hash", 0)
            .fully_delivered_receipt()
            .is_none()
    );

    session.append_control(ControlEntry::AgentThreadResultRecorded(
        crate::AgentThreadResultRecordedEntry {
            result: crate::AgentThreadResult {
                thread_id: page.thread_id.clone(),
                session_ref: crate::SessionRef::new_relative("children/result.jsonl")?,
                status: crate::AgentThreadTerminalStatus::Completed,
                summary: "new result with the same hash".to_owned(),
                summary_truncated: false,
                original_summary_chars: None,
                artifacts: Vec::new(),
                changed_paths: Vec::new(),
                risks: Vec::new(),
                followups: Vec::new(),
                usage: None,
                output_hash: page.output_hash.clone(),
                final_answer_ref: None,
            },
        },
    ))?;
    assert!(
        session
            .agent_result_delivery_since(&page.thread_id, &page.output_hash, 0)
            .fully_delivered_receipt()
            .is_none()
    );
    session.append_control(ControlEntry::AgentThreadResultDelivered(receipt(
        10, 20, 30,
    )))?;
    let projection = session.agent_thread_state_projection();
    assert_eq!(
        projection.threads[&page.thread_id].result_delivered_chars,
        0
    );
    assert!(!projection.threads[&page.thread_id].result_fully_delivered);
    let mut restored: super::super::AgentThreadStateProjection =
        serde_json::from_str(&serde_json::to_string(&projection)?)?;
    let prefix = ControlEntry::AgentThreadResultDelivered(receipt(0, 10, 30));
    restored.apply_control_entry(&prefix);
    session.append_control(prefix)?;
    let replayed = session.agent_thread_state_projection();
    assert!(restored.threads[&page.thread_id].result_fully_delivered);
    assert_eq!(restored.threads[&page.thread_id].result_delivered_chars, 30);
    assert_eq!(
        restored.threads[&page.thread_id].result_delivery_coverage,
        replayed.threads[&page.thread_id].result_delivery_coverage
    );
    Ok(())
}
