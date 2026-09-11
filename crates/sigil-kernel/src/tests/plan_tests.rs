#![allow(clippy::unwrap_used)] // Test fixtures prefer direct unwraps.
use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, ConversationTurnRef, NetworkEffect, PlanApprovalPermission,
    PlanArtifactProjection, PlanDecision, PlanDecisionActor, PlanDecisionRecordedEntry, PlanId,
    PlanReviewAttemptEntry, PlanReviewAttemptId, PlanReviewAttemptStatus,
    PlanReviewCandidateCompletenessV1, PlanReviewId, PlanReviewSource, PlanSourceRef,
    SessionLogEntry, TaskCreatedFromPlanEntry, TaskId, TaskIsolationMode, TaskStepMode, ToolAccess,
    ToolCategory, ToolPreviewCapability, ToolSpec, plain_text_plan_draft_entry,
    plain_text_plan_draft_entry_with_plan_id, plan_draft_created_entry,
    plan_draft_created_entry_with_plan_id, plan_review_candidate_recorded_entry,
    plan_review_child_session_ref, plan_review_detail_from_entries, plan_task_input_from_draft,
    plan_text_hash, plan_workspace_paths, task_id_from_plan_draft,
};

use crate::plan::{
    PlanReviewResult, PlanReviewResultValidationError, decode_plan_review_result,
    submit_plan_review_result,
};

fn tool_spec(
    name: &str,
    category: ToolCategory,
    access: ToolAccess,
    network_effect: Option<NetworkEffect>,
    preview: ToolPreviewCapability,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: "test tool".to_owned(),
        input_schema: json!({"type": "object"}),
        category,
        access,
        network_effect,
        preview,
    }
}
fn simple_structured_plan(summary: &str, title: &str, path: &str) -> String {
    format!(
        r#"Plan:

```sigil-plan-v2
{{
  "summary": "{summary}",
  "steps": [
    {{
      "step_id": "step-1",
      "title": "{title}",
      "target_paths": ["{path}"]
    }}
  ],
  "target_paths": ["{path}"],
  "suggested_checks": [
    {{
      "check_spec_id": "cargo-test",
      "command": "cargo",
      "args": ["test", "-p", "sigil-kernel", "plan"]
    }}
  ]
}}
```
"#
    )
}

fn explicit_plan_review_attempt(
    plan_id: &PlanId,
    objective: Option<&str>,
    status: PlanReviewAttemptStatus,
    recorded_at_ms: u64,
) -> Result<PlanReviewAttemptEntry> {
    let plan_review_id = PlanReviewId::new(format!("detail-review-{}", plan_id.as_str()))?;
    let attempt_id = PlanReviewAttemptId::new(format!("detail-attempt-{}", plan_id.as_str()))?;
    Ok(PlanReviewAttemptEntry {
        plan_review_id: plan_review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: plan_id.clone(),
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef {
            session_scope_id: "detail-session".to_owned(),
            message_id: format!("plan-command-{}", plan_id.as_str()),
            logical_run_id: format!("plan-run-{}", plan_id.as_str()),
        },
        explicit_objective: objective.map(str::to_owned),
        route_decision_id: None,
        child_session_ref: plan_review_child_session_ref(&plan_review_id, &attempt_id),
        finalizer_session_ref: None,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: None,
        pending_user_input: None,
        status,
        terminal_reason: None,
        recorded_at_ms,
    })
}

#[test]
fn plan_text_hash_is_stable_and_prefixed() {
    let left = plan_text_hash("inspect then edit");
    let right = plan_text_hash("inspect then edit");

    assert_eq!(left, right);
    assert!(left.starts_with("sha256:"));
    assert_ne!(left, plan_text_hash("different plan"));
}

#[test]
fn plan_workspace_paths_extracts_conservative_workspace_scopes() {
    let paths = plan_workspace_paths(
        r#"
        1. inspect `crates/sigil-tui/src/app.rs`
        2. edit crates/sigil-tui after checking README.md.
        3. ignore https://example.com/a/b and ../outside.txt
        4. review .repo-local-dev/sigil-agent-task-subagent-redesign-technical-solution-2026-06-20.md
        "#,
    );

    assert_eq!(
        paths,
        vec![
            ".repo-local-dev/sigil-agent-task-subagent-redesign-technical-solution-2026-06-20.md",
            "README.md",
            "crates/sigil-tui",
        ]
    );
}

#[test]
fn plan_workspace_paths_returns_empty_for_plan_without_paths() {
    assert!(plan_workspace_paths("inspect the design, then propose edits").is_empty());
}

#[test]
fn plan_draft_created_entry_skips_blank_and_preserves_metadata() -> Result<()> {
    assert!(plan_draft_created_entry("   \n\t", PlanSourceRef::default(), 42, None)?.is_none());
    assert!(
        plan_draft_created_entry(
            "1. Inspect README.md\n2. Update crates/sigil-tui/src/app.rs",
            PlanSourceRef::default(),
            42,
            None
        )?
        .is_none()
    );

    let draft = plan_draft_created_entry(
        &simple_structured_plan(
            "Inspect and update TUI docs",
            "Update crates/sigil-tui/src/app.rs",
            "crates/sigil-tui/src/app.rs",
        ),
        PlanSourceRef {
            session_ref: Some("session.jsonl".to_owned()),
            run_id: Some("run_1".to_owned()),
            final_message_id: Some("msg_1".to_owned()),
            ..PlanSourceRef::default()
        },
        42,
        Some("snapshot_1".to_owned()),
    )?
    .expect("non-empty plan should create a durable draft");

    assert!(draft.plan_id.as_str().starts_with("plan_"));
    assert!(draft.plan_hash.starts_with("sha256:"));
    assert_eq!(draft.summary, "Inspect and update TUI docs");
    assert_eq!(draft.steps.len(), 1);
    assert_eq!(draft.steps[0].title, "Update crates/sigil-tui/src/app.rs");
    assert!(
        draft
            .inline_text
            .as_deref()
            .unwrap_or_default()
            .contains("Steps:")
    );
    assert!(
        draft
            .target_paths
            .iter()
            .any(|path| path == "crates/sigil-tui/src/app.rs")
    );
    assert_eq!(
        draft
            .suggested_checks
            .iter()
            .map(|check| check.check_spec_id.as_str())
            .collect::<Vec<_>>(),
        vec!["cargo-test"]
    );
    assert_eq!(draft.workspace_snapshot_id.as_deref(), Some("snapshot_1"));
    Ok(())
}

