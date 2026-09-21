use anyhow::Result;
use serde_json::{Value, json};
use sigil_kernel::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec};

use super::{
    StrictToolsMode, ToolSchemaDiagnosticLevel, prepare_tools, sanitize_request_tool_schemas,
};

fn tool_spec(name: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: name.to_owned(),
        input_schema,
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

fn assert_no_true_additional_properties(value: &Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "additionalProperties" {
                    assert_ne!(child, &Value::Bool(true));
                }
                assert_no_true_additional_properties(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_true_additional_properties(item);
            }
        }
        _ => {}
    }
}

#[test]
fn final_wire_boundary_removes_true_additional_properties_from_every_tool_schema() {
    let mut body = json!({
        "tools": [
            {
                "type": "function",
                "function": {
                    "parameters": {
                        "type": "object",
                        "additionalProperties": true,
                        "properties": {
                            "members": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": true
                                }
                            }
                        }
                    }
                }
            },
            {
                "name": "custom_tool",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "nested": {
                            "type": "object",
                            "additionalProperties": true
                        }
                    }
                }
            }
        ]
    });

    sanitize_request_tool_schemas(&mut body);

    assert_no_true_additional_properties(&body["tools"]);
    assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    assert_eq!(body["tools"][1]["name"], "custom_tool");
}

