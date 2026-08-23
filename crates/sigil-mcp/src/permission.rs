use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ExecutionContainmentRequest, McpServerTrustPolicy, McpTrustClass, NetworkEffect, ToolAccess,
    ToolAnalysisReason, ToolAnalysisReasonCode, ToolAnalysisStatus, ToolOperation,
    ToolPermissionEffect, ToolPermissionPlanDraft, ToolPermissionSummary, ToolSemanticScope,
    ToolSubject,
};

/// MCP tool hints as transmitted by the server.
///
/// Boolean fields deliberately remain optional. The MCP schema specifies defaults for wire
/// compatibility, but permission planning must distinguish an explicit claim from a missing one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

impl McpToolAnnotations {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.read_only_hint.is_none()
            && self.destructive_hint.is_none()
            && self.idempotent_hint.is_none()
            && self.open_world_hint.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpPermissionTransport {
    Stdio,
    StreamableHttp,
}

impl McpPermissionTransport {
    fn binding_name(self) -> &'static str {
        match self {
            Self::Stdio => "mcp_stdio_v2",
            Self::StreamableHttp => "mcp_streamable_http_v2",
        }
    }

    fn qualifier(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable_http",
        }
    }
}

/// Safe, already-fingerprinted execution identity bound into a V2 permission plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPermissionBinding {
    pub execution_profile: String,
    pub environment_binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPermissionClassification {
    pub access: ToolAccess,
    pub network_effect: Option<NetworkEffect>,
    pub effects: BTreeSet<ToolPermissionEffect>,
    pub analysis: ToolAnalysisStatus,
    qualifiers: BTreeMap<String, String>,
    summary_detail: String,
}

