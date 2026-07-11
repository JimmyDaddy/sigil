use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{CompiledMcpSchema, McpRemoteTool};

const SEARCH_ID_MAX_BYTES: usize = 256;

/// Exact, release-maintained adapter descriptor. User configuration never constructs this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownMcpSearchAdapter {
    pub adapter_id: String,
    pub codec_id: Option<String>,
    pub server_identity_fingerprint: String,
    pub tool_name: String,
    pub input_schema_fingerprint: String,
    pub output_schema_fingerprint: Option<String>,
}

/// Stable request modes. `GenericQueryText` never acquires a source contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSearchAdapterKind {
    KnownVersioned {
        adapter_id: String,
        codec_id: Option<String>,
    },
    GenericQueryText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSearchIncompatibility {
    InvalidIdentity,
    RequiredTaskUnsupported,
    SchemaDrift,
    QueryContractMissing,
    QueryContractAmbiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpStableSearchEligibility {
    Eligible(McpSearchAdapterKind),
    Incompatible(McpSearchIncompatibility),
}

/// Chooses one mutually exclusive stable-search adapter without guessing field aliases.
pub fn classify_mcp_search_binding(
    server_identity_fingerprint: &str,
    tool: &McpRemoteTool,
    known_adapters: &[KnownMcpSearchAdapter],
) -> McpStableSearchEligibility {
    if !valid_id(server_identity_fingerprint) || !valid_id(&tool.name) {
        return McpStableSearchEligibility::Incompatible(McpSearchIncompatibility::InvalidIdentity);
    }
    if tool.task_support.as_deref() == Some("required") {
        return McpStableSearchEligibility::Incompatible(
            McpSearchIncompatibility::RequiredTaskUnsupported,
        );
    }
    if CompiledMcpSchema::compile(&tool.input_schema).is_err()
        || tool
            .output_schema
            .as_ref()
            .is_some_and(|schema| CompiledMcpSchema::compile(schema).is_err())
    {
        return McpStableSearchEligibility::Incompatible(McpSearchIncompatibility::SchemaDrift);
    }
    let input_fingerprint = canonical_json_fingerprint(&tool.input_schema);
    let output_fingerprint = tool.output_schema.as_ref().map(canonical_json_fingerprint);
    if let Some(adapter) = known_adapters.iter().find(|adapter| {
        valid_known_adapter(adapter)
            && adapter.server_identity_fingerprint == server_identity_fingerprint
            && adapter.tool_name == tool.name
            && adapter.input_schema_fingerprint == input_fingerprint
            && adapter.output_schema_fingerprint == output_fingerprint
    }) {
        return McpStableSearchEligibility::Eligible(McpSearchAdapterKind::KnownVersioned {
            adapter_id: adapter.adapter_id.clone(),
            codec_id: adapter.codec_id.clone(),
        });
    }
    classify_generic_query_schema(&tool.input_schema)
}

#[must_use]
pub fn mcp_tool_schema_fingerprint(tool: &McpRemoteTool) -> String {
    canonical_json_fingerprint(&serde_json::json!({
        "name": tool.name,
        "input_schema": tool.input_schema,
        "output_schema": tool.output_schema,
        "task_support": tool.task_support,
    }))
}

#[must_use]
pub fn mcp_schema_fingerprint(schema: &Value) -> String {
    canonical_json_fingerprint(schema)
}

fn classify_generic_query_schema(schema: &Value) -> McpStableSearchEligibility {
    let Some(root) = schema.as_object() else {
        return incompatible_missing();
    };
    if root.get("type").and_then(Value::as_str) != Some("object")
        || root
            .get("additionalProperties")
            .is_some_and(|value| value != &Value::Bool(false))
        || root.keys().any(|key| {
            !matches!(
                key.as_str(),
                "$schema"
                    | "type"
                    | "properties"
                    | "required"
                    | "additionalProperties"
                    | "title"
                    | "description"
            )
        })
    {
        return incompatible_ambiguous();
    }
    let Some(required) = root.get("required").and_then(Value::as_array) else {
        return incompatible_missing();
    };
    if required.len() != 1 || required[0].as_str() != Some("query") {
        return if required.iter().any(|value| value.as_str() == Some("query")) {
            incompatible_ambiguous()
        } else {
            incompatible_missing()
        };
    }
    let Some(properties) = root.get("properties").and_then(Value::as_object) else {
        return incompatible_missing();
    };
    let Some(query) = properties.get("query").and_then(Value::as_object) else {
        return incompatible_missing();
    };
    if query.get("type").and_then(Value::as_str) != Some("string")
        || query
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "title" | "description"))
    {
        return incompatible_ambiguous();
    }
    McpStableSearchEligibility::Eligible(McpSearchAdapterKind::GenericQueryText)
}

fn incompatible_missing() -> McpStableSearchEligibility {
    McpStableSearchEligibility::Incompatible(McpSearchIncompatibility::QueryContractMissing)
}

fn incompatible_ambiguous() -> McpStableSearchEligibility {
    McpStableSearchEligibility::Incompatible(McpSearchIncompatibility::QueryContractAmbiguous)
}

fn valid_known_adapter(adapter: &KnownMcpSearchAdapter) -> bool {
    valid_id(&adapter.adapter_id)
        && adapter.codec_id.as_deref().is_none_or(valid_id)
        && valid_id(&adapter.server_identity_fingerprint)
        && valid_id(&adapter.tool_name)
        && is_sha256(&adapter.input_schema_fingerprint)
        && adapter
            .output_schema_fingerprint
            .as_deref()
            .is_none_or(is_sha256)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= SEARCH_ID_MAX_BYTES
        && value.is_ascii()
        && !value.chars().any(char::is_control)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn canonical_json_fingerprint(value: &Value) -> String {
    let canonical = canonicalize_json(value);
    let bytes = serde_json::to_vec(&canonical).expect("JSON values always serialize");
    format!("{:x}", Sha256::digest(bytes))
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize_json(&values[key]));
            }
            Value::Object(canonical)
        }
        scalar => scalar.clone(),
    }
}

#[cfg(test)]
#[path = "tests/search_binding_tests.rs"]
mod tests;
