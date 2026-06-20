use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalProjection,
    PlanApprovalScope, PlanApprovedEntry, Session, SessionLogEntry, ToolAccess, ToolCategory,
    ToolPreviewCapability, ToolSpec, plan_text_hash, plan_workspace_paths,
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
