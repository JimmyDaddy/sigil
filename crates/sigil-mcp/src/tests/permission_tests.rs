use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    McpServerTrustPolicy, McpTrustClass, NetworkEffect, Tool, ToolAccess, ToolAnalysisStatus,
    ToolCall, ToolCategory, ToolContext, ToolPermissionEffect, ToolPermissionPlanDraft,
    ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
};

use super::{
    McpPermissionBinding, McpPermissionTransport, McpToolAnnotations, classify_mcp_permission,
    mcp_permission_fingerprint, mcp_tool_permission_plan,
};

fn complete(read_only: bool, destructive: bool, open_world: bool) -> McpToolAnnotations {
    McpToolAnnotations {
        title: None,
        read_only_hint: Some(read_only),
        destructive_hint: Some(destructive),
        idempotent_hint: Some(!read_only),
        open_world_hint: Some(open_world),
    }
}

#[test]
fn missing_and_untrusted_annotations_fail_closed() {
    for (annotations, trust) in [
        (McpToolAnnotations::default(), McpTrustClass::Official),
        (complete(true, false, false), McpTrustClass::ThirdParty),
        (complete(true, true, false), McpTrustClass::SelfHosted),
    ] {
        let result =
            classify_mcp_permission(&annotations, trust, McpPermissionTransport::StreamableHttp);
        assert_eq!(result.access, ToolAccess::Execute);
        assert!(matches!(
            result.analysis,
            ToolAnalysisStatus::Conservative { .. }
        ));
        assert!(result.effects.contains(&ToolPermissionEffect::Unknown));
        assert!(
            result
                .effects
                .contains(&ToolPermissionEffect::NetworkUnknown)
        );
    }
}

#[test]
fn trusted_annotations_map_read_and_mutation_effects() {
    let read = classify_mcp_permission(
        &complete(true, false, true),
        McpTrustClass::Official,
        McpPermissionTransport::StreamableHttp,
    );
    assert_eq!(read.access, ToolAccess::Read);
    assert_eq!(read.network_effect, Some(NetworkEffect::Read));
    assert_eq!(
        read.effects,
        std::collections::BTreeSet::from([ToolPermissionEffect::NetworkRead])
    );
    assert!(matches!(read.analysis, ToolAnalysisStatus::Complete));

    let mutation = classify_mcp_permission(
        &complete(false, true, true),
        McpTrustClass::SelfHosted,
        McpPermissionTransport::Stdio,
    );
    assert_eq!(mutation.access, ToolAccess::Write);
    for effect in [
        ToolPermissionEffect::FileWrite,
        ToolPermissionEffect::FileDelete,
        ToolPermissionEffect::RemoteMutation,
        ToolPermissionEffect::NetworkMutate,
    ] {
        assert!(mutation.effects.contains(&effect));
    }
}

struct PlannedMcpTool {
    annotations: McpToolAnnotations,
    binding: McpPermissionBinding,
}

#[async_trait]
impl Tool for PlannedMcpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mcp__fixture__read".to_owned(),
            description: "fixture".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Mcp,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Read),
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> anyhow::Result<ToolPermissionPlanDraft> {
        mcp_tool_permission_plan(
            "mcp__fixture__read",
            "read",
            &self.annotations,
            &McpServerTrustPolicy::default(),
            McpPermissionTransport::StreamableHttp,
            vec![ToolSubject::mcp_tool("mcp__fixture__read")],
            &self.binding,
        )
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> anyhow::Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "mcp__fixture__read",
            "unused",
            ToolResultMeta::default(),
        ))
    }
}

#[test]
fn lifecycle_generation_changes_bound_plan_hash_without_leaking_source_material()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let context = ToolContext::new(temp.path().to_path_buf(), 5);
    let annotations = complete(true, false, false);
    let source = json!({
        "endpoint": "https://secret.example.test/private",
        "header_value": "top-secret-value",
    });
    let environment_binding = mcp_permission_fingerprint(&source)?;
    let call = ToolCall {
        id: "call".to_owned(),
        name: "mcp__fixture__read".to_owned(),
        args_json: "{}".to_owned(),
    };
    let plan_for = |generation: &str| -> anyhow::Result<_> {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(PlannedMcpTool {
            annotations: annotations.clone(),
            binding: McpPermissionBinding {
                execution_profile: mcp_permission_fingerprint(&json!({
                    "server_identity": "server-v1",
                    "lifecycle_generation": generation,
                }))?,
                environment_binding: environment_binding.clone(),
            },
        }));
        registry.permission_plan(&context, &call)
    };

    let first = plan_for("generation-a")?;
    let replacement = plan_for("generation-b")?;
    assert_ne!(first.plan_hash, replacement.plan_hash);
    let persisted = serde_json::to_string(&first)?;
    assert!(!persisted.contains("secret.example.test"));
    assert!(!persisted.contains("top-secret-value"));
    Ok(())
}
