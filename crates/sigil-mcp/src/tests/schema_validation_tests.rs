use serde_json::json;

use super::*;

#[test]
fn schema_validation_accepts_bounded_object_and_rejects_refs_and_mismatch() {
    let schema = json!({
        "type":"object",
        "properties":{
            "query":{"type":"string","pattern":"^[a-z ]+$"},
            "limit":{"type":"integer"}
        },
        "required":["query"],
        "additionalProperties":false
    });
    let compiled = CompiledMcpSchema::compile(&schema).expect("schema");
    compiled
        .validate(&json!({"query":"rust agent","limit":2}))
        .expect("valid");
    assert!(compiled.validate(&json!({"query":"BAD!"})).is_err());
    assert!(CompiledMcpSchema::compile(&json!({"$ref":"https://example/schema"})).is_err());
}

#[test]
fn schema_validation_enforces_depth_nodes_and_output_structured_content() {
    let mut schema = json!({"type":"string"});
    for _ in 0..=MAX_SCHEMA_DEPTH {
        schema = json!({"type":"array","items":schema});
    }
    assert!(CompiledMcpSchema::compile(&schema).is_err());
    let output = McpCallToolResult::parse(&json!({"content":[],"structuredContent":{"ok":true}}))
        .expect("content array is required but may be empty");
    assert_eq!(output.structured_content, Some(json!({"ok":true})));
    assert!(McpCallToolResult::parse(&json!({"structuredContent":{}})).is_err());
}

#[test]
fn schema_validation_supports_known_draft_local_refs_and_real_constraints() {
    let schema = json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "$defs":{
            "tag":{"type":"string","minLength":2,"maxLength":4,"pattern":"^[a-z]+$"}
        },
        "type":"object",
        "properties":{
            "tag":{"$ref":"#/$defs/tag"},
            "count":{"type":"integer","minimum":1,"maximum":3},
            "kinds":{"type":"array","items":{"oneOf":[{"const":"docs"},{"const":"code"}]},"minItems":1,"maxItems":2,"uniqueItems":true}
        },
        "required":["tag","count","kinds"],
        "additionalProperties":false
    });
    let compiled = CompiledMcpSchema::compile(&schema).expect("local ref schema");
    compiled
        .validate(&json!({"tag":"rust","count":2,"kinds":["docs","code"]}))
        .expect("constraints pass");
    for invalid in [
        json!({"tag":"RUST","count":2,"kinds":["docs"]}),
        json!({"tag":"r","count":2,"kinds":["docs"]}),
        json!({"tag":"rust","count":4,"kinds":["docs"]}),
        json!({"tag":"rust","count":2,"kinds":["docs","docs"]}),
        json!({"tag":"rust","count":2,"kinds":[]}),
    ] {
        assert!(compiled.validate(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn schema_validation_rejects_external_cycle_unknown_draft_and_partial_ref_siblings() {
    for invalid in [
        json!({"$ref":"https://example.test/schema"}),
        json!({"$defs":{"loop":{"$ref":"#/$defs/loop"}},"$ref":"#/$defs/loop"}),
        json!({"$schema":"https://example.test/unknown-draft","type":"string"}),
        json!({"type":"string","format":"uri"}),
        json!({"$defs":{"value":{"type":"string"}},"$ref":"#/$defs/value","minLength":2}),
    ] {
        assert!(CompiledMcpSchema::compile(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn schema_validation_enforces_64k_node_and_regex_caps() {
    let oversize = json!({"type":"string","description":"x".repeat(MAX_SCHEMA_BYTES)});
    assert!(CompiledMcpSchema::compile(&oversize).is_err());
    let too_many_nodes = json!({
        "$defs": (0..2100).map(|index| (format!("d{index}"), json!({"type":"string"}))).collect::<serde_json::Map<_,_>>()
    });
    assert!(CompiledMcpSchema::compile(&too_many_nodes).is_err());
    let regex = json!({"type":"string","pattern":"a".repeat(1025)});
    assert!(CompiledMcpSchema::compile(&regex).is_err());
}

#[test]
fn call_tool_result_requires_bounded_known_content_blocks() {
    assert!(McpCallToolResult::parse(&json!({"content":[{"type":"text","text":"ok"}]})).is_ok());
    assert!(McpCallToolResult::parse(&json!({"content":[{"type":"unknown"}]})).is_err());
    assert!(McpCallToolResult::parse(&json!({"content":["raw"]})).is_err());
    assert!(McpCallToolResult::parse(&json!({"content":[],"isError":"yes"})).is_err());
    assert!(matches!(
        McpCallToolResult::parse(&json!({"structuredContent":{"ok":true}})),
        Err(McpStreamableHttpError::MissingRequiredContent)
    ));
}
