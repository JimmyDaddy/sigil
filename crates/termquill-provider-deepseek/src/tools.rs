use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use termquill_kernel::ToolSpec;

use crate::config::StrictToolsMode;

#[derive(Debug)]
pub struct PreparedTools {
    pub payload: Option<Vec<Value>>,
    pub strict_mode_enabled: bool,
    pub diagnostics: Vec<ToolSchemaDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSchemaDiagnostic {
    pub level: ToolSchemaDiagnosticLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSchemaDiagnosticLevel {
    Notice,
}

pub fn prepare_tools(specs: &[ToolSpec], mode: StrictToolsMode) -> Result<PreparedTools> {
    if specs.is_empty() {
        return Ok(PreparedTools {
            payload: None,
            strict_mode_enabled: false,
            diagnostics: Vec::new(),
        });
    }

    match mode {
        StrictToolsMode::Off => Ok(PreparedTools {
            payload: Some(specs.iter().map(prepare_standard_tool).collect()),
            strict_mode_enabled: false,
            diagnostics: Vec::new(),
        }),
        StrictToolsMode::Auto => match specs
            .iter()
            .map(prepare_strict_tool)
            .collect::<Result<Vec<_>>>()
        {
            Ok(payload) => Ok(PreparedTools {
                payload: Some(payload),
                strict_mode_enabled: true,
                diagnostics: Vec::new(),
            }),
            Err(error) => Ok(PreparedTools {
                payload: Some(specs.iter().map(prepare_standard_tool).collect()),
                strict_mode_enabled: false,
                diagnostics: vec![ToolSchemaDiagnostic {
                    level: ToolSchemaDiagnosticLevel::Notice,
                    message: format!(
                        "DeepSeek strict tools disabled for this request; using standard tools: {error:#}"
                    ),
                }],
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
            diagnostics: Vec::new(),
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
            "parameters": normalize_schema(&spec.input_schema)
                .with_context(|| format!("tool `{}` strict schema normalization failed", spec.name))?,
        }
    }))
}

fn normalize_schema(schema: &Value) -> Result<Value> {
    normalize_schema_at(schema, "$")
}

fn normalize_schema_at(schema: &Value, path: &str) -> Result<Value> {
    match schema {
        Value::Object(map) => normalize_schema_object(map, path),
        Value::Bool(_) => {
            bail!("{path}: boolean JSON Schema is not supported in DeepSeek strict mode")
        }
        other => Err(anyhow!("{path}: unexpected JSON schema node {other}")),
    }
}

fn normalize_schema_object(map: &Map<String, Value>, path: &str) -> Result<Value> {
    if let Some(any_of) = map.get("anyOf").and_then(Value::as_array) {
        let mut normalized = Map::new();
        normalized.insert(
            "anyOf".to_owned(),
            Value::Array(
                any_of
                    .iter()
                    .enumerate()
                    .map(|(index, item)| normalize_schema_at(item, &any_of_path(path, index)))
                    .collect::<Result<Vec<_>>>()?,
            ),
        );
        copy_common_fields(map, &mut normalized);
        return Ok(Value::Object(normalized));
    }

    let schema_type = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{path}: strict tool schema requires explicit type"))?;

    match schema_type {
        "object" => normalize_object_schema(map, path),
        "array" => normalize_array_schema(map, path),
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
        other => bail!("{path}: unsupported strict schema type {other}"),
    }
}

fn normalize_object_schema(map: &Map<String, Value>, path: &str) -> Result<Value> {
    let properties = map
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{path}: object schema missing properties"))?;
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
        let normalized_prop = normalize_schema_at(prop_schema, &property_path(path, name))?;
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

fn normalize_array_schema(map: &Map<String, Value>, path: &str) -> Result<Value> {
    let items = map
        .get("items")
        .ok_or_else(|| anyhow!("{path}: array schema missing items"))?;
    let mut normalized = Map::new();
    normalized.insert("type".to_owned(), Value::String("array".to_owned()));
    normalized.insert(
        "items".to_owned(),
        normalize_schema_at(items, &items_path(path))?,
    );
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

fn property_path(parent: &str, name: &str) -> String {
    format!("{parent}.properties.{name}")
}

fn items_path(parent: &str) -> String {
    format!("{parent}.items")
}

fn any_of_path(parent: &str, index: usize) -> String {
    format!("{parent}.anyOf[{index}]")
}

#[cfg(test)]
#[path = "tests/tools_tests.rs"]
mod tests;
