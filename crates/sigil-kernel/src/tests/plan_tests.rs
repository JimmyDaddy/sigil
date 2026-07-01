use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalProjection,
    PlanApprovalScope, PlanApprovedEntry, PlanArtifactProjection, PlanDecision, PlanDecisionActor,
    PlanDecisionRecordedEntry, PlanSourceRef, Session, SessionLogEntry, TaskCreatedFromPlanEntry,
    TaskId, ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec, plan_draft_created_entry,
    plan_task_input_from_draft, plan_text_hash, plan_workspace_paths,
};

fn tool_spec(
    name: &str,
    category: ToolCategory,
    access: ToolAccess,
    preview: ToolPreviewCapability,
) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: "test tool".to_owned(),
        input_schema: json!({"type": "object"}),
        category,
        access,
        preview,
    }
}

fn approved_entry(plan_text: &str, plan_version: u32) -> PlanApprovedEntry {
    PlanApprovedEntry {
        plan_version,
        plan_hash: plan_text_hash(plan_text),
        approved_at_ms: 42,
        permission: PlanApprovalPermission::WorkspaceEdits,
        scope: PlanApprovalScope {
            summary: "edit workspace files described by the plan".to_owned(),
            workspace_paths: vec!["crates/sigil-tui".to_owned()],
        },
        expires: PlanApprovalExpiry::NextUserPrompt,
        clear_planning_context: true,
    }
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

    let draft = plan_draft_created_entry(
        "1. Inspect README.md\n2. Update crates/sigil-tui/src/app.rs\n3. Run cargo test -p sigil-kernel plan",
        PlanSourceRef {
            session_ref: Some("session.jsonl".to_owned()),
            run_id: Some("run_1".to_owned()),
            final_message_id: Some("msg_1".to_owned()),
        },
        42,
        Some("snapshot_1".to_owned()),
    )?
    .expect("non-empty plan should create a durable draft");

    assert!(draft.plan_id.as_str().starts_with("plan_"));
    assert!(draft.plan_hash.starts_with("sha256:"));
    assert_eq!(draft.summary, "Inspect README.md");
    assert_eq!(
        draft
            .inline_text
            .as_deref()
            .unwrap_or_default()
            .lines()
            .count(),
        3
    );
    assert!(draft.target_paths.iter().any(|path| path == "README.md"));
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
        r#"# 计划

文件: README.md
问题: 第 3 行 "This docs has typoo." 中 "typoo" 拼写错误。
修复: 将 typoo 改为 typo。

```diff
- This docs has typoo.
+ This docs has typo.
```

是否需要我执行这个修改？
"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("non-empty plan should create a durable draft");
    let task_input = plan_task_input_from_draft(&draft);

    assert!(task_input.contains("Execute the following user-approved plan"));
    assert!(task_input.contains("authoritative task input"));
    assert!(task_input.contains("Preserve the approved plan's scope and order"));
    assert!(task_input.contains("Approved plan:"));
    assert!(task_input.contains("This docs has typoo"));
    assert_eq!(draft.target_paths, vec!["README.md"]);
    Ok(())
}

#[test]
fn sigil_plan_v1_block_is_treated_as_plain_plan_text() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"计划如下。

