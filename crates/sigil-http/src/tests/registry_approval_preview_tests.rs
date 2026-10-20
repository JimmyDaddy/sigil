use super::*;

fn approval(call_id: &str, body: &str) -> HttpPendingApproval {
    HttpPendingApproval {
        call_id: call_id.to_owned(),
        tool_name: "write_file".to_owned(),
        approval_request_id: format!("request-{call_id}"),
        tool_call_hash: "a".repeat(64),
        policy_version: "policy-1".to_owned(),
        expires_at_ms: u64::MAX,
        session_grant_available: false,
        session_grant_unavailable_reason: None,
        display: crate::HttpPendingApprovalDisplay {
            event_sequence: 1,
            effects: vec!["file_write".to_owned()],
            preview: Some(crate::HttpPendingApprovalPreview {
                title: "Edit file".to_owned(),
                summary: "Frozen change".to_owned(),
                body: body.to_owned(),
            }),
            ..Default::default()
        },
    }
}

#[test]
fn snapshot_keeps_frozen_review_for_cold_renderer_and_bounds_only_the_view() {
    let mut run = HttpRunState::new(
        "run-1".to_owned(),
        "session-1".to_owned(),
        HttpPermissionMode::Manual,
        None,
        String::new(),
    );
    let body = "界".repeat(40_000);
    for index in 0..MAX_PENDING_APPROVALS_PER_RUN {
        let call_id = format!("call-{index}");
        run.pending_approvals
            .insert(call_id.clone(), approval(&call_id, &body));
    }
    for index in 0..MAX_APPROVAL_LIFECYCLES_PER_RUN {
        let call_id = format!("terminal-{index}");
        run.approval_lifecycles.insert(
            call_id.clone(),
            HttpApprovalLifecycleView {
                approval: approval(&call_id, &body),
                state: HttpApprovalLifecycleState::Terminal,
            },
        );
    }
    let snapshot = run.snapshot();
    assert_eq!(
        snapshot.pending_approvals.len(),
        MAX_PENDING_APPROVALS_PER_RUN
    );
    let reviews = snapshot
        .pending_approvals
        .iter()
        .filter_map(|approval| approval.display.preview.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(
        reviews.len(),
        2,
        "overflow is explicit absence, never a generic replacement review"
    );
    assert!(reviews.iter().all(|review| review.body == body));
    let bytes: usize = reviews
        .iter()
        .map(|review| review.title.len() + review.summary.len() + review.body.len())
        .sum();
    assert!(bytes <= MAX_RUN_APPROVAL_REVIEW_BYTES);
    assert!(
        snapshot
            .approval_lifecycles
            .iter()
            .all(|lifecycle| lifecycle.approval.display.preview.is_none())
    );
    assert!(serde_json::to_vec(&snapshot).expect("snapshot JSON").len() < 2 * 1024 * 1024);
    assert!(run.pending_approvals.values().all(|approval| {
        approval
            .display
            .preview
            .as_ref()
            .is_some_and(|review| review.body == body)
    }));
    assert!(
        run.approval_lifecycles.values().all(|lifecycle| lifecycle
            .approval
            .display
            .preview
            .is_some())
    );
    assert_eq!(
        snapshot,
        run.snapshot(),
        "reconnect does not depend on renderer memory"
    );
}

#[test]
fn snapshot_retains_honest_no_preview_guard_and_clears_terminal_review_only() {
    let mut run = HttpRunState::new(
        "run-1".to_owned(),
        "session-1".to_owned(),
        HttpPermissionMode::Manual,
        None,
        String::new(),
    );
    let mut pending = approval("missing", "unused");
    pending.display.preview = None;
    run.pending_approvals
        .insert(pending.call_id.clone(), pending.clone());
    run.approval_lifecycles.insert(
        "accepted".to_owned(),
        HttpApprovalLifecycleView {
            approval: approval("accepted", "exact accepted review"),
            state: HttpApprovalLifecycleState::DecisionAccepted,
        },
    );
    let snapshot = run.snapshot();
    assert_eq!(snapshot.pending_approvals, vec![pending]);
    assert_eq!(
        snapshot.approval_lifecycles[0]
            .approval
            .display
            .preview
            .as_ref()
            .expect("nonterminal review")
            .body,
        "exact accepted review"
    );
}
