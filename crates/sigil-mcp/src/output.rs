use super::*;

pub(super) fn bounded_mcp_tool_result(
    secret_redactor: &SecretRedactor,
    tool_name: &McpToolName,
    trust: &McpServerTrustPolicy,
    identity: &McpServerObservedIdentity,
    surface_kind: &str,
    operation: &str,
    content: String,
) -> (String, ToolResultMeta) {
    let redacted = secret_redactor.redact_text(&content);
    let budget = truncate_text_budget(&redacted, MCP_OUTPUT_LIMIT_BYTES, MCP_OUTPUT_LIMIT_LINES);
    let mut metadata = ToolResultMeta {
        bytes: Some(to_u64(budget.returned_bytes)),
        truncated: budget.truncated,
        omitted_bytes: if budget.truncated {
            Some(to_u64(budget.omitted_bytes))
        } else {
            None
        },
        limit_bytes: Some(to_u64(MCP_OUTPUT_LIMIT_BYTES)),
        limit_lines: Some(to_u64(MCP_OUTPUT_LIMIT_LINES)),
        returned_bytes: Some(to_u64(budget.returned_bytes)),
        returned_lines: Some(to_u64(budget.returned_lines)),
        total_bytes: Some(to_u64(budget.total_bytes)),
        total_lines: Some(to_u64(budget.total_lines)),
        details: json!({
            "mcp": {
                "server": tool_name.server_name,
                "tool": tool_name.original_name,
                "trust_class": trust.trust_class.as_str(),
                "kind": surface_kind,
                "operation": operation,
                "server_identity": identity.to_json(),
            }
        }),
        ..ToolResultMeta::default()
    };
    if budget.truncated {
        metadata.details["mcp"]["truncation"] = json!({
            "omitted_bytes": budget.omitted_bytes,
            "limit_bytes": MCP_OUTPUT_LIMIT_BYTES,
            "limit_lines": MCP_OUTPUT_LIMIT_LINES,
        });
    }
    (budget.content, metadata)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TextBudgetResult {
    pub(super) content: String,
    pub(super) truncated: bool,
    pub(super) total_bytes: usize,
    pub(super) total_lines: usize,
    pub(super) returned_bytes: usize,
    pub(super) returned_lines: usize,
    pub(super) omitted_bytes: usize,
}

pub(super) fn truncate_text_budget(
    text: &str,
    max_bytes: usize,
    max_lines: usize,
) -> TextBudgetResult {
    let total_bytes = text.len();
    let total_lines = text.lines().count().max(usize::from(!text.is_empty()));
    let mut returned = String::new();
    let mut returned_lines = 0usize;
    let mut truncated = false;

    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }
        if returned.len().saturating_add(line.len()) > max_bytes {
            let remaining = max_bytes.saturating_sub(returned.len());
            append_utf8_prefix(&mut returned, line, remaining);
            truncated = true;
            break;
        }
        returned.push_str(line);
        returned_lines += 1;
    }

    if !truncated && returned.len() < total_bytes {
        truncated = true;
    }
    if truncated {
        let marker = "\n[MCP output truncated]";
        if returned.len().saturating_add(marker.len()) <= max_bytes {
            returned.push_str(marker);
        }
    }
    let returned_bytes = returned.len();
    TextBudgetResult {
        content: returned,
        truncated,
        total_bytes,
        total_lines,
        returned_bytes,
        returned_lines: returned_lines.max(usize::from(returned_bytes > 0)),
        omitted_bytes: total_bytes.saturating_sub(returned_bytes),
    }
}

pub(super) fn append_utf8_prefix(output: &mut String, text: &str, byte_budget: usize) {
    if byte_budget == 0 {
        return;
    }
    let mut end = 0usize;
    for (index, ch) in text.char_indices() {
        let next = index + ch.len_utf8();
        if next > byte_budget {
            break;
        }
        end = next;
    }
    output.push_str(&text[..end]);
}

pub(super) fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub(super) fn summarize_egress_json(value: &Value) -> Value {
    let byte_count = serde_json::to_vec(value).map_or(0, |bytes| bytes.len());
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let field_types = keys
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        Value::String(json_type_label(object.get(key).unwrap_or(&Value::Null))),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            json!({
                "type": "object",
                "byte_count": byte_count,
                "top_level_keys": keys,
                "field_types": field_types,
            })
        }
        Value::Array(items) => json!({
            "type": "array",
            "byte_count": byte_count,
            "item_count": items.len(),
        }),
        other => json!({
            "type": json_type_label(other),
            "byte_count": byte_count,
        }),
    }
}

pub(super) fn json_type_label(value: &Value) -> String {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_owned()
}
