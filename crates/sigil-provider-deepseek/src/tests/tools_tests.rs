use anyhow::Result;
use serde_json::{Value, json};
use sigil_kernel::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec};

use super::{StrictToolsMode, ToolSchemaDiagnosticLevel, prepare_tools};

fn tool_spec(name: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: name.to_owned(),
        input_schema,
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        preview: ToolPreviewCapability::None,
    }
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
fn strict_always_errors_for_unsupported_schema() {
    let error = prepare_tools(
        &[ToolSpec {
            name: "unsupported".to_owned(),
            description: "unsupported".to_owned(),
            input_schema: Value::Bool(true),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }],
        StrictToolsMode::Always,
    )
    .expect_err("strict always should reject unsupported schemas");

    assert!(format!("{error:#}").contains("boolean JSON Schema is not supported"));
}

#[test]
fn local_tool_metadata_does_not_affect_standard_tool_wire_schema() -> Result<()> {
    let read_tool = ToolSpec {
        name: "inspect".to_owned(),
        description: "inspect".to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access: ToolAccess::Read,
        preview: ToolPreviewCapability::None,
    };
    let write_tool = ToolSpec {
        category: ToolCategory::Shell,
        access: ToolAccess::Execute,
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
