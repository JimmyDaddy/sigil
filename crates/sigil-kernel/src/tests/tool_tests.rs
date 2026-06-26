use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use crate::{
    ApprovalMode, MessageRole, Tool, ToolAccess, ToolCategory, ToolContext, ToolDiffBudget,
    ToolDiffStats, ToolEgressAudit, ToolErrorKind, ToolPreview, ToolPreviewCapability,
    ToolPreviewFile, ToolPreviewSnapshot, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubjectKind, ToolSubjectScope, provider::ToolCall,
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

struct NamedRegistryTool(&'static str);

#[async_trait]
impl Tool for NamedRegistryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.to_owned(),
            description: "named fixture".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.0,
            "ok",
            ToolResultMeta::default(),
        ))
    }
}

#[tokio::test]
async fn tool_registry_executes_registered_tool_and_exposes_hooks() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RegistryFixtureTool));
    let ctx = ToolContext::new(std::env::temp_dir(), 5);
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "fixture".to_owned(),
        args_json: r#"{"path":"file.txt"}"#.to_owned(),
    };

    let result = registry.execute(ctx.clone(), call.clone()).await?;
    let preview = registry.preview(ctx.clone(), call.clone()).await?;
    let access = registry.permission_access(&ctx, &call)?;
    let operation = registry.permission_operation(&ctx, &call)?;
    let default_mode = registry.permission_default_mode(&ctx, &call)?;
    let egress = registry.egress_audit(&ctx, &call)?;

    assert_eq!(result.content, "executed");
    assert_eq!(
        preview.expect("preview should exist").title,
        "Fixture preview"
    );
    assert_eq!(access, ToolAccess::Execute);
    assert_eq!(operation, crate::ToolOperation::ExecuteUnknownCommand);
    assert_eq!(default_mode, Some(ApprovalMode::Ask));
    assert!(matches!(
        egress,
        Some(ToolEgressAudit { redacted: true, .. })
    ));
    Ok(())
}

#[test]
fn tool_registry_unregisters_tools_by_name_prefix() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedRegistryTool("mcp__docs__resources_list")));
    registry.register(Arc::new(NamedRegistryTool("mcp__docs__read_file")));
    registry.register(Arc::new(NamedRegistryTool("read_file")));

    let removed = registry.unregister_by_name_prefix("mcp__docs__");

    assert_eq!(removed, 2);
    assert!(registry.spec_for("mcp__docs__resources_list").is_none());
    assert!(registry.spec_for("mcp__docs__read_file").is_none());
    assert!(registry.spec_for("read_file").is_some());
}

#[test]
fn tool_registry_scope_empty_and_into_registry_are_stable() {
    assert!(ToolRegistryScope::default().is_empty());
    assert!(
        !ToolRegistryScope::from_names_and_prefixes(["read_file"], std::iter::empty::<&str>())
            .is_empty()
    );

    let allow_all = ToolRegistryScope {
        allow_all: true,
        ..ToolRegistryScope::default()
    };
    assert!(!allow_all.is_empty());

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedRegistryTool("read_file")));
    let scoped = registry.scoped(ToolRegistryScope::from_names_and_prefixes(
        ["read_file"],
        std::iter::empty::<&str>(),
    ));
    let unwrapped = scoped.into_registry();

    assert!(unwrapped.spec_for("read_file").is_some());
}

#[test]
fn tool_registry_scope_intersection_keeps_only_shared_names_and_prefixes() {
    let role = ToolRegistryScope::from_names_and_prefixes(["read_file", "write_file"], ["mcp__"]);
    let skill = ToolRegistryScope::from_names_and_prefixes(["write_file", "bash"], ["mcp__docs__"]);

    let effective = role.intersection(&skill);

    assert!(!effective.allow_all);
    assert!(effective.names.contains("write_file"));
    assert!(!effective.names.contains("read_file"));
    assert!(!effective.names.contains("bash"));
    assert_eq!(effective.prefixes, vec!["mcp__docs__"]);
}

