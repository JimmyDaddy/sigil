use anyhow::Result;
use serde_json::{Value, json};
use termquill_kernel::ToolSpec;

use super::{StrictToolsMode, prepare_tools};

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
            read_only: true,
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
