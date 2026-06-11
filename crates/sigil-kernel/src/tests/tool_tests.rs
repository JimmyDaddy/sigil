use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use crate::{
    ApprovalMode, Tool, ToolAccess, ToolCategory, ToolContext, ToolDiffBudget, ToolDiffStats,
    ToolEgressAudit, ToolPreview, ToolPreviewCapability, ToolPreviewFile, ToolPreviewSnapshot,
    ToolRegistry, ToolResult, ToolResultMeta, ToolSpec, provider::ToolCall,
};

#[test]
fn tool_diff_stats_ignore_file_headers() {
    let stats = ToolDiffStats::from_unified_diff(
        "--- a/file.txt\n+++ b/file.txt\n@@ -1,2 +1,3 @@\n old\n-removed\n+added\n+another",
    );

    assert_eq!(stats.added, 2);
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.hunks, 1);
}

#[test]
fn preview_snapshot_builder_truncates_by_file_and_line_budget() {
    let preview = ToolPreview {
        title: "Write file".to_owned(),
        summary: "Update two files".to_owned(),
        body: "preview body".to_owned(),
        changed_files: vec!["a.txt".to_owned(), "b.txt".to_owned()],
        file_diffs: vec![
            ToolPreviewFile {
                path: "a.txt".to_owned(),
                diff: "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-old\n+new\n+extra".to_owned(),
            },
            ToolPreviewFile {
                path: "b.txt".to_owned(),
                diff: "--- a/b.txt\n+++ b/b.txt\n@@ -0,0 +1 @@\n+created".to_owned(),
            },
        ],
    };

    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &preview,
        ToolDiffBudget {
            max_files: 1,
            max_lines_total: 5,
            max_lines_per_file: 5,
            max_bytes_total: 1024,
            max_bytes_per_file: 1024,
        },
        Some("preview-hash".to_owned()),
    );

    assert_eq!(snapshot.call_id, "call-1");
    assert_eq!(snapshot.tool_name, "write_file");
    assert_eq!(
        snapshot.original_preview_hash.as_deref(),
        Some("preview-hash")
    );
    assert!(snapshot.truncated);
    assert_eq!(snapshot.file_diffs.len(), 1);
    assert_eq!(snapshot.file_diffs[0].path, "a.txt");
    assert_eq!(snapshot.file_diffs[0].rendered_line_count, 5);
    assert!(snapshot.file_diffs[0].truncated);
    assert_eq!(snapshot.original_stats.added, 3);
    assert_eq!(snapshot.original_stats.removed, 1);
    assert_eq!(snapshot.original_stats.hunks, 2);
    assert_eq!(snapshot.rendered_stats.added, 1);
    assert_eq!(snapshot.rendered_stats.removed, 1);
    assert_eq!(snapshot.rendered_stats.hunks, 1);
}

#[test]
fn preview_snapshot_builder_truncates_by_byte_budget() {
    let preview = ToolPreview {
        title: "Write file".to_owned(),
        summary: "Update file".to_owned(),
        body: "preview body".to_owned(),
        changed_files: vec!["a.txt".to_owned()],
        file_diffs: vec![ToolPreviewFile {
            path: "a.txt".to_owned(),
            diff: "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new".to_owned(),
        }],
    };

    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &preview,
        ToolDiffBudget {
            max_files: 1,
            max_lines_total: 160,
            max_lines_per_file: 160,
            max_bytes_total: 20,
            max_bytes_per_file: 20,
        },
        None,
    );

    assert!(snapshot.truncated);
    assert!(snapshot.file_diffs[0].truncated);
    assert!(snapshot.rendered_byte_count <= 20);
    assert!(snapshot.rendered_line_count < snapshot.original_line_count);
}

struct RegistryFixtureTool;

#[async_trait]
impl Tool for RegistryFixtureTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fixture".to_owned(),
            description: "fixture".to_owned(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            category: ToolCategory::Custom,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::Optional,
        }
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Ask))
    }

    fn egress_audit(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ToolEgressAudit>> {
        Ok(Some(ToolEgressAudit {
            destination: "test:fixture".to_owned(),
            operation: "run".to_owned(),
            payload: json!({"shape":"minimal"}),
            redacted: true,
        }))
    }

    async fn preview(
        &self,
        _ctx: ToolContext,
        _args: serde_json::Value,
    ) -> Result<Option<ToolPreview>> {
        Ok(Some(ToolPreview {
            title: "Fixture preview".to_owned(),
            summary: "Preview".to_owned(),
            body: "Body".to_owned(),
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        }))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "fixture",
            "executed",
            ToolResultMeta::default(),
        ))
    }
}

#[tokio::test]
async fn tool_registry_executes_registered_tool_and_exposes_hooks() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RegistryFixtureTool));
    let ctx = ToolContext {
        workspace_root: std::env::temp_dir(),
        timeout_secs: 5,
    };
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "fixture".to_owned(),
        args_json: r#"{"path":"file.txt"}"#.to_owned(),
    };

    let result = registry.execute(ctx.clone(), call.clone()).await?;
    let preview = registry.preview(ctx.clone(), call.clone()).await?;
    let access = registry.permission_access(&ctx, &call)?;
    let default_mode = registry.permission_default_mode(&ctx, &call)?;
    let egress = registry.egress_audit(&ctx, &call)?;

    assert_eq!(result.content, "executed");
    assert_eq!(
        preview.expect("preview should exist").title,
        "Fixture preview"
    );
    assert_eq!(access, ToolAccess::Execute);
    assert_eq!(default_mode, Some(ApprovalMode::Ask));
    assert!(matches!(
        egress,
        Some(ToolEgressAudit { redacted: true, .. })
    ));
    Ok(())
}

#[tokio::test]
async fn tool_registry_surfaces_unknown_tool_and_invalid_json() {
    let registry = ToolRegistry::new();
    let ctx = ToolContext {
        workspace_root: std::env::temp_dir(),
        timeout_secs: 5,
    };

    let unknown_error = registry
        .execute(
            ctx.clone(),
            ToolCall {
                id: "call-1".to_owned(),
                name: "missing".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await
        .expect_err("unknown tool should fail");
    assert!(unknown_error.to_string().contains("unknown tool missing"));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RegistryFixtureTool));
    let invalid_json_error = registry
        .preview(
            ctx,
            ToolCall {
                id: "call-2".to_owned(),
                name: "fixture".to_owned(),
                args_json: "{not json}".to_owned(),
            },
        )
        .await
        .expect_err("invalid json should fail");
    assert!(
        invalid_json_error
            .to_string()
            .contains("invalid tool args for fixture")
    );
}

#[test]
fn tool_result_summary_and_model_content_include_structured_error_details() {
    let result = ToolResult::error(
        "call-9",
        "fixture",
        crate::ToolErrorKind::Protocol,
        "protocol mismatch",
    )
    .with_error_details(true, json!({"stage":"decode"}));

    let content = result.to_model_content();
    let summary = result.summary();

    assert!(content.contains(r#""kind":"protocol""#));
    assert!(content.contains(r#""retryable":true"#));
    assert!(content.contains(r#""stage":"decode""#));
    assert!(summary.is_error);
    assert_eq!(summary.error_kind, Some(crate::ToolErrorKind::Protocol));
    assert_eq!(summary.error_message.as_deref(), Some("protocol mismatch"));
}