#[test]
fn tool_registry_scope_intersection_handles_empty_allow_all_and_nested_prefixes() {
    let empty = ToolRegistryScope::default();
    let read_only =
        ToolRegistryScope::from_names_and_prefixes(["read_file"], std::iter::empty::<&str>());
    assert!(empty.intersection(&read_only).is_empty());
    assert!(read_only.intersection(&empty).is_empty());

    let allow_all = ToolRegistryScope {
        allow_all: true,
        ..ToolRegistryScope::default()
    };
    assert_eq!(allow_all.intersection(&read_only), read_only);
    assert_eq!(read_only.intersection(&allow_all), read_only);

    let broad =
        ToolRegistryScope::from_names_and_prefixes(std::iter::empty::<&str>(), ["mcp__docs__"]);
    let narrow = ToolRegistryScope::from_names_and_prefixes(
        std::iter::empty::<&str>(),
        ["mcp__docs__search__"],
    );
    assert_eq!(
        broad.intersection(&narrow).prefixes,
        vec!["mcp__docs__search__"]
    );
}

#[test]
fn tool_registry_scope_union_merges_names_prefixes_and_allow_all() {
    let left = ToolRegistryScope::from_names_and_prefixes(["read_file"], ["mcp__docs__"]);
    let right = ToolRegistryScope::from_names_and_prefixes(["write_file"], ["mcp__git__"]);

    let merged = left.union(&right);

    assert!(!merged.allow_all);
    assert!(merged.names.contains("read_file"));
    assert!(merged.names.contains("write_file"));
    assert_eq!(merged.prefixes, vec!["mcp__docs__", "mcp__git__"]);

    let allow_all = ToolRegistryScope {
        allow_all: true,
        ..ToolRegistryScope::default()
    };
    assert!(merged.union(&allow_all).allow_all);
}

#[test]
fn tool_registry_drains_by_name_prefix_after_lock_poisoning() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedRegistryTool("mcp__poisoned__echo")));
    registry.register(Arc::new(NamedRegistryTool("read_file")));
    let tools = Arc::clone(&registry.tools);

    let _ = std::thread::spawn(move || {
        let _guard = tools
            .write()
            .expect("registry write lock should be acquired");
        panic!("poison registry lock for recovery coverage");
    })
    .join();

    let drained = registry.drain_by_name_prefix("mcp__poisoned__");
    let operation = registry
        .permission_operation(
            &ToolContext::new(std::env::temp_dir(), 5),
            &ToolCall {
                id: "call-read".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .expect("poisoned registry lock should recover for permission operation");

    assert_eq!(drained.len(), 1);
    assert_eq!(operation, crate::ToolOperation::Read);
    assert!(registry.spec_for("mcp__poisoned__echo").is_none());
    assert!(registry.spec_for("read_file").is_some());
}

#[tokio::test]
async fn scoped_tool_registry_denies_matching_names_after_allow_scope() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedRegistryTool("read_file")));
    registry.register(Arc::new(NamedRegistryTool("write_file")));
    let scoped = registry.scoped_with_denies(
        ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        ToolRegistryScope::from_names_and_prefixes(["write_file"], std::iter::empty::<&str>()),
    );

    assert!(scoped.spec_for("read_file").is_some());
    assert!(scoped.spec_for("write_file").is_none());
    assert!(
        scoped
            .execute(
                ToolContext::new(std::env::temp_dir(), 5),
                ToolCall {
                    id: "call-1".to_owned(),
                    name: "write_file".to_owned(),
                    args_json: "{}".to_owned(),
                },
            )
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn scoped_tool_registry_gates_all_tool_paths() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RegistryFixtureTool));
    registry.register(Arc::new(NamedRegistryTool("read_file")));
    registry.register(Arc::new(NamedRegistryTool("mcp__docs__lookup")));
    let scoped = registry.scoped(ToolRegistryScope::from_names_and_prefixes(
        ["read_file"],
        ["mcp__docs__"],
    ));
    let ctx = ToolContext::new(std::env::temp_dir(), 5);
    let blocked_call = ToolCall {
        id: "call-1".to_owned(),
        name: "fixture".to_owned(),
        args_json: r#"{"path":"file.txt"}"#.to_owned(),
    };

    let visible_names = scoped
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert_eq!(
        visible_names,
        vec!["mcp__docs__lookup".to_owned(), "read_file".to_owned()]
    );
    assert!(scoped.spec_for("fixture").is_none());
    assert!(
        scoped
            .execute(ctx.clone(), blocked_call.clone())
            .await
            .is_err()
    );
    assert!(
        scoped
            .preview(ctx.clone(), blocked_call.clone())
            .await
            .is_err()
    );
    assert!(scoped.permission_access(&ctx, &blocked_call).is_err());
    assert!(scoped.permission_operation(&ctx, &blocked_call).is_err());
    assert!(scoped.permission_subjects(&ctx, &blocked_call).is_err());
    assert!(scoped.permission_default_mode(&ctx, &blocked_call).is_err());
    assert!(scoped.egress_audit(&ctx, &blocked_call).is_err());
    Ok(())
}