#[test]
fn plain_text_plan_is_bounded_reviewable_input_without_graph_authority() -> Result<()> {
    let plan_id = PlanId::new("plan_plain_text")?;
    assert!(
        plain_text_plan_draft_entry_with_plan_id(
            plan_id.clone(),
            "  \n\t",
            PlanSourceRef::default(),
            42,
            None,
        )?
        .is_none()
    );

    let text = "# Implementation plan\n\n1. Inspect the live path.\n2. Apply and verify.";
    let draft = plain_text_plan_draft_entry_with_plan_id(
        plan_id.clone(),
        text,
        PlanSourceRef::default(),
        42,
        Some("snapshot-1".to_owned()),
    )?
    .expect("non-empty model prose remains reviewable");

    assert_eq!(draft.plan_id, plan_id);
    assert_eq!(draft.summary, "Implementation plan");
    assert_eq!(draft.inline_text.as_deref(), Some(text));
    assert_eq!(draft.plan_hash, plan_text_hash(text));
    assert!(draft.steps.is_empty());
    assert!(draft.intent_proposal.is_none());
    assert!(draft.target_paths.is_empty());
    assert!(draft.suggested_checks.is_empty());
    assert_eq!(draft.workspace_snapshot_id.as_deref(), Some("snapshot-1"));
    Ok(())
}

#[test]
fn plain_text_plan_derives_stable_host_identity_from_persisted_text() -> Result<()> {
    let source = PlanSourceRef::default();
    let left = plain_text_plan_draft_entry(
        "# Implement\n\n1. Inspect.\n2. Edit and verify.",
        source.clone(),
        42,
        None,
    )?
    .expect("plain model output should remain reviewable");
    let right = plain_text_plan_draft_entry(
        "  # Implement\n\n1. Inspect.\n2. Edit and verify.  ",
        source,
        43,
        None,
    )?
    .expect("equivalent trimmed output should remain reviewable");

    assert_eq!(left.plan_id, right.plan_id);
    assert_eq!(left.plan_hash, right.plan_hash);
    assert_eq!(left.summary, "Implement");
    assert!(left.steps.is_empty());
    Ok(())
}

#[test]
fn plan_task_input_uses_human_readable_plan_without_step_translation() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"计划如下。

```sigil-plan-v2
{
  "summary": "Fix README typo",
  "steps": [
    {
      "step_id": "fix-readme-typo",
      "title": "Fix README.md line 3 typo",
      "detail": "第 3 行 \"This docs has typoo.\" 中 \"typoo\" 拼写错误，修复为 typo。",
      "target_paths": ["README.md"],
      "acceptance": ["README.md line 3 no longer contains typoo"]
    }
  ],
  "target_paths": ["README.md"]
}

```

是否需要我执行这个修改？
"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("non-empty plan should create a durable draft");
    let task_input = plan_task_input_from_draft(&draft);

    assert!(task_input.contains("Execute the following user-approved Plan"));
    assert!(task_input.contains("Treat the complete Plan as task context"));
    assert!(task_input.contains("inspect the live workspace"));
    assert!(task_input.contains("Approved Plan:"));
    assert!(task_input.contains("This docs has typoo"));
    assert_eq!(draft.target_paths, vec!["README.md"]);
    Ok(())
}

#[test]
fn legacy_sigil_plan_v2_preserves_structured_steps_without_task_authority() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Inspect then report",
  "steps": [
    {
      "step_id": "inspect",
      "title": "Inspect README",
      "role": "executor",
      "depends_on": [],
      "mode": "read",
      "isolation": "shared_read_only",
      "target_paths": ["README.md"]
    },
    {
      "step_id": "report",
      "title": "Report findings",
      "role": "subagent_read",
      "depends_on": ["inspect"],
      "mode": "read",
      "isolation": "shared_read_only",
      "target_paths": ["README.md"]
    }
  ],
  "target_paths": ["README.md"]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("v2 plan should create a draft");
    assert_eq!(draft.schema_version, 2);
    assert_eq!(
        task_id_from_plan_draft(&draft)?,
        task_id_from_plan_draft(&draft)?
    );

    assert_eq!(draft.steps.len(), 2);
    assert_eq!(draft.steps[1].depends_on, vec!["inspect"]);
    assert_eq!(draft.steps[0].mode, Some(TaskStepMode::Read));
    assert_eq!(
        draft.steps[0].isolation,
        Some(TaskIsolationMode::SharedReadOnly)
    );
    let entries = vec![SessionLogEntry::Control(ControlEntry::PlanDraftCreated(
        draft,
    ))];
    assert!(
        crate::TaskStateProjection::from_entries(&entries)
            .tasks
            .is_empty()
    );
    Ok(())
}

#[test]
fn sigil_plan_v2_rejects_verify_as_a_participant_step() {
    let error = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Compile the workspace",
  "steps": [{
    "step_id": "verify",
    "title": "Run tests",
    "role": "executor",
    "depends_on": [],
    "mode": "verify",
    "isolation": "shared_read_only"
  }],
  "target_paths": ["Cargo.toml"]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("verification must be system-owned, not a participant step");

    assert!(
        error
            .to_string()
            .contains("cannot create verify participant steps")
    );
}

#[test]
fn sigil_plan_v2_rejects_verification_run_as_participant_capability() {
    let error = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Inspect before trusted verification",
  "steps": [{
    "step_id": "inspect",
    "title": "Inspect Cargo",
    "role": "executor",
    "depends_on": [],
    "mode": "read",
    "isolation": "shared_read_only",
    "required_capabilities": ["verification_run"],
    "suggested_checks": ["cargo check"]
  }],
  "target_paths": ["Cargo.toml"]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("provider text must not delegate host-owned verification");

    assert!(
        error
            .to_string()
            .contains("cannot delegate verification_run")
    );
}

