use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, NetworkEffect, PlanApprovalExpiry, PlanApprovalPermission,
    PlanApprovalProjection, PlanApprovalScope, PlanApprovedEntry, PlanArtifactProjection,
    PlanDecision, PlanDecisionActor, PlanDecisionRecordedEntry, PlanSourceRef, Session,
    SessionLogEntry, TaskCreatedFromPlanEntry, TaskId, TaskIsolationMode, TaskStepMode, ToolAccess,
    ToolCategory, ToolPreviewCapability, ToolSpec, plan_draft_created_entry,
    plan_task_input_from_draft, plan_text_hash, plan_workspace_paths, task_id_from_plan_draft,
    task_plan_from_plan_draft,
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

fn simple_structured_plan(summary: &str, title: &str, path: &str) -> String {
    format!(
        r#"Plan:

```sigil-plan-v1
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

```sigil-plan-v1
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
      "id": "inspect",
      "title": "Inspect README",
      "role": "executor",
      "depends_on": [],
      "mode": "read",
      "isolation": "shared_read_only",
      "target_paths": ["README.md"]
    },
    {
      "id": "report",
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

    let (task_plan, mapping) = task_plan_from_plan_draft(&draft, TaskId::new("task_1")?, 1)?
        .expect("v2 plan should promote directly");
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
fn sigil_plan_v1_never_direct_promotes_even_with_v2_fields() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v1
{
  "summary": "Legacy fully specified plan",
  "steps": [{
    "id": "inspect",
    "title": "Inspect README",
    "role": "executor",
    "depends_on": [],
    "mode": "read",
    "isolation": "shared_read_only"
  }]
}
```"#,
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("v1 plan should remain a durable compatibility draft");

    assert_eq!(draft.schema_version, 1);
    assert!(task_plan_from_plan_draft(&draft, TaskId::new("task_1")?, 1)?.is_none());
    Ok(())
}

#[test]
fn sigil_plan_v1_accepts_single_string_notes_and_acceptance() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v1
{
  "summary": "Fix README typo",
  "steps": [
    {
      "id": "fix-readme-typo",
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
    assert_eq!(
        draft.steps[0].notes,
        vec![
            "One token replacement.",
            "acceptance: README.md contains the corrected marker.",
        ]
    );
    Ok(())
}

#[test]
fn sigil_plan_v1_block_creates_structured_executable_plan() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"计划如下。

```sigil-plan-v1
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

    assert_eq!(draft.summary, "Fix README typo");
    assert_eq!(draft.target_paths, vec!["README.md"]);
    assert_eq!(draft.steps.len(), 2);
    assert!(task_input.contains("Fix README.md line 3 typo"));
    assert!(!task_input.contains("sigil-plan-v1"));
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
fn legacy_approved_plan_input_is_stable_and_requires_planner_fallback() -> Result<()> {
    let draft = plan_draft_created_entry(
        r#"
Plan:

```sigil-plan-v1
{
  "summary": "Update quickstart docs",
  "steps": [
    {
      "step_id": "inspect-readme",
      "title": "Inspect README.md",
      "mode": "read_only",
      "target_paths": ["README.md"]
    },
    {
      "step_id": "update-quickstart",
      "title": "Update docs/en/quickstart.md copy",
      "mode": "write",
      "target_paths": ["docs/en/quickstart.md"]
    },
    {
      "step_id": "verify-plan-tests",
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
    assert!(left.contains("authoritative task input"));
    assert!(!left.contains("sigil-plan-v1"));
    assert!(task_plan_from_plan_draft(&draft, TaskId::new("task_1")?, 1)?.is_none());
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
