use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use termquill_kernel::ToolSpec;

use crate::config::StrictToolsMode;

#[derive(Debug)]
pub struct PreparedTools {
    pub payload: Option<Vec<Value>>,
    pub strict_mode_enabled: bool,
}

pub fn prepare_tools(specs: &[ToolSpec], mode: StrictToolsMode) -> Result<PreparedTools> {
    if specs.is_empty() {
        return Ok(PreparedTools {
            payload: None,
            strict_mode_enabled: false,
        });
    }

    match mode {
        StrictToolsMode::Off => Ok(PreparedTools {
            payload: Some(specs.iter().map(prepare_standard_tool).collect()),
            strict_mode_enabled: false,
        }),
        StrictToolsMode::Auto => match specs
            .iter()
            .map(prepare_strict_tool)
            .collect::<Result<Vec<_>>>()
        {
            Ok(payload) => Ok(PreparedTools {
                payload: Some(payload),
                strict_mode_enabled: true,
            }),
            Err(_) => Ok(PreparedTools {
                payload: Some(specs.iter().map(prepare_standard_tool).collect()),
                strict_mode_enabled: false,
            }),
        },
        StrictToolsMode::Always => Ok(PreparedTools {
            payload: Some(
                specs
                    .iter()
                    .map(prepare_strict_tool)
                    .collect::<Result<Vec<_>>>()?,
            ),
            strict_mode_enabled: true,
        }),
    }
}

fn prepare_standard_tool(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": spec.description,
            "parameters": spec.input_schema,
        }
    })
}

fn prepare_strict_tool(spec: &ToolSpec) -> Result<Value> {
    Ok(json!({
        "type": "function",
        "function": {
            "name": spec.name,
            "description": spec.description,
            "strict": true,
            "parameters": normalize_schema(&spec.input_schema)?,
        }
    }))
}

fn normalize_schema(schema: &Value) -> Result<Value> {
    match schema {
        Value::Object(map) => normalize_schema_object(map),
        Value::Bool(_) => bail!("boolean JSON Schema is not supported in DeepSeek strict mode"),
        other => Err(anyhow!("unexpected JSON schema node {other}")),
    }
}

fn normalize_schema_object(map: &Map<String, Value>) -> Result<Value> {
    if let Some(any_of) = map.get("anyOf").and_then(Value::as_array) {
        let mut normalized = Map::new();
        normalized.insert(
            "anyOf".to_owned(),
            Value::Array(
                any_of
                    .iter()
                    .map(normalize_schema)
                    .collect::<Result<Vec<_>>>()?,
            ),
        );
        copy_common_fields(map, &mut normalized);
        return Ok(Value::Object(normalized));
    }

    let schema_type = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("strict tool schema requires explicit type"))?;

    match schema_type {
        "object" => normalize_object_schema(map),
        "array" => normalize_array_schema(map),
        "string" | "number" | "integer" | "boolean" | "null" => {
            let mut normalized = Map::new();
            normalized.insert("type".to_owned(), Value::String(schema_type.to_owned()));
            if let Some(enum_values) = map.get("enum") {
                normalized.insert("enum".to_owned(), enum_values.clone());
            }
            if let Some(description) = map.get("description") {
                normalized.insert("description".to_owned(), description.clone());
            }
            Ok(Value::Object(normalized))
        }
        other => bail!("unsupported strict schema type {other}"),
    }
}

fn normalize_object_schema(map: &Map<String, Value>) -> Result<Value> {
    let properties = map
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("object schema missing properties"))?;
    let required = map
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut normalized_properties = Map::new();
    let mut normalized_required = Vec::new();

    for (name, prop_schema) in properties {
        let normalized_prop = normalize_schema(prop_schema)?;
        let required_here = required.iter().any(|item| item == name);
        normalized_properties.insert(
            name.clone(),
            if required_here {
                normalized_prop
            } else {
                wrap_optional(normalized_prop)
            },
        );
        normalized_required.push(Value::String(name.clone()));
    }

    let mut normalized = Map::new();
    normalized.insert("type".to_owned(), Value::String("object".to_owned()));
    normalized.insert(
        "properties".to_owned(),
        Value::Object(normalized_properties),
    );
    normalized.insert("required".to_owned(), Value::Array(normalized_required));
    normalized.insert("additionalProperties".to_owned(), Value::Bool(false));
    copy_common_fields(map, &mut normalized);
    Ok(Value::Object(normalized))
}

fn normalize_array_schema(map: &Map<String, Value>) -> Result<Value> {
    let items = map
        .get("items")
        .ok_or_else(|| anyhow!("array schema missing items"))?;
    let mut normalized = Map::new();
    normalized.insert("type".to_owned(), Value::String("array".to_owned()));
    normalized.insert("items".to_owned(), normalize_schema(items)?);
    copy_common_fields(map, &mut normalized);
    Ok(Value::Object(normalized))
}

fn wrap_optional(schema: Value) -> Value {
    json!({
        "anyOf": [
            schema,
            { "type": "null" }
        ]
    })
}

fn copy_common_fields(source: &Map<String, Value>, target: &mut Map<String, Value>) {
    for key in ["description", "enum"] {
        if let Some(value) = source.get(key) {
            target.insert(key.to_owned(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
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
}