#[test]
fn strict_mode_normalizes_optional_fields() -> Result<()> {
    let prepared = prepare_tools(
        &[ToolSpec {
            name: "ls".to_owned(),
            description: "list".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "recursive": {"type": "boolean"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        StrictToolsMode::Always,
    )?;
    let parameters = &prepared
        .payload
        .as_ref()
        .expect("strict tools payload missing")[0]["function"]["parameters"];
    assert_eq!(parameters["additionalProperties"], Value::Bool(false));
    assert_eq!(
        parameters["required"]
            .as_array()
            .expect("required array missing")
            .len(),
        2
    );
    assert!(parameters["properties"]["recursive"]["anyOf"].is_array());
    Ok(())
}

#[test]
fn strict_auto_falls_back_to_standard_tools_for_unsupported_schema() -> Result<()> {
    let prepared = prepare_tools(
        &[ToolSpec {
            name: "unsupported".to_owned(),
            description: "unsupported".to_owned(),
            input_schema: Value::Bool(true),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        StrictToolsMode::Auto,
    )?;

    assert!(!prepared.strict_mode_enabled);
    let function = &prepared.payload.as_ref().expect("tools payload missing")[0]["function"];
    assert_eq!(function["name"], "unsupported");
    assert!(function.get("strict").is_none());
    assert_eq!(function["parameters"], Value::Bool(true));
    assert_eq!(prepared.diagnostics.len(), 1);
    assert_eq!(
        prepared.diagnostics[0].level,
        ToolSchemaDiagnosticLevel::Notice
    );
    assert!(
        prepared.diagnostics[0]
            .message
            .contains("using standard tools")
    );
    assert!(prepared.diagnostics[0].message.contains("$"));
    Ok(())
}

#[test]
fn standard_fallback_omits_true_additional_properties_at_every_schema_depth() -> Result<()> {
    let prepared = prepare_tools(
        &[tool_spec(
            "unsupported_with_permissive_objects",
            json!({
                "type": "object",
                "properties": {
                    "nested": {
                        "type": "object",
                        "properties": {"value": {"type": "string"}},
                        "additionalProperties": true
                    },
                    "unsupported": true
                },
                "additionalProperties": true
            }),
        )],
        StrictToolsMode::Auto,
    )?;

    assert!(!prepared.strict_mode_enabled);
    let payload = prepared.payload.expect("standard fallback payload");
    let function = &payload[0]["function"];
    assert_no_true_additional_properties(function);
    assert!(function["parameters"].get("additionalProperties").is_none());
    assert!(
        function["parameters"]["properties"]["nested"]
            .get("additionalProperties")
            .is_none()
    );
    assert_eq!(
        function["parameters"]["properties"]["nested"]["properties"]["value"]["type"],
        "string"
    );
    Ok(())
}

#[test]
fn strict_always_errors_for_unsupported_schema() {
    let error = prepare_tools(
        &[ToolSpec {
            name: "unsupported".to_owned(),
            description: "unsupported".to_owned(),
            input_schema: Value::Bool(true),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject unsupported schemas");

    assert!(format!("{error:#}").contains("boolean JSON Schema is not supported"));
}

#[test]
fn strict_always_errors_for_non_object_schema_nodes_and_unknown_types() {
    let scalar_error = prepare_tools(
        &[tool_spec("scalar", json!("bad"))],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject scalar schemas");
    assert!(format!("{scalar_error:#}").contains("unexpected JSON schema node"));

    let type_error = prepare_tools(
        &[tool_spec("unknown", json!({ "type": "function" }))],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject unsupported schema types");
    assert!(format!("{type_error:#}").contains("unsupported strict schema type function"));
}

#[test]
fn local_tool_metadata_does_not_affect_standard_tool_wire_schema() -> Result<()> {
    let read_tool = ToolSpec {
        name: "inspect".to_owned(),
        description: "inspect".to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    };
    let write_tool = ToolSpec {
        category: ToolCategory::Shell,
        access: ToolAccess::Execute,
        network_effect: None,
        preview: ToolPreviewCapability::Required,
        ..read_tool.clone()
    };

    let read_payload = prepare_tools(&[read_tool], StrictToolsMode::Off)?
        .payload
        .expect("read payload missing");
    let write_payload = prepare_tools(&[write_tool], StrictToolsMode::Off)?
        .payload
        .expect("write payload missing");

    assert_eq!(read_payload, write_payload);
    Ok(())
}

#[test]
fn standard_mode_preserves_original_schema_without_strict_diagnostics() -> Result<()> {
    let prepared = prepare_tools(
        &[tool_spec(
            "standard",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    }
                },
                "required": ["query"]
            }),
        )],
        StrictToolsMode::Off,
    )?;

    assert!(!prepared.strict_mode_enabled);
    assert!(prepared.diagnostics.is_empty());
    let tool = &prepared.payload.as_ref().expect("standard payload missing")[0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["name"], "standard");
    assert_eq!(
        tool["function"]["parameters"]["properties"]["query"]["description"],
        "Search query"
    );
    assert!(tool["function"].get("strict").is_none());
    Ok(())
}

#[test]
fn strict_mode_normalizes_nested_objects_arrays_enum_and_any_of() -> Result<()> {
    let prepared = prepare_tools(
        &[tool_spec(
            "complex",
            json!({
                "type": "object",
                "properties": {
                    "settings": {
                        "type": "object",
                        "properties": {
                            "enabled": {"type": "boolean"},
                            "mode": {
                                "type": "string",
                                "enum": ["fast", "safe"]
                            }
                        },
                        "required": ["enabled"]
                    },
                    "tags": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["code", "docs"]
                        }
                    },
                    "choice": {
                        "anyOf": [
                            {"type": "string"},
                            {"type": "integer"}
                        ]
                    }
                },
                "required": ["settings", "tags", "choice"]
            }),
        )],
        StrictToolsMode::Always,
    )?;

    let parameters = &prepared
        .payload
        .as_ref()
        .expect("strict tools payload missing")[0]["function"]["parameters"];
    let settings = &parameters["properties"]["settings"];
    assert_eq!(settings["additionalProperties"], Value::Bool(false));
    assert_eq!(
        settings["properties"]["mode"]["anyOf"][0]["enum"],
        json!(["fast", "safe"])
    );
    assert_eq!(
        parameters["properties"]["tags"]["items"]["enum"],
        json!(["code", "docs"])
    );
    assert_eq!(
        parameters["properties"]["choice"]["anyOf"][1]["type"],
        "integer"
    );
    Ok(())
}

#[test]
fn strict_mode_preserves_primitive_descriptions_and_any_of_common_fields() -> Result<()> {
    let prepared = prepare_tools(
        &[tool_spec(
            "described",
            json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "description": "Execution mode",
                        "enum": ["fast", "safe"]
                    },
                    "choice": {
                        "description": "Value choice",
                        "anyOf": [
                            {"type": "string"},
                            {"type": "integer"}
                        ]
                    }
                },
                "required": ["mode", "choice"]
            }),
        )],
        StrictToolsMode::Always,
    )?;

    let parameters = &prepared
        .payload
        .as_ref()
        .expect("strict tools payload missing")[0]["function"]["parameters"];
    assert_eq!(
        parameters["properties"]["mode"]["description"],
        "Execution mode"
    );
    assert_eq!(
        parameters["properties"]["choice"]["description"],
        "Value choice"
    );
    Ok(())
}