#[tokio::test]
async fn scoped_tool_registry_prefix_allows_lazily_registered_tools() -> Result<()> {
    let mut registry = ToolRegistry::new();
    let scoped = registry.scoped(ToolRegistryScope::from_names_and_prefixes(
        std::iter::empty::<&str>(),
        ["mcp__lazy__"],
    ));
    registry.register(Arc::new(NamedRegistryTool("mcp__lazy__read")));
    registry.register(Arc::new(NamedRegistryTool("mcp__other__read")));

    assert!(scoped.spec_for("mcp__lazy__read").is_some());
    assert!(scoped.spec_for("mcp__other__read").is_none());
    let result = scoped
        .execute(
            ToolContext::new(std::env::temp_dir(), 5),
            ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__lazy__read".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;

    assert_eq!(result.content, "ok");
    Ok(())
}

#[tokio::test]
async fn tool_registry_surfaces_unknown_tool_and_invalid_json() {
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(std::env::temp_dir(), 5);

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

#[test]
fn tool_result_model_content_omits_empty_false_metadata_values() {
    let result = ToolResult::ok(
        "call-1",
        "fixture",
        "ok",
        ToolResultMeta {
            details: json!(false),
            ..ToolResultMeta::default()
        },
    );

    let content = result.to_model_content();

    assert!(!content.contains("meta"));
}

struct DefaultHookTool;

#[async_trait]
impl Tool for DefaultHookTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "default_hooks".to_owned(),
            description: "default hooks".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::Optional,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            "ok",
            ToolResultMeta::default(),
        ))
    }
}

#[test]
fn tool_labels_are_stable() {
    assert_eq!(ToolCategory::File.as_str(), "file");
    assert_eq!(ToolCategory::Search.as_str(), "search");
    assert_eq!(ToolCategory::Shell.as_str(), "shell");
    assert_eq!(ToolCategory::Mcp.as_str(), "mcp");
    assert_eq!(ToolCategory::Agent.as_str(), "agent");
    assert_eq!(ToolCategory::Custom.as_str(), "custom");

    assert_eq!(ToolAccess::Read.as_str(), "read");
    assert_eq!(ToolAccess::Write.as_str(), "write");
    assert_eq!(ToolAccess::Execute.as_str(), "execute");
    assert_eq!(ToolAccess::Network.as_str(), "network");

    assert_eq!(ToolSubjectKind::Path.as_str(), "path");
    assert_eq!(ToolSubjectKind::Command.as_str(), "command");
    assert_eq!(
        ToolSubjectKind::NetworkEndpoint.as_str(),
        "network_endpoint"
    );
    assert_eq!(ToolSubjectKind::McpTool.as_str(), "mcp_tool");
    assert_eq!(ToolSubjectKind::McpTrustClass.as_str(), "mcp_trust_class");
    assert_eq!(ToolSubjectKind::Other.as_str(), "other");

    assert_eq!(ToolSubjectScope::Workspace.as_str(), "workspace");
    assert_eq!(ToolSubjectScope::External.as_str(), "external");
    assert_eq!(ToolSubjectScope::Unknown.as_str(), "unknown");

    assert_eq!(ToolErrorKind::InvalidInput.as_str(), "invalid_input");
    assert_eq!(
        ToolErrorKind::PermissionDenied.as_str(),
        "permission_denied"
    );
    assert_eq!(
        ToolErrorKind::ApprovalRequired.as_str(),
        "approval_required"
    );
    assert_eq!(ToolErrorKind::ApprovalDenied.as_str(), "approval_denied");
    assert_eq!(
        ToolErrorKind::PathOutsideWorkspace.as_str(),
        "path_outside_workspace"
    );
    assert_eq!(
        ToolErrorKind::ExternalDirectoryRequired.as_str(),
        "external_directory_required"
    );
    assert_eq!(ToolErrorKind::NotFound.as_str(), "not_found");
    assert_eq!(ToolErrorKind::Timeout.as_str(), "timeout");
    assert_eq!(ToolErrorKind::Interrupted.as_str(), "interrupted");
    assert_eq!(ToolErrorKind::ExitStatus.as_str(), "exit_status");
    assert_eq!(ToolErrorKind::Io.as_str(), "io");
    assert_eq!(ToolErrorKind::Utf8.as_str(), "utf8");
    assert_eq!(ToolErrorKind::Network.as_str(), "network");
    assert_eq!(ToolErrorKind::Protocol.as_str(), "protocol");
    assert_eq!(ToolErrorKind::Unsupported.as_str(), "unsupported");
    assert_eq!(ToolErrorKind::Internal.as_str(), "internal");
}