#[test]
fn sigil_plan_v2_carries_digest_bound_intent_proposal_without_runtime_authority() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Implement and verify retry behavior",
  "intents": [
    {
      "intent_alias": "retry",
      "title": "Retry behavior",
      "statement": "Retries preserve the original operation semantics.",
      "acceptance_criteria": [
        {
          "criterion_alias": "retry-test",
          "statement": "The retry regression test passes.",
          "required": true
        }
      ],
      "depends_on_aliases": []
    }
  ],
  "steps": [
    {
      "id": "implement-retry",
      "title": "Implement retry behavior",
      "role": "executor",
      "depends_on": [],
      "intent_aliases": ["retry"],
      "mode": "write",
      "isolation": "sequential_workspace_write",
      "target_paths": ["src/retry.rs"]
    }
  ],
  "target_paths": ["src/retry.rs"]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("intent-enabled plan should create a durable draft");

    let proposal = draft
        .intent_proposal
        .as_ref()
        .expect("provider intent proposal should remain explicit and unaccepted");
    proposal.validate_contract()?;
    assert_eq!(proposal.intents[0].intent_alias, "retry");
    assert_eq!(
        proposal.proposal_digest,
        proposal.computed_digest()?,
        "the host must bind the exact provider proposal"
    );
    assert_eq!(draft.steps[0].intent_aliases, vec!["retry"]);
    let entries = vec![SessionLogEntry::Control(ControlEntry::PlanDraftCreated(
        draft,
    ))];
    assert!(
        crate::TaskStateProjection::from_entries(&entries)
            .tasks
            .is_empty()
    );
    Ok(())
}

#[test]
fn legacy_sigil_plan_v2_keeps_unbound_intent_aliases_as_readable_context() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Unsafe partial intent plan",
  "steps": [{
    "id": "write",
    "title": "Write file",
    "role": "executor",
    "depends_on": [],
    "intent_aliases": ["missing"],
    "mode": "write",
    "isolation": "sequential_workspace_write"
  }]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("readable plan must remain reviewable before task materialization");

    assert!(draft.intent_proposal.is_none());
    assert_eq!(draft.steps[0].intent_aliases, vec!["missing"]);
    let entries = vec![SessionLogEntry::Control(ControlEntry::PlanDraftCreated(
        draft,
    ))];
    assert!(
        crate::TaskStateProjection::from_entries(&entries)
            .tasks
            .is_empty()
    );
    Ok(())
}

#[test]
fn sigil_plan_v2_accepts_single_string_notes_and_acceptance() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Fix README typo",
  "steps": [
    {
      "step_id": "fix-readme-typo",
      "title": "Fix README marker",
      "target_paths": ["README.md"],
      "notes": "One token replacement.",
      "acceptance": "README.md contains the corrected marker."
    }
  ],
  "target_paths": ["README.md"],
  "notes": "Plan mode only; no files were modified."
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("single-string notes and acceptance should remain durable");

    assert_eq!(draft.notes, vec!["Plan mode only; no files were modified."]);
    assert_eq!(draft.steps[0].step_id, "fix-readme-typo");
    assert_eq!(draft.steps[0].notes, vec!["One token replacement."]);
    assert_eq!(
        draft.steps[0].acceptance_criteria,
        vec!["README.md contains the corrected marker."]
    );
    Ok(())
}

#[test]
fn sigil_plan_v2_block_creates_structured_executable_plan() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"计划如下。

```sigil-plan-v2
{
  "summary": "Fix README typo",
  "steps": [
    {
      "step_id": "fix-readme-typo",
      "title": "Fix README.md line 3 typo",
      "mode": "write",
      "target_paths": ["README.md"],
      "acceptance": ["README.md line 3 no longer contains typoo"]
    },
    {
      "step_id": "verify-readme",
      "title": "Verify README.md wording",
      "mode": "read",
      "target_paths": ["README.md"]
    }
  ],
  "suggested_checks": []
}
```
"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("non-empty plan should create a durable draft");
    let task_input = plan_task_input_from_draft(&draft);

    assert_eq!(draft.summary, "Fix README typo");
    assert_eq!(draft.target_paths, vec!["README.md"]);
    assert_eq!(draft.steps.len(), 2);
    assert!(task_input.contains("Fix README.md line 3 typo"));
    assert!(!task_input.contains("sigil-plan-v2"));
    assert!(task_input.contains("fix-readme-typo"));
    Ok(())
}

#[test]
fn plan_artifact_projection_tracks_pending_decision_and_created_task() -> Result<()> {
    let draft = plan_draft_created_entry(
        &simple_structured_plan("Update README", "Update README.md", "README.md"),
        PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("draft");
    let decision = PlanDecisionRecordedEntry {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        decision: PlanDecision::Accepted,
        decided_by: PlanDecisionActor::User,
        decided_at_ms: 2,
        reason: Some("looks good".to_owned()),
    };
    let created = TaskCreatedFromPlanEntry {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        task_id: TaskId::new("task_1")?,
        task_plan_version: 1,
        step_mapping: Vec::new(),
        created_at_ms: 3,
        stale_reason: None,
    };
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft.clone())),
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision.clone())),
        SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(created.clone())),
    ];

    let accepted_without_task = PlanArtifactProjection::from_entries(&entries[..2]);
    assert_eq!(accepted_without_task.latest_pending_plan(), Some(&draft));

    let projection = PlanArtifactProjection::from_entries(&entries);

    assert_eq!(projection.latest_plan(), Some(&draft));
    assert!(projection.latest_pending_plan().is_none());
    assert_eq!(projection.latest_decision(&draft.plan_id), Some(&decision));
    assert_eq!(
        projection.tasks_created.get(&draft.plan_id),
        Some(&vec![created])
    );
    Ok(())
}

#[test]
fn plan_draft_projects_sensitive_model_text_before_hash_and_persistence() -> Result<()> {
    let raw_url = "https://example.com/private?signature=plan-draft-secret";
    let raw = simple_structured_plan(
        &format!("Inspect {raw_url}"),
        "Use token=plan-step-secret",
        "README.md",
    );

    let draft = plan_draft_created_entry(&raw, PlanSourceRef::default(), 1, None)?
        .expect("structured plan should produce a draft");
    let durable = serde_json::to_string(&draft)?;

    for forbidden in [raw_url, "plan-draft-secret", "plan-step-secret"] {
        assert!(!durable.contains(forbidden));
    }
    assert_eq!(
        draft.plan_hash,
        plan_text_hash(
            draft
                .inline_text
                .as_deref()
                .expect("sensitive plan should retain bounded safe inline text")
        )
    );
    Ok(())
}