/// Produces a SHA-256 fingerprint over canonical JSON permission material.
///
/// The source material is process-local and must never be logged or persisted. Only the returned
/// digest is safe to bind into a permission plan.
pub fn mcp_permission_fingerprint(material: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(&canonical_permission_json(material))
        .context("failed to encode MCP permission material")?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn canonical_permission_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.iter().map(canonical_permission_json).collect())
        }
        Value::Object(object) => {
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_permission_json(value)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

/// Interprets MCP annotations fail-closed.
///
/// Third-party annotations are untrusted. Missing required boolean hints and internally
/// contradictory hints are incomplete analysis, even though the MCP wire schema has defaults.
#[must_use]
pub fn classify_mcp_permission(
    annotations: &McpToolAnnotations,
    trust_class: McpTrustClass,
    transport: McpPermissionTransport,
) -> McpPermissionClassification {
    let incomplete_reason = if trust_class == McpTrustClass::ThirdParty {
        Some("mcp_annotations_untrusted")
    } else if annotations.read_only_hint.is_none()
        || annotations.destructive_hint.is_none()
        || annotations.idempotent_hint.is_none()
        || annotations.open_world_hint.is_none()
    {
        Some("mcp_annotations_missing")
    } else if annotations.read_only_hint == Some(true) && annotations.destructive_hint == Some(true)
    {
        Some("mcp_annotations_conflicting")
    } else {
        None
    };

    if let Some(reason) = incomplete_reason {
        let mut effects = BTreeSet::from([
            ToolPermissionEffect::Unknown,
            ToolPermissionEffect::RemoteMutation,
            ToolPermissionEffect::NetworkUnknown,
        ]);
        if transport == McpPermissionTransport::Stdio {
            effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
            effects.insert(ToolPermissionEffect::FileWrite);
        }
        return McpPermissionClassification {
            access: ToolAccess::Execute,
            network_effect: Some(NetworkEffect::Unknown),
            effects,
            analysis: ToolAnalysisStatus::Conservative {
                reasons: vec![ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::UnprovenContainment,
                    Some(reason.to_owned()),
                )],
            },
            qualifiers: BTreeMap::from([
                ("annotation_status".to_owned(), reason.to_owned()),
                ("transport".to_owned(), transport.qualifier().to_owned()),
            ]),
            summary_detail:
                "MCP tool effects are unknown because its annotations are incomplete or untrusted"
                    .to_owned(),
        };
    }

    let read_only = annotations.read_only_hint == Some(true);
    let destructive = annotations.destructive_hint == Some(true);
    let idempotent = annotations.idempotent_hint == Some(true);
    let open_world = annotations.open_world_hint == Some(true);
    let mut effects = BTreeSet::new();
    let network_effect;
    let access;

    if read_only {
        access = ToolAccess::Read;
        if transport == McpPermissionTransport::Stdio {
            effects.insert(ToolPermissionEffect::FileRead);
        }
        if transport == McpPermissionTransport::StreamableHttp || open_world {
            effects.insert(ToolPermissionEffect::NetworkRead);
            network_effect = Some(NetworkEffect::Read);
        } else {
            network_effect = None;
        }
    } else {
        access = ToolAccess::Write;
        effects.insert(ToolPermissionEffect::RemoteMutation);
        if transport == McpPermissionTransport::Stdio {
            effects.insert(ToolPermissionEffect::FileWrite);
            if destructive {
                effects.insert(ToolPermissionEffect::FileDelete);
            }
        }
        if transport == McpPermissionTransport::StreamableHttp || open_world {
            effects.insert(ToolPermissionEffect::NetworkMutate);
            network_effect = Some(NetworkEffect::Mutate);
        } else {
            network_effect = None;
        }
    }

    McpPermissionClassification {
        access,
        network_effect,
        effects,
        analysis: ToolAnalysisStatus::Complete,
        qualifiers: BTreeMap::from([
            (
                "annotation_status".to_owned(),
                "trusted_complete".to_owned(),
            ),
            ("destructive".to_owned(), destructive.to_string()),
            ("idempotent".to_owned(), idempotent.to_string()),
            ("open_world".to_owned(), open_world.to_string()),
            ("read_only".to_owned(), read_only.to_string()),
            ("transport".to_owned(), transport.qualifier().to_owned()),
        ]),
        summary_detail: if read_only {
            "MCP server declares this tool read-only".to_owned()
        } else if destructive {
            "MCP server declares this tool mutating and potentially destructive".to_owned()
        } else {
            "MCP server declares this tool mutating".to_owned()
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mcp_tool_permission_plan(
    tool_name: &str,
    original_tool_name: &str,
    annotations: &McpToolAnnotations,
    trust: &McpServerTrustPolicy,
    transport: McpPermissionTransport,
    subjects: Vec<ToolSubject>,
    binding: &McpPermissionBinding,
) -> Result<ToolPermissionPlanDraft> {
    let classification = classify_mcp_permission(annotations, trust.trust_class, transport);
    let annotation_fingerprint = mcp_permission_fingerprint(
        &serde_json::to_value(annotations).context("failed to encode MCP annotations")?,
    )?;
    let mut semantic_scope = ToolSemanticScope::new("mcp_tool", 2);
    semantic_scope.qualifiers = classification.qualifiers.clone();
    semantic_scope.qualifiers.insert(
        "tool_identity".to_owned(),
        mcp_permission_fingerprint(&json!({ "name": original_tool_name }))?,
    );

    Ok(ToolPermissionPlanDraft {
        access: classification.access,
        operation: ToolOperation::NetworkRequest,
        effects: classification.effects,
        subjects,
        analysis: classification.analysis,
        containment: ExecutionContainmentRequest::default(),
        semantic_scope: Some(semantic_scope),
        tool_default_mode: Some(trust.approval_default),
        managed_file_access: None,
        analysis_bindings: BTreeMap::from([
            (
                "execution_backend".to_owned(),
                transport.binding_name().to_owned(),
            ),
            (
                "execution_profile".to_owned(),
                binding.execution_profile.clone(),
            ),
            (
                "environment_binding".to_owned(),
                binding.environment_binding.clone(),
            ),
            ("mcp_annotations".to_owned(), annotation_fingerprint),
        ]),
        safe_summary: ToolPermissionSummary {
            title: tool_name.to_owned(),
            detail: classification.summary_detail,
            step_count: 1,
            workspace_code_steps: 0,
        },
    })
}

#[cfg(test)]
#[path = "tests/permission_tests.rs"]
mod tests;
