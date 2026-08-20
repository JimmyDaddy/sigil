use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, NetworkEffect, PlanApprovalPermission, PlanArtifactProjection, PlanDecision,
    PlanDecisionActor, PlanDecisionRecordedEntry, PlanId, PlanSourceRef, SessionLogEntry,
    TaskCreatedFromPlanEntry, TaskId, TaskIsolationMode, TaskStepMode, ToolAccess, ToolCategory,
    ToolPreviewCapability, ToolSpec, plan_draft_created_entry, plan_review_detail_from_entries,
    plan_task_input_from_draft, plan_text_hash, plan_workspace_paths, submit_plan_draft_entry,
    task_id_from_plan_draft, task_plan_from_plan_draft,
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

    assert!(task_input.contains("Execute the following user-approved structured plan"));
    assert!(task_input.contains("authoritative task input"));
    assert!(task_input.contains("Preserve the approved plan's scope and order"));
    assert!(task_input.contains("Approved structured plan:"));
    assert!(task_input.contains("This docs has typoo"));
    assert_eq!(draft.target_paths, vec!["README.md"]);
    Ok(())
}

#[test]
fn sigil_plan_v2_promotes_directly_to_the_shared_task_dag() -> Result<()> {
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

    let promotion = task_plan_from_plan_draft(&draft, TaskId::new("task_1")?, 1)?
        .expect("v2 plan should promote directly");
    let task_plan = promotion.task_plan;
    let mapping = promotion.step_mapping;
    assert_eq!(task_plan.steps.len(), 2);
    assert_eq!(mapping.len(), 2);
    assert_eq!(task_plan.steps[1].depends_on[0].as_str(), "inspect");
    assert_eq!(task_plan.steps[0].effective_mode(), TaskStepMode::Read);
    assert_eq!(
        task_plan.steps[0].effective_isolation(),
        TaskIsolationMode::SharedReadOnly
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
fn direct_plan_promotion_rejects_legacy_verify_participant_steps() -> Result<()> {
    let mut draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Inspect Cargo",
  "steps": [{
    "step_id": "inspect",
    "title": "Inspect Cargo",
    "role": "executor",
    "depends_on": [],
    "mode": "read",
    "isolation": "shared_read_only",
    "target_paths": ["Cargo.toml"]
  }],
  "target_paths": ["Cargo.toml"]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("read plan should create a draft");
    draft.steps[0].mode = Some(TaskStepMode::Verify);

    let error = task_plan_from_plan_draft(&draft, TaskId::new("task_legacy_verify")?, 1)
        .expect_err("legacy verify steps must not become impossible participants");
    assert!(
        error
            .to_string()
            .contains("cannot promote verify participant steps")
    );
    Ok(())
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
    let promotion = task_plan_from_plan_draft(&draft, TaskId::new("task_1")?, 1)?
        .expect("v2 plan should promote directly");
    assert!(promotion.task_plan.steps[0].intent_refs.is_empty());
    assert_eq!(
        promotion.intent_alias_bindings[0].intent_aliases,
        vec!["retry"]
    );
    Ok(())
}

#[test]
fn sigil_plan_v2_rejects_intent_aliases_without_matching_proposal() {
    let error = plan_draft_created_entry(
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
    )
    .expect_err("unknown provider alias must fail closed");

    assert!(
        error
            .to_string()
            .contains("require a top-level intent proposal")
    );
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
    let draft = submit_plan_draft_entry(
        &serde_json::to_string(&args)?,
        plan_id.clone(),
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
fn plan_review_rejects_oversized_summary_instead_of_truncating_detail() -> Result<()> {
    let args = json!({
        "schema_version": 2,
        "summary": "x".repeat(2 * 1024 + 1),
        "steps": [{"step_id": "inspect", "title": "Inspect"}],
        "target_paths": [],
        "suggested_checks": []
    });

    let error = submit_plan_draft_entry(
        &serde_json::to_string(&args)?,
        PlanId::new("plan-summary-too-large")?,
        PlanSourceRef::default(),
        42,
        None,
    )
    .expect_err("oversized summaries must fail closed");

    assert!(error.to_string().contains("2048-byte"));
    Ok(())
}

#[test]
fn plan_display_name_is_bounded_before_draft_commit_and_during_legacy_promotion() -> Result<()> {
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
    let mut draft = submit_plan_draft_entry(
        &serde_json::to_string(&args)?,
        PlanId::new("bounded-display-name")?,
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
    let promotion = task_plan_from_plan_draft(&draft, TaskId::new("task_bounded")?, 1)?
        .expect("bounded draft promotes");
    assert_eq!(
        promotion.task_plan.steps[0].display_name.as_deref(),
        Some(committed)
    );

    // A previously persisted V2 draft may still carry the old unbounded representation. It must
    // remain executable after an upgrade because this field is presentation-only.
    draft.steps[0].display_name = Some(oversized.to_owned());
    let legacy = task_plan_from_plan_draft(&draft, TaskId::new("task_legacy")?, 1)?
        .expect("legacy draft promotes");
    let legacy_name = legacy.task_plan.steps[0]
        .display_name
        .as_deref()
        .expect("legacy display name");
    assert_eq!(
        legacy_name.chars().count(),
        crate::TASK_AGENT_DISPLAY_NAME_MAX_CHARS
    );
    assert!(legacy_name.ends_with('…'));
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