#[test]
fn plan_review_detail_preserves_complete_typed_content_and_exact_hash() -> Result<()> {
    let plan_id = PlanId::new("plan-detail-v1")?;
    let args = json!({
        "schema_version": 2,
        "summary": "Inspect the durable lifecycle, repair recovery, and verify every public surface.",
        "steps": [{
            "step_id": "inspect",
            "title": "Inspect durable lifecycle",
            "detail": "Trace every append-only transition before changing the reducer.",
            "role": "planner",
            "depends_on": [],
            "mode": "serial",
            "isolation": "shared_workspace",
            "target_paths": ["crates/sigil-kernel/src/plan.rs"],
            "suggested_checks": ["cargo test -p sigil-kernel plan_review_detail"],
            "risk": "medium",
            "notes": ["Preserve exact identity bindings."]
        }],
        "target_paths": ["crates/sigil-kernel/src/plan.rs"],
        "suggested_checks": ["cargo test -p sigil-kernel plan_review_detail"],
        "notes": ["Use the shared converter."]
    });
    let draft = plan_draft_created_entry_with_plan_id(
        plan_id.clone(),
        &format!("```sigil-plan-v2\n{}\n```", &serde_json::to_string(&args)?),
        PlanSourceRef::default(),
        42,
        Some("snapshot-detail".to_owned()),
    )?
    .expect("typed plan");
    let entries = vec![SessionLogEntry::Control(ControlEntry::PlanDraftCreated(
        draft.clone(),
    ))];

    let detail = plan_review_detail_from_entries(&entries, &plan_id, &draft.plan_hash)?;

    assert_eq!(detail.summary, draft.summary);
    assert_eq!(detail.steps.len(), 1);
    assert_eq!(
        detail.steps[0].detail.as_deref(),
        Some("Trace every append-only transition before changing the reducer.")
    );
    assert_eq!(
        detail.workspace_snapshot_id.as_deref(),
        Some("snapshot-detail")
    );
    assert!(detail.legacy_markdown.is_none());
    assert!(plan_review_detail_from_entries(&entries, &plan_id, "sha256:stale").is_err());
    Ok(())
}

#[test]
fn plan_review_detail_rejects_a_legacy_explicit_attempt_without_its_objective() -> Result<()> {
    let plan_id = PlanId::new("plan-detail-legacy-explicit")?;
    let draft = plain_text_plan_draft_entry_with_plan_id(
        plan_id.clone(),
        "Reject missing explicit objective",
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("nonempty draft");
    let current = explicit_plan_review_attempt(
        &plan_id,
        Some("Original explicit plan objective"),
        PlanReviewAttemptStatus::Started,
        43,
    )?;
    let mut legacy = serde_json::to_value(current)?;
    legacy
        .as_object_mut()
        .expect("attempt object")
        .remove("explicit_objective");
    let legacy: PlanReviewAttemptEntry = serde_json::from_value(legacy)?;
    assert!(legacy.explicit_objective.is_none());
    let ready = PlanReviewAttemptEntry {
        status: PlanReviewAttemptStatus::DraftReady,
        recorded_at_ms: 44,
        ..legacy.clone()
    };
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft.clone())),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(legacy)),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(ready)),
    ];

    assert!(plan_review_detail_from_entries(&entries, &plan_id, &draft.plan_hash).is_err());
    Ok(())
}

#[test]
fn plan_review_detail_accepts_one_valid_review_without_trusting_an_unrelated_conflict() -> Result<()>
{
    let plan_id = PlanId::new("plan-detail-explicit-valid")?;
    let draft = plain_text_plan_draft_entry_with_plan_id(
        plan_id.clone(),
        "Preserve explicit source",
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("nonempty draft");
    let started = explicit_plan_review_attempt(
        &plan_id,
        Some("Original explicit plan objective"),
        PlanReviewAttemptStatus::Started,
        43,
    )?;
    let ready = PlanReviewAttemptEntry {
        status: PlanReviewAttemptStatus::DraftReady,
        recorded_at_ms: 44,
        ..started.clone()
    };
    let unrelated = explicit_plan_review_attempt(
        &PlanId::new("plan-detail-unrelated-invalid")?,
        None,
        PlanReviewAttemptStatus::Started,
        45,
    )?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft.clone())),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(started)),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(ready)),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(unrelated)),
    ];

    let detail = plan_review_detail_from_entries(&entries, &plan_id, &draft.plan_hash)?;
    assert_eq!(detail.source, PlanReviewSource::ExplicitPlanCommand);
    Ok(())
}

#[test]
fn plan_review_detail_rejects_later_cross_review_identity_collisions() -> Result<()> {
    let plan_id = PlanId::new("plan-detail-collision-target")?;
    let draft = plain_text_plan_draft_entry_with_plan_id(
        plan_id.clone(),
        "Preserve exact review identity",
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("nonempty draft");
    let mut started = explicit_plan_review_attempt(
        &plan_id,
        Some("Original objective"),
        PlanReviewAttemptStatus::Started,
        43,
    )?;
    started.source = PlanReviewSource::AutomaticConversationRoute;
    started.explicit_objective = None;
    started.route_decision_id = Some(crate::ConversationRouteDecisionId::new(
        "detail-route-identity",
    )?);
    let ready = PlanReviewAttemptEntry {
        status: PlanReviewAttemptStatus::DraftReady,
        recorded_at_ms: 44,
        ..started.clone()
    };
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft.clone())),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(started.clone())),
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(ready)),
    ];
    plan_review_detail_from_entries(&entries, &plan_id, &draft.plan_hash)?;
    let unrelated = explicit_plan_review_attempt(
        &PlanId::new("plan-detail-collision-other")?,
        Some("Other objective"),
        PlanReviewAttemptStatus::Started,
        45,
    )?;
    let mut attempt_collision = unrelated.clone();
    attempt_collision.attempt_id = started.attempt_id.clone();
    let mut route_collision = unrelated;
    route_collision.source = PlanReviewSource::AutomaticConversationRoute;
    route_collision.explicit_objective = None;
    route_collision.route_decision_id = started.route_decision_id.clone();
    for collision in [attempt_collision, route_collision] {
        let other_review = collision.plan_review_id.clone();
        let mut corrupted = entries.clone();
        corrupted.push(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(
            collision,
        )));
        let projection = crate::PlanReviewProjection::from_entries(&corrupted);
        for review_id in [&started.plan_review_id, &other_review] {
            assert!(
                !projection
                    .review(review_id)
                    .expect("both owners exist")
                    .conflicts
                    .is_empty()
            );
        }
        assert!(plan_review_detail_from_entries(&corrupted, &plan_id, &draft.plan_hash).is_err());
    }
    Ok(())
}

