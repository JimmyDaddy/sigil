use serde_json::json;

use super::*;
use crate::McpToolAnnotations;

fn tool(schema: Value) -> McpRemoteTool {
    McpRemoteTool {
        name: "search".to_owned(),
        description: None,
        input_schema: schema,
        output_schema: None,
        task_support: None,
        annotations: McpToolAnnotations::default(),
    }
}

#[test]
fn search_binding_generic_accepts_only_exact_required_query_string() {
    let eligible = tool(json!({
        "type":"object",
        "properties":{
            "query":{"type":"string","description":"query"},
            "optional":{"type":"integer"}
        },
        "required":["query"],
        "additionalProperties":false
    }));
    assert_eq!(
        classify_mcp_search_binding("identity", &eligible, &[]),
        McpStableSearchEligibility::Eligible(McpSearchAdapterKind::GenericQueryText)
    );
    for schema in [
        json!({"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}),
        json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query","limit"]}),
        json!({"type":"object","properties":{"query":{"type":"string","pattern":".*"}},"required":["query"]}),
        json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"oneOf":[]}),
        json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":true}),
        json!({"$ref":"#/$defs/request","$defs":{"request":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}}}),
    ] {
        assert!(matches!(
            classify_mcp_search_binding("identity", &tool(schema), &[]),
            McpStableSearchEligibility::Incompatible(_)
        ));
    }
}

#[test]
fn remote_tool_reads_task_support_from_current_execution_shape() {
    let parsed: McpRemoteTool = serde_json::from_value(json!({
        "name": "search",
        "inputSchema": {"type":"object","properties":{}},
        "execution": {"taskSupport":"forbidden"}
    }))
    .expect("tool");
    assert_eq!(parsed.task_support.as_deref(), Some("forbidden"));
}

#[test]
fn remote_tool_preserves_annotation_presence_and_values() {
    let parsed: McpRemoteTool = serde_json::from_value(json!({
        "name": "mutate",
        "inputSchema": {"type":"object","properties":{}},
        "annotations": {
            "title": "Mutate remote record",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true
        }
    }))
    .expect("tool");
    assert_eq!(
        parsed.annotations,
        McpToolAnnotations {
            title: Some("Mutate remote record".to_owned()),
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        }
    );

    let missing: McpRemoteTool = serde_json::from_value(json!({
        "name": "unknown",
        "inputSchema": {"type":"object","properties":{}}
    }))
    .expect("tool");
    assert_eq!(missing.annotations.read_only_hint, None);
}

#[test]
fn search_binding_known_adapter_requires_all_exact_fingerprints() {
    let remote = tool(json!({
        "type":"object",
        "properties":{"query":{"type":"string"},"numResults":{"type":"number"}},
        "required":["query"],
        "additionalProperties":false
    }));
    let descriptor = KnownMcpSearchAdapter {
        adapter_id: "known-v1".to_owned(),
        codec_id: Some("codec-v1".to_owned()),
        server_identity_fingerprint: "identity".to_owned(),
        tool_name: "search".to_owned(),
        input_schema_fingerprint: canonical_json_fingerprint(&remote.input_schema),
        output_schema_fingerprint: None,
    };
    assert!(matches!(
        classify_mcp_search_binding("identity", &remote, std::slice::from_ref(&descriptor)),
        McpStableSearchEligibility::Eligible(McpSearchAdapterKind::KnownVersioned { .. })
    ));
    assert_eq!(
        classify_mcp_search_binding("changed", &remote, &[descriptor]),
        McpStableSearchEligibility::Eligible(McpSearchAdapterKind::GenericQueryText)
    );
}

#[test]
fn search_binding_required_tasks_and_schema_drift_are_incompatible() {
    let mut task = tool(
        json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}),
    );
    task.task_support = Some("required".to_owned());
    assert_eq!(
        classify_mcp_search_binding("identity", &task, &[]),
        McpStableSearchEligibility::Incompatible(McpSearchIncompatibility::RequiredTaskUnsupported)
    );
    let drift = tool(
        json!({"type":"object","properties":{"query":{"type":"string","format":"uri"}},"required":["query"]}),
    );
    assert_eq!(
        classify_mcp_search_binding("identity", &drift, &[]),
        McpStableSearchEligibility::Incompatible(McpSearchIncompatibility::SchemaDrift)
    );
}