```sigil-plan-v1
{
  "summary": "Fix README typo",
  "steps": [
    {
      "id": "fix-readme-typo",
      "title": "Fix README.md line 3 typo",
      "mode": "write",
      "target_paths": ["README.md"],
      "acceptance": ["README.md line 3 no longer contains typoo"]
    },
    {
      "id": "verify-readme",
      "title": "Verify README.md wording",
      "mode": "verify",
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

    assert_eq!(draft.summary, "计划如下。");
    assert!(draft.target_paths.iter().any(|path| path == "README.md"));
    assert!(task_input.contains("sigil-plan-v1"));
    assert!(task_input.contains("fix-readme-typo"));
    Ok(())
}

#[test]
fn plan_artifact_projection_tracks_pending_decision_and_created_task() -> Result<()> {
    let draft = plan_draft_created_entry(
        "1. Inspect README.md\n2. Update README.md",
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
fn approved_plan_input_is_stable_and_does_not_materialize_task_plan() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"
Plan:

```sigil-plan-v1
{
  "summary": "Update quickstart docs",
  "steps": [
    {
      "id": "inspect-readme",
      "title": "Inspect README.md",
      "mode": "read_only",
      "target_paths": ["README.md"]
    },
    {
      "id": "update-quickstart",
      "title": "Update docs/en/quickstart.md copy",
      "mode": "write",
      "target_paths": ["docs/en/quickstart.md"]
    },
    {
      "id": "verify-plan-tests",
      "title": "Verify with cargo test -p sigil-kernel plan",
      "mode": "verify"
    }
  ]
}
```
"#,
        PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("draft");
    let left = plan_task_input_from_draft(&draft);
    let right = plan_task_input_from_draft(&draft);

    assert_eq!(left, right);
    assert!(left.contains("Update docs/en/quickstart.md copy"));
    assert!(left.contains("task execution plan"));
    assert!(left.contains("include a concise reason in the task step detail"));
    Ok(())
}

#[test]
fn plan_approved_control_entry_roundtrips() -> Result<()> {
    let entry = approved_entry("inspect then edit", 3);
    let session_entry = SessionLogEntry::Control(ControlEntry::PlanApproved(entry.clone()));

    let encoded = serde_json::to_string(&session_entry)?;
    let decoded: SessionLogEntry = serde_json::from_str(&encoded)?;

    assert!(encoded.contains("plan_approved"));
    assert!(encoded.contains("workspace_edits"));
    assert!(matches!(
        decoded,
        SessionLogEntry::Control(ControlEntry::PlanApproved(restored)) if restored == entry
    ));
    Ok(())
}

#[test]
fn plan_approval_projection_tracks_latest_and_latest_by_hash() -> Result<()> {
    let first = approved_entry("first plan", 1);
    let mut second = approved_entry("second plan", 2);
    second.permission = PlanApprovalPermission::Ask;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanApproved(first.clone())),
        SessionLogEntry::Control(ControlEntry::PlanApproved(second.clone())),
    ];

    let projection = PlanApprovalProjection::from_entries(&entries);

    assert_eq!(projection.approvals, vec![first.clone(), second.clone()]);
    assert_eq!(projection.latest_approval, Some(second));
    assert_eq!(
        projection.latest_by_hash.get(&first.plan_hash),
        Some(&first)
    );

    let session = Session::from_entries("mock", "model", entries);
    assert_eq!(session.plan_approval_projection().latest_by_hash.len(), 2);
    Ok(())
}

#[test]
fn workspace_edits_plan_permission_does_not_cover_shell_network_mcp_or_agent() {
    let permission = PlanApprovalPermission::WorkspaceEdits;

    assert!(permission.covers_tool(&tool_spec(
        "edit_file",
        ToolCategory::File,
        ToolAccess::Write,
        ToolPreviewCapability::Required
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "write_file_without_preview",
        ToolCategory::File,
        ToolAccess::Write,
        ToolPreviewCapability::None
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "bash",
        ToolCategory::Shell,
        ToolAccess::Execute,
        ToolPreviewCapability::None
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "web_fetch",
        ToolCategory::Custom,
        ToolAccess::Network,
        ToolPreviewCapability::None
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "mcp__filesystem__read",
        ToolCategory::Mcp,
        ToolAccess::Write,
        ToolPreviewCapability::Optional
    )));
    assert!(!permission.covers_tool(&tool_spec(
        "spawn_agent",
        ToolCategory::Agent,
        ToolAccess::Execute,
        ToolPreviewCapability::Required
    )));
    assert!(!PlanApprovalPermission::Ask.covers_tool(&tool_spec(
        "edit_file",
        ToolCategory::File,
        ToolAccess::Write,
        ToolPreviewCapability::Required
    )));
}