#[test]
fn typed_plan_review_result_preserves_complete_draft_content_and_hash() -> Result<()> {
    let content =
        "# Review the coordinator\n\n1. Trace the state transitions.\n2. Add focused tests.";
    let args = serde_json::to_string(&json!({
        "schema_version": 1,
        "outcome": "draft",
        "content": content,
    }))?;
    let envelope = decode_plan_review_result(&args)?;
    assert_eq!(envelope.content, content);
    let plan_id = PlanId::new("typed-result-draft")?;
    let result =
        submit_plan_review_result(&args, plan_id.clone(), PlanSourceRef::default(), 42, None)?;

    let PlanReviewResult::Draft(draft) = result else {
        panic!("typed draft result must produce a draft");
    };
    assert_eq!(draft.plan_id, plan_id);
    assert_eq!(draft.inline_text.as_deref(), Some(content));
    assert_eq!(draft.plan_hash, plan_text_hash(content));
    // The small result envelope does not make execution hints mandatory.
    assert!(draft.steps.is_empty());
    Ok(())
}

#[test]
fn typed_plan_review_no_plan_has_no_draft_artifact_or_run_candidate() -> Result<()> {
    let result = submit_plan_review_result(
        r#"{
            "schema_version": 1,
            "outcome": "no_plan",
            "content": "The requested scope is not specified enough to form a safe Plan."
        }"#,
        PlanId::new("typed-result-no-plan")?,
        PlanSourceRef::default(),
        42,
        None,
    )?;

    let PlanReviewResult::NoPlan { reason } = result else {
        panic!("no_plan result must not materialize a draft");
    };
    assert!(reason.contains("not specified"));
    // A no_plan result has no PlanDraftCreatedEntry, so it cannot make the durable Plan
    // projection ready or expose the direct Plan run path.
    let projection = PlanArtifactProjection::from_entries(&[]);
    assert!(!projection.plan_is_ready(&PlanId::new("typed-result-no-plan")?));
    Ok(())
}

#[test]
fn plan_review_candidate_is_immutable_attempt_bound_evidence() -> Result<()> {
    let review_id = PlanReviewId::new("candidate-review")?;
    let attempt_id = PlanReviewAttemptId::new("candidate-attempt")?;
    let plan_id = PlanId::new("candidate-plan")?;
    let source = PlanSourceRef {
        plan_review_id: Some(review_id.clone()),
        source_turn: Some(ConversationTurnRef {
            session_scope_id: "session".to_owned(),
            message_id: "source".to_owned(),
            logical_run_id: "run".to_owned(),
        }),
        ..PlanSourceRef::default()
    };
    let candidate = plan_review_candidate_recorded_entry(
        review_id.clone(),
        attempt_id.clone(),
        plan_id.clone(),
        source,
        Some("assistant-1".to_owned()),
        "# Candidate\n\n1. Preserve the exact body.",
        PlanReviewCandidateCompletenessV1::Complete,
        42,
    )?;
    assert_eq!(candidate.schema_version, 1);
    assert_eq!(candidate.content_hash, plan_text_hash(&candidate.content));

    let entry = SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
        candidate.clone(),
    )));
    let encoded = serde_json::to_value(&entry)?;
    let decoded: SessionLogEntry = serde_json::from_value(encoded)?;
    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::PlanReviewCandidateRecordedV1(ref value))
            if value.as_ref() == &candidate
                && value.plan_review_id == review_id
                && value.attempt_id == attempt_id
                && value.plan_id == plan_id
    ));
    Ok(())
}