#[test]
fn tool_result_serializes_error_message_meta_and_summary() {
    let ok = ToolResult::ok(
        "call-ok",
        "read_file",
        "hello",
        ToolResultMeta {
            returned_bytes: Some(5),
            ..ToolResultMeta::default()
        },
    )
    .with_error_details(true, json!({"ignored": true}));
    let ok_summary = ok.summary();
    assert_eq!(ok_summary.status_label, "ok");
    assert_eq!(ok_summary.bytes, Some(5));
    assert!(!ok_summary.is_error);

    let mut error = ToolResult::error(
        "call-err",
        "bash",
        ToolErrorKind::ExitStatus,
        "command failed",
    )
    .with_error_details(true, json!({"stderr": "boom"}));
    error.metadata = ToolResultMeta {
        exit_code: Some(7),
        truncated: true,
        changed_files: vec!["note.txt".to_owned()],
        details: json!({"path": "note.txt"}),
        ..ToolResultMeta::default()
    };

    let content: serde_json::Value =
        serde_json::from_str(&error.to_model_content()).expect("tool result should serialize");
    assert_eq!(content["status"], "error");
    assert_eq!(content["content"], "command failed");
    assert_eq!(content["error"]["kind"], "exit_status");
    assert_eq!(content["error"]["retryable"], true);
    assert_eq!(content["error"]["details"]["stderr"], "boom");
    assert_eq!(content["meta"]["exit_code"], 7);
    assert_eq!(content["meta"]["truncated"], true);
    assert_eq!(content["meta"]["details"]["path"], "note.txt");

    let message = error.to_model_message();
    assert_eq!(message.role, MessageRole::Tool);
    assert_eq!(message.tool_call_id.as_deref(), Some("call-err"));

    let summary = error.summary();
    assert_eq!(summary.status_label, "error");
    assert!(summary.is_error);
    assert_eq!(summary.error_kind, Some(ToolErrorKind::ExitStatus));
    assert_eq!(summary.error_message.as_deref(), Some("command failed"));
    assert_eq!(summary.exit_code, Some(7));
    assert_eq!(summary.changed_files, vec!["note.txt"]);
}

#[test]
fn bounded_diff_and_meta_helpers_cover_zero_budgets_and_empty_values() {
    let bounded = super::bounded_diff_text("@@ -1 +1 @@\n-a\n+b", 0, 32);
    assert!(bounded.truncated);
    assert!(bounded.diff.is_empty());

    assert_eq!(super::diff_line_count(""), 0);
    assert_eq!(super::diff_line_count("a\nb"), 2);

    assert!(ToolResultMeta::default().to_model_value().is_none());

    let meta = ToolResultMeta {
        duration_ms: Some(4),
        exit_code: Some(1),
        stdout_bytes: Some(12),
        stderr_bytes: Some(2),
        bytes: Some(14),
        truncated: true,
        omitted_bytes: Some(8),
        limit_bytes: Some(64),
        limit_lines: Some(16),
        returned_bytes: Some(14),
        returned_lines: Some(2),
        total_bytes: Some(22),
        total_lines: Some(3),
        returned_matches: Some(1),
        total_matches: Some(2),
        returned_entries: Some(3),
        total_entries: Some(4),
        changed_files: vec!["src/lib.rs".to_owned()],
        details: json!({"scope": "workspace"}),
    };
    let value = meta
        .to_model_value()
        .expect("populated meta should serialize");
    assert_eq!(value["duration_ms"], 4);
    assert_eq!(value["exit_code"], 1);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["changed_files"][0], "src/lib.rs");
    assert_eq!(value["details"]["scope"], "workspace");

    assert!(super::value_is_empty(&serde_json::Value::Null));
    assert!(super::value_is_empty(&json!([])));
    assert!(super::value_is_empty(&json!({})));
    assert!(!super::value_is_empty(&json!(true)));
    assert!(!super::value_is_empty(&json!("text")));
}