#[test]
fn strict_mode_normalizes_number_null_and_missing_required_list() -> Result<()> {
    let prepared = prepare_tools(
        &[tool_spec(
            "primitive_edges",
            json!({
                "type": "object",
                "description": "Root object",
                "properties": {
                    "score": { "type": "number" },
                    "unset": { "type": "null" }
                }
            }),
        )],
        StrictToolsMode::Always,
    )?;

    let parameters = &prepared
        .payload
        .as_ref()
        .expect("strict tools payload missing")[0]["function"]["parameters"];
    assert_eq!(parameters["description"], "Root object");
    assert_eq!(
        parameters["properties"]["score"]["anyOf"][0]["type"],
        "number"
    );
    assert_eq!(
        parameters["properties"]["unset"]["anyOf"][0]["type"],
        "null"
    );
    assert_eq!(parameters["required"], json!(["score", "unset"]));
    Ok(())
}

#[test]
fn strict_always_errors_for_missing_type_properties_items_and_bad_any_of_items() {
    let missing_type = prepare_tools(
        &[tool_spec(
            "missing_type",
            json!({ "description": "no type" }),
        )],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject schemas without type");
    assert!(format!("{missing_type:#}").contains("$: strict tool schema requires explicit type"));

    let missing_properties = prepare_tools(
        &[tool_spec("missing_properties", json!({ "type": "object" }))],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject object schemas without properties");
    assert!(format!("{missing_properties:#}").contains("$: object schema missing properties"));

    let missing_items = prepare_tools(
        &[tool_spec("missing_items", json!({ "type": "array" }))],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject array schemas without items");
    assert!(format!("{missing_items:#}").contains("$: array schema missing items"));

    let bad_any_of = prepare_tools(
        &[tool_spec(
            "bad_any_of",
            json!({
                "anyOf": [
                    { "type": "string" },
                    true
                ]
            }),
        )],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should include the failing anyOf item path");
    assert!(format!("{bad_any_of:#}").contains("$.anyOf[1]"));
}

#[test]
fn strict_always_errors_include_schema_path() {
    let error = prepare_tools(
        &[tool_spec(
            "bad_nested",
            json!({
                "type": "object",
                "properties": {
                    "nested": {
                        "type": "object",
                        "properties": {
                            "bad": true
                        },
                        "required": ["bad"]
                    }
                },
                "required": ["nested"]
            }),
        )],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject unsupported nested schemas");

    let message = format!("{error:#}");
    assert!(message.contains("tool `bad_nested` strict schema normalization failed"));
    assert!(message.contains("$.properties.nested.properties.bad"));
}

#[test]
fn task_checklist_schema_normalizes_without_fallback_or_nullable_defaults() -> Result<()> {
    let checklist = sigil_kernel::update_task_checklist_tool_spec();
    for mode in [StrictToolsMode::Always, StrictToolsMode::Auto] {
        let prepared = prepare_tools(std::slice::from_ref(&checklist), mode)?;
        assert!(prepared.strict_mode_enabled);
        assert!(prepared.diagnostics.is_empty());
        let function = &prepared.payload.as_ref().expect("checklist payload")[0]["function"];
        assert_eq!(function["strict"], true);
        assert_no_true_additional_properties(function);
        let parameters = &function["parameters"];
        assert_eq!(parameters["additionalProperties"], false);
        let item = &parameters["properties"]["items"]["items"];
        assert_eq!(item["additionalProperties"], false);
        assert_eq!(item["properties"]["text"]["type"], "string");
        assert_eq!(item["properties"]["status"]["type"], "string");
        assert_eq!(
            item["properties"].as_object().expect("item fields").len(),
            2
        );
        for field in item["properties"]
            .as_object()
            .expect("item fields")
            .values()
        {
            assert!(field.get("anyOf").is_none());
        }
    }
    Ok(())
}