#[test]
fn typed_plan_review_result_rejects_unknown_fields_and_invalid_outcomes_fail_closed() -> Result<()>
{
    let plan_id = PlanId::new("typed-result-invalid")?;
    let unknown_field = r#"{
        "schema_version": 1,
        "outcome": "draft",
        "content": "A complete Plan",
        "task_id": "must-not-be-authority"
    }"#;
    let error = submit_plan_review_result(
        unknown_field,
        plan_id.clone(),
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("unknown result fields must fail closed");
    assert!(
        error
            .downcast_ref::<PlanReviewResultValidationError>()
            .is_some()
    );
    assert!(error.to_string().contains("invalid_result_envelope"));

    let invalid_outcome = r#"{
        "schema_version": 1,
        "outcome": "completed",
        "content": "A complete Plan"
    }"#;
    let error =
        submit_plan_review_result(invalid_outcome, plan_id, PlanSourceRef::default(), 42, None)
            .expect_err("unknown outcomes must fail closed");
    let issue = error
        .downcast_ref::<PlanReviewResultValidationError>()
        .expect("invalid outcome should expose typed validation");
    assert_eq!(issue.issue.code, "invalid_result_envelope");
    assert_eq!(issue.issue.field_path, "$");

    let empty_content = r#"{
        "schema_version": 1,
        "outcome": "no_plan",
        "content": "  "
    }"#;
    let error = submit_plan_review_result(
        empty_content,
        PlanId::new("typed-result-empty")?,
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("empty result content must fail closed");
    let issue = error
        .downcast_ref::<PlanReviewResultValidationError>()
        .expect("empty content should expose typed validation");
    assert_eq!(issue.issue.code, "empty_result_content");
    assert_eq!(issue.issue.field_path, "$.content");

    let oversized = serde_json::to_string(&json!({
        "schema_version": 1,
        "outcome": "draft",
        "content": "x".repeat(64 * 1024 + 1),
    }))?;
    let error = submit_plan_review_result(
        &oversized,
        PlanId::new("typed-result-oversized")?,
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("oversized result content must fail closed");
    assert_eq!(
        error
            .downcast_ref::<PlanReviewResultValidationError>()
            .expect("oversized content should expose typed validation")
            .issue
            .code,
        "result_content_too_large"
    );
    Ok(())
}

#[test]
fn plan_review_rejects_oversized_summary_instead_of_truncating_detail() -> Result<()> {
    let args = json!({
        "schema_version": 2,
        "summary": "x".repeat(2 * 1024 + 1),
        "steps": [{"step_id": "inspect", "title": "Inspect"}],
        "target_paths": [],
        "suggested_checks": []
    });

    let error = plan_draft_created_entry_with_plan_id(
        PlanId::new("plan-summary-too-large")?,
        &format!("```sigil-plan-v2\n{}\n```", &serde_json::to_string(&args)?),
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("oversized summaries must fail closed");

    assert!(error.to_string().contains("2048-byte"));
    Ok(())
}

#[test]
fn plan_display_name_is_bounded_at_commit_and_legacy_text_remains_readable() -> Result<()> {
    let oversized = "提交 code-intel / mcp / provider-deepseek / tools-builtin";
    let args = json!({
        "schema_version": 2,
        "summary": "Commit support crates",
        "steps": [{
            "step_id": "commit-support",
            "title": "Commit the support crates as one coherent batch",
            "display_name": oversized,
            "role": "executor",
            "mode": "write",
            "isolation": "sequential_workspace_write",
            "target_paths": ["crates"]
        }],
        "target_paths": ["crates"],
        "suggested_checks": []
    });
    let mut draft = plan_draft_created_entry_with_plan_id(
        PlanId::new("bounded-display-name")?,
        &format!("```sigil-plan-v2\n{}\n```", &serde_json::to_string(&args)?),
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("typed plan");

    let committed = draft.steps[0]
        .display_name
        .as_deref()
        .expect("display name");
    assert_eq!(
        committed.chars().count(),
        crate::TASK_AGENT_DISPLAY_NAME_MAX_CHARS
    );
    assert!(committed.ends_with('…'));
    // Historical display text remains readable without manufacturing a TaskPlan from it.
    draft.steps[0].display_name = Some(oversized.to_owned());
    let persisted = serde_json::to_string(&draft)?;
    let replayed: crate::PlanDraftCreatedEntry = serde_json::from_str(&persisted)?;
    assert_eq!(replayed.steps[0].display_name.as_deref(), Some(oversized));
    Ok(())
}

#[test]
fn workspace_edits_plan_permission_does_not_cover_shell_network_mcp_or_agent() {
    let permission = PlanApprovalPermission::WorkspaceEdits;

    assert!(permission.covers_tool(&tool_spec(
        "edit_file",
        ToolCategory::File,
        ToolAccess::Write,
        None,
        ToolPreviewCapability::Required
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "write_file_without_preview",
        ToolCategory::File,
        ToolAccess::Write,
        None,
        ToolPreviewCapability::None
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "bash",
        ToolCategory::Shell,
        ToolAccess::Execute,
        None,
        ToolPreviewCapability::None
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "web_fetch",
        ToolCategory::Custom,
        ToolAccess::Read,
        Some(NetworkEffect::Read),
        ToolPreviewCapability::None
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "mcp__filesystem__read",
        ToolCategory::Mcp,
        ToolAccess::Write,
        None,
        ToolPreviewCapability::Optional
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "spawn_agent",
        ToolCategory::Agent,
        ToolAccess::Execute,
        None,
        ToolPreviewCapability::Required
    )));
    assert!(!PlanApprovalPermission::Ask.covers_tool(&tool_spec(
        "edit_file",
        ToolCategory::File,
        ToolAccess::Write,
        None,
        ToolPreviewCapability::Required
    )));
}

// Historical execution records are fixed snapshots, never output of a retired generator.
#[derive(serde::Deserialize)]
struct HistoricalPlanExecutionFixture {
    draft: crate::PlanDraftCreatedEntry,
    adoption: crate::PlanExecutionAdoptedV1Entry,
}

fn historical_plan_execution_fixture() -> HistoricalPlanExecutionFixture {
    serde_json::from_str(include_str!("fixtures/historical_plan_execution_v1.json"))
        .expect("historical execution fixture must decode")
}

#[test]
fn historical_execution_candidate_round_trips_and_validates_its_fixed_hash() {
    let fixture = historical_plan_execution_fixture();
    let candidate = fixture.adoption.adopted_candidate;
    candidate
        .validate()
        .expect("historical candidate must self-check");
    assert_eq!(candidate.plan_id, fixture.draft.plan_id);
    assert_eq!(candidate.plan_hash, fixture.draft.plan_hash);
    assert_eq!(
        candidate.task_id,
        task_id_from_plan_draft(&fixture.draft).unwrap()
    );
    assert_eq!(candidate.task_plan.plan_version, 1);
    assert_eq!(candidate.step_contracts.len(), 1);
    let decoded: crate::ExecutablePlanCandidateV1 =
        serde_json::from_str(&serde_json::to_string(&candidate).unwrap()).unwrap();
    assert_eq!(decoded, *candidate);
    let mut changed = decoded;
    changed.safe_objective.push_str(" changed");
    assert!(
        changed.validate().is_err(),
        "historical hashes must reject altered payloads"
    );
}

#[test]
fn historical_candidate_canonical_hash_survives_json_field_reordering() {
    let fixture = historical_plan_execution_fixture();
    let candidate = fixture.adoption.adopted_candidate;
    let value = serde_json::to_value(&candidate).unwrap();
    let decoded: crate::ExecutablePlanCandidateV1 = serde_json::from_value(value).unwrap();
    assert_eq!(
        crate::candidate_canonical_hash(&decoded).unwrap(),
        candidate.candidate_hash
    );
}

#[test]
fn historical_compile_failure_round_trips_without_becoming_execution_authority() {
    let fixture = historical_plan_execution_fixture();
    let failure = crate::PlanCompileFailureV1 {
        plan_id: fixture.draft.plan_id.clone(),
        plan_hash: fixture.draft.plan_hash.clone(),
        reason_code: "incomplete_step_contract".to_owned(),
        reason: "plan step is missing its role, mode or isolation contract".to_owned(),
        affected_step: Some("step_1".to_owned()),
        compile_binding: Some(fixture.adoption.adopted_candidate.compile_binding.clone()),
        failed_at_ms: 20,
    };
    failure.validate().unwrap();
    let entry = SessionLogEntry::Control(ControlEntry::PlanCompileFailedV1(failure));
    let decoded: SessionLogEntry =
        serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
    let projection = PlanArtifactProjection::from_entries(std::slice::from_ref(&decoded));
    assert_eq!(
        projection.plan_ready_state(&fixture.draft.plan_id),
        crate::PlanReadyStateV1::CompileFailed
    );
    assert!(
        crate::TaskStateProjection::from_entries(&[decoded])
            .tasks
            .is_empty()
    );
}

#[test]
fn durable_draft_is_ready_while_candidate_state_remains_advisory() {
    let fixture = historical_plan_execution_fixture();
    let draft = fixture.draft;
    let plan_id = draft.plan_id.clone();
    let mut projection = PlanArtifactProjection::default();
    projection.apply_draft(&draft);
    // A durable draft is reviewable and directly runnable without a model-authored candidate.
    assert_eq!(
        projection.plan_ready_state(&plan_id),
        crate::PlanReadyStateV1::Ready
    );
    let candidate = *fixture.adoption.adopted_candidate;
    projection
        .candidates
        .insert(plan_id.clone(), candidate.clone());
    // Legacy candidate evidence cannot make the readable Plan less runnable.
    assert_eq!(
        projection.plan_ready_state(&plan_id),
        crate::PlanReadyStateV1::Ready
    );
    projection.ready_markers.insert(
        plan_id.clone(),
        crate::PlanReadyCommittedV1Entry {
            plan_id: plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            candidate_hash: candidate.candidate_hash.clone(),
            attempt_id: "attempt-1".to_owned(),
            committed_at_ms: 20,
        },
    );
    assert_eq!(
        projection.plan_ready_state(&plan_id),
        crate::PlanReadyStateV1::Ready
    );
    // Even mismatched advisory evidence cannot invalidate the durable Plan text.
    projection
        .ready_markers
        .get_mut(&plan_id)
        .unwrap()
        .candidate_hash = "sha256:other".to_owned();
    assert_eq!(
        projection.plan_ready_state(&plan_id),
        crate::PlanReadyStateV1::Ready
    );
    // Candidate/marker crash prefixes without their Plan remain non-runnable legacy evidence.
    projection.plans.remove(&plan_id);
    assert_eq!(
        projection.plan_ready_state(&plan_id),
        crate::PlanReadyStateV1::CandidatePrepared
    );
}

#[test]
fn historical_adoption_replays_plan_and_task_authority() {
    let fixture = historical_plan_execution_fixture();
    let draft = fixture.draft;
    let plan_id = draft.plan_id.clone();
    let adoption = fixture.adoption;
    let candidate = *adoption.adopted_candidate.clone();
    adoption.validate().expect("adoption event must validate");
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft.clone())),
        SessionLogEntry::Control(ControlEntry::ExecutablePlanCandidatePreparedV1(Box::new(
            candidate,
        ))),
        SessionLogEntry::Control(ControlEntry::PlanReadyCommittedV1(
            crate::PlanReadyCommittedV1Entry {
                plan_id: plan_id.clone(),
                plan_hash: draft.plan_hash.clone(),
                candidate_hash: adoption.candidate_hash.clone(),
                attempt_id: "attempt-1".to_owned(),
                committed_at_ms: 20,
            },
        )),
        SessionLogEntry::Control(ControlEntry::PlanExecutionAdoptedV1(Box::new(adoption))),
    ];
    let artifacts = PlanArtifactProjection::from_entries(&entries);
    assert_eq!(artifacts.adoptions.len(), 1);
    assert_eq!(
        artifacts
            .adoption_for_command("run-command-1")
            .unwrap()
            .task_id,
        artifacts
            .adoption_for_task(
                &artifacts
                    .adoption_for_command("run-command-1")
                    .unwrap()
                    .task_id
            )
            .unwrap()
            .task_id
    );
    // Decision and task-created views derive from the single adoption event.
    assert_eq!(
        artifacts.latest_decision(&plan_id).unwrap().decision,
        PlanDecision::Accepted
    );
    assert!(artifacts.task_created_for_plan(&plan_id));
    assert!(!artifacts.plan_is_rejected(&plan_id));
    // Task views derive from the adoption event without separate TaskRun/TaskPlan records.
    let tasks = crate::TaskStateProjection::from_entries(&entries);
    let task_id = artifacts
        .adoption_for_command("run-command-1")
        .unwrap()
        .task_id
        .clone();
    let task = tasks
        .tasks
        .get(&task_id)
        .expect("adopted task must project");
    assert_eq!(task.status, crate::TaskRunStatus::Started);
    assert_eq!(
        task.title.as_deref(),
        Some("Historical implementation plan")
    );
    assert_eq!(
        tasks.execution_phase(&task_id),
        Some(crate::TaskExecutionPhaseV1::Preparing)
    );
    assert_eq!(task.latest_plan_version, Some(1));
    assert_eq!(task.plans.get(&1).unwrap().step_contracts.len(), 1);
    assert!(task.plans.get(&1).unwrap().contract_set_committed_v2);
    assert_eq!(tasks.current_task_id.as_ref(), Some(&task_id));
}