#[tokio::test]
async fn tool_registry_surfaces_errors_and_default_trait_hooks() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(DefaultHookTool));
    let ctx = ToolContext::new(std::env::temp_dir(), 5);
    let call = crate::ToolCall {
        id: "call-1".to_owned(),
        name: "default_hooks".to_owned(),
        args_json: "{}".to_owned(),
    };

    assert!(registry.preview(ctx.clone(), call.clone()).await?.is_none());
    assert_eq!(registry.permission_access(&ctx, &call)?, ToolAccess::Read);
    assert_eq!(registry.permission_default_mode(&ctx, &call)?, None);
    assert!(registry.egress_audit(&ctx, &call)?.is_none());

    let result = registry.execute(ctx.clone(), call.clone()).await?;
    assert_eq!(result.content, "ok");

    let invalid_args = crate::ToolCall {
        args_json: "{".to_owned(),
        ..call.clone()
    };
    assert!(
        registry
            .preview(ctx.clone(), invalid_args)
            .await
            .expect_err("invalid args should fail")
            .to_string()
            .contains("invalid tool args")
    );
    assert!(
        registry
            .execute(
                ctx,
                crate::ToolCall {
                    id: "missing".to_owned(),
                    name: "missing".to_owned(),
                    args_json: "{}".to_owned(),
                },
            )
            .await
            .expect_err("unknown tools should fail")
            .to_string()
            .contains("unknown tool missing")
    );

    let policy = ApprovalMode::Ask;
    assert_eq!(policy.as_str(), "ask");
    Ok(())
}

#[tokio::test]
async fn tool_registry_surfaces_unknown_and_invalid_args_for_all_public_hooks() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(DefaultHookTool));
    let ctx = ToolContext::new(std::env::temp_dir(), 5);
    let valid = crate::ToolCall {
        id: "call-1".to_owned(),
        name: "default_hooks".to_owned(),
        args_json: "{}".to_owned(),
    };
    let invalid = crate::ToolCall {
        args_json: "{".to_owned(),
        ..valid.clone()
    };
    let unknown = crate::ToolCall {
        name: "missing".to_owned(),
        ..valid.clone()
    };

    for error in [
        registry
            .execute(ctx.clone(), invalid.clone())
            .await
            .expect_err("invalid args should fail execute"),
        registry
            .preview(ctx.clone(), invalid.clone())
            .await
            .expect_err("invalid args should fail preview"),
    ] {
        assert!(
            error
                .to_string()
                .contains("invalid tool args for default_hooks")
        );
    }

    for error in [
        registry
            .permission_subjects(&ctx, &invalid)
            .expect_err("invalid args should fail permission subjects"),
        registry
            .permission_access(&ctx, &invalid)
            .expect_err("invalid args should fail permission access"),
        registry
            .permission_default_mode(&ctx, &invalid)
            .expect_err("invalid args should fail permission default mode"),
        registry
            .egress_audit(&ctx, &invalid)
            .expect_err("invalid args should fail egress audit"),
    ] {
        assert!(
            error
                .to_string()
                .contains("invalid tool args for default_hooks")
        );
    }

    assert!(registry.spec_for("missing").is_none());
    for error in [
        registry
            .preview(ctx.clone(), unknown.clone())
            .await
            .expect_err("unknown tool should fail preview"),
        registry
            .execute(ctx.clone(), unknown.clone())
            .await
            .expect_err("unknown tool should fail execute"),
    ] {
        assert!(error.to_string().contains("unknown tool missing"));
    }
    for error in [
        registry
            .permission_subjects(&ctx, &unknown)
            .expect_err("unknown tool should fail permission subjects"),
        registry
            .permission_access(&ctx, &unknown)
            .expect_err("unknown tool should fail permission access"),
        registry
            .permission_default_mode(&ctx, &unknown)
            .expect_err("unknown tool should fail permission default mode"),
        registry
            .egress_audit(&ctx, &unknown)
            .expect_err("unknown tool should fail egress audit"),
    ] {
        assert!(error.to_string().contains("unknown tool missing"));
    }

    assert_eq!(registry.specs().len(), 1);
    Ok(())
}

#[test]
fn tool_context_debug_redacts_mutation_recorder_details() {
    let ctx = crate::ToolContext::new("/tmp/workspace", 45);
    let rendered = format!("{ctx:?}");
    assert!(rendered.contains("workspace_root"));
    assert!(rendered.contains("timeout_secs"));
    assert!(rendered.contains("mutation_recorder: false"));
    assert_eq!(
        ctx.workspace_root,
        std::path::PathBuf::from("/tmp/workspace")
    );
    assert_eq!(ctx.timeout_secs, 45);
    assert!(ctx.mutation_recorder.is_none());
}