#[test]
fn materialization_attempts_replay_block_then_prepare_without_replacing_task_shell() {
    let fixture = historical_plan_execution_fixture();
    let draft = fixture.draft;
    let plan_id = draft.plan_id.clone();
    let candidate = *fixture.adoption.adopted_candidate;
    let task_id = candidate.task_id.clone();
    let blocker = crate::TaskBlockerV1 {
        reason_code: crate::TaskBlockerReasonCodeV1::ContractRecompileRequired,
        summary: "task preparation needs a corrected contract".to_owned(),
        affected_step: None,
        affected_capability: None,
        retryable: true,
        available_actions: vec![crate::TaskBlockerActionV1::RetryAdmission],
        evidence_digest: crate::stable_event_hash(b"materialization-blocked"),
        created_at_ms: 20,
        resolved_at_ms: None,
    };
    let materialization = crate::PlanExecutionAdoptedV1Entry {
        command_id: "r69-materialize-command".to_owned(),
        plan_id: plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        candidate_hash: candidate.candidate_hash.clone(),
        task_id: task_id.clone(),
        task_title: candidate.semantic_title.clone(),
        parent_session_ref: crate::SessionRef::new_relative("parent.jsonl").unwrap(),
        start_mode: crate::PlanTaskStartMode::CreateAndRun,
        permission_grant: None,
        execution_segments: Some(crate::materialize_execution_segments(&candidate)),
        adopted_candidate: Box::new(candidate),
        initial_phase: crate::TaskExecutionPhaseV1::Preparing,
        adopted_at_ms: 10,
    };
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft.clone())),
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(
            PlanDecisionRecordedEntry {
                plan_id: plan_id.clone(),
                plan_hash: draft.plan_hash.clone(),
                decision: PlanDecision::Accepted,
                decided_by: PlanDecisionActor::User,
                decided_at_ms: 10,
                reason: Some("approved before materialization".to_owned()),
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskRun(crate::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: crate::SessionRef::new_relative("parent.jsonl").unwrap(),
            objective: materialization.adopted_candidate.safe_objective.clone(),
            title: Some(materialization.task_title.clone()),
            status: crate::TaskRunStatus::Started,
            reason: Some("stable shell".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(
            TaskCreatedFromPlanEntry {
                plan_id: plan_id.clone(),
                plan_hash: draft.plan_hash.clone(),
                task_id: task_id.clone(),
                task_plan_version: 0,
                step_mapping: Vec::new(),
                stale_reason: Some("Task materialization pending".to_owned()),
                created_at_ms: 10,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskMaterializationAttemptStartedV1(
            crate::TaskMaterializationAttemptStartedV1 {
                task_id: task_id.clone(),
                generation: 1,
                plan_hash: draft.plan_hash.clone(),
                compiler_contract_fingerprint: crate::stable_event_hash(b"compiler-v1"),
                started_at_ms: 20,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskMaterializationBlockedV1(
            crate::TaskMaterializationBlockedV1 {
                task_id: task_id.clone(),
                generation: 1,
                plan_hash: draft.plan_hash.clone(),
                blocker_id: "r69-materialization-blocker-1".to_owned(),
                blocker,
                blocked_at_ms: 20,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskMaterializationAttemptStartedV1(
            crate::TaskMaterializationAttemptStartedV1 {
                task_id: task_id.clone(),
                generation: 2,
                plan_hash: draft.plan_hash.clone(),
                compiler_contract_fingerprint: crate::stable_event_hash(b"compiler-v2"),
                started_at_ms: 30,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskMaterializationPreparedV1(Box::new(
            materialization,
        ))),
    ];

    let artifacts = PlanArtifactProjection::from_entries(&entries);
    assert_eq!(
        artifacts
            .materialization_attempts
            .get(&task_id)
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(artifacts.next_materialization_generation(&task_id), 3);
    assert!(artifacts.materialization_for_task(&task_id).is_some());
    assert!(
        artifacts
            .materialization_blocker_for_task(&task_id)
            .is_none()
    );

    let tasks = crate::TaskStateProjection::from_entries(&entries);
    assert!(tasks.tasks.contains_key(&task_id));
    assert!(tasks.active_blocker(&task_id).is_none());
    let segments = tasks
        .tasks
        .get(&task_id)
        .and_then(|task| task.execution_segments.get(&1))
        .expect("post-approval materialization must carry execution segments");
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].task_id, task_id);
    assert_eq!(
        segments[0].checkpoint_policy,
        crate::SegmentCheckpointPolicyV1::EveryProviderTurn
    );
}

#[test]
fn admission_attempts_are_monotonic_and_drive_phase() {
    let fixture = historical_plan_execution_fixture();
    let draft = fixture.draft;
    let plan_id = draft.plan_id.clone();
    let candidate = *fixture.adoption.adopted_candidate;
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)),
        SessionLogEntry::Control(ControlEntry::PlanExecutionAdoptedV1(Box::new(
            crate::PlanExecutionAdoptedV1Entry {
                command_id: "run-command-2".to_owned(),
                plan_id: plan_id.clone(),
                plan_hash: candidate.plan_hash.clone(),
                candidate_hash: candidate.candidate_hash.clone(),
                task_id: candidate.task_id.clone(),
                task_title: candidate.semantic_title.clone(),
                parent_session_ref: crate::SessionRef::new_relative("parent.jsonl").unwrap(),
                start_mode: crate::PlanTaskStartMode::CreateAndRun,
                permission_grant: None,
                adopted_candidate: Box::new(candidate.clone()),
                execution_segments: None,
                initial_phase: crate::TaskExecutionPhaseV1::Preparing,
                adopted_at_ms: 30,
            },
        ))),
    ];
    let blocker = crate::TaskBlockerV1 {
        reason_code: crate::TaskBlockerReasonCodeV1::WorkspaceChanged,
        summary: "workspace changed".to_owned(),
        affected_step: None,
        affected_capability: None,
        retryable: true,
        available_actions: vec![crate::TaskBlockerActionV1::RetryAdmission],
        evidence_digest: crate::stable_event_hash(b"evidence"),
        created_at_ms: 40,
        resolved_at_ms: None,
    };
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskAdmissionAttemptedV1(crate::TaskAdmissionAttemptV1 {
            task_id: candidate.task_id.clone(),
            plan_version: 1,
            ordinal: 1,
            candidate_hash: candidate.candidate_hash.clone(),
            observed_environment: crate::TaskAdmissionObservationV1 {
                base_workspace_snapshot_id: Some("sha256:snapshot-a".to_owned()),
                current_workspace_snapshot_id: Some("sha256:snapshot-b".to_owned()),
                workspace_state: crate::WorkspaceAdmissionStateV1::ExternalDrift,
                missing_capabilities: Vec::new(),
                provider_route_available: true,
                credential_available: true,
                permission_profile_ok: true,
                disk_space_bytes: None,
                external_writer_active: false,
                verification_runner_available: true,
                observed_at_ms: 40,
            },
            outcome: crate::TaskAdmissionOutcomeV1::Blocked(blocker.clone()),
        }),
    ));
    let tasks = crate::TaskStateProjection::from_entries(&entries);
    assert_eq!(
        tasks.execution_phase(&candidate.task_id),
        Some(crate::TaskExecutionPhaseV1::Blocked)
    );
    assert_eq!(tasks.next_admission_ordinal(&candidate.task_id), 2);
    assert_eq!(
        tasks
            .active_blocker(&candidate.task_id)
            .unwrap()
            .reason_code,
        crate::TaskBlockerReasonCodeV1::WorkspaceChanged
    );
    // A second attempt resolves the blocker and moves the phase to Ready.
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskAdmissionAttemptedV1(crate::TaskAdmissionAttemptV1 {
            task_id: candidate.task_id.clone(),
            plan_version: 1,
            ordinal: 2,
            candidate_hash: candidate.candidate_hash.clone(),
            observed_environment: crate::TaskAdmissionObservationV1 {
                base_workspace_snapshot_id: Some("sha256:snapshot-a".to_owned()),
                current_workspace_snapshot_id: Some("sha256:snapshot-a".to_owned()),
                workspace_state: crate::WorkspaceAdmissionStateV1::ExactMatch,
                missing_capabilities: Vec::new(),
                provider_route_available: true,
                credential_available: true,
                permission_profile_ok: true,
                disk_space_bytes: None,
                external_writer_active: false,
                verification_runner_available: true,
                observed_at_ms: 50,
            },
            outcome: crate::TaskAdmissionOutcomeV1::Ready(crate::TaskRuntimeLeaseBindingV1 {
                lease_id: "lease-1".to_owned(),
                granted_at_ms: 50,
            }),
        }),
    ));
    let tasks = crate::TaskStateProjection::from_entries(&entries);
    assert_eq!(
        tasks.execution_phase(&candidate.task_id),
        Some(crate::TaskExecutionPhaseV1::Ready)
    );
    assert!(tasks.active_blocker(&candidate.task_id).is_none());
    // CreatePaused adoption stays Paused and never invents blockers.
    let paused = crate::TaskStateProjection::from_entries(&entries);
    assert_eq!(paused.admission_attempts.len(), 1);
}
