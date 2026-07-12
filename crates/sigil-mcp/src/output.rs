use std::io::{self, Write};

use super::*;

const MCP_JSON_PRETTY_THRESHOLD_BYTES: usize = 16 * 1024;
const MCP_METADATA_TEXT_LIMIT_BYTES: usize = 512;
const MCP_METADATA_GRANT_NAME_LIMIT_BYTES: usize = 128;
const MCP_METADATA_GRANT_NAMES_LIMIT: usize = 32;
const MCP_METADATA_GRANT_NAMES_TOTAL_BYTES: usize = 4 * 1024;
const MCP_EGRESS_FIELD_LIMIT: usize = 64;
const MCP_EGRESS_KEY_LIMIT_BYTES: usize = 256;
const MCP_EGRESS_KEYS_TOTAL_BYTES: usize = 8 * 1024;

pub(super) struct McpProtocolErrorProjection {
    pub(super) summary: String,
    pub(super) details: Value,
}

pub(super) fn bounded_mcp_protocol_error(
    secret_redactor: &SecretRedactor,
    error: &Value,
    operation_label: &str,
) -> McpProtocolErrorProjection {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("remote MCP error");
    let prefix = format!("{operation_label} (code {code}): ");
    let budget = bounded_mcp_text_segments(secret_redactor, [prefix.as_str(), message], "");
    let message_returned_bytes = budget
        .returned_bytes
        .saturating_sub(prefix.len())
        .min(message.len());
    let message_total_lines = text_line_count(message);
    let message_returned_lines = budget.returned_lines.min(message_total_lines);
    let data_projection = error.get("data").map(protocol_error_data_projection);
    let details = secret_safe_mcp_metadata(
        secret_redactor,
        json!({
            "remote_error": {
                "code": code,
                "message_total_bytes": message.len(),
                "message_returned_bytes": message_returned_bytes,
                "message_returned_lines": message_returned_lines,
                "message_total_lines": message_total_lines,
                "message_omitted_bytes": message.len().saturating_sub(message_returned_bytes),
                "message_truncated": message_returned_bytes < message.len(),
                "summary_rendered_bytes": budget.content.len(),
                "limit_bytes": MCP_OUTPUT_LIMIT_BYTES,
                "limit_lines": MCP_OUTPUT_LIMIT_LINES,
                "data": data_projection,
            }
        }),
    );
    McpProtocolErrorProjection {
        summary: budget.content,
        details,
    }
}

fn protocol_error_data_projection(value: &Value) -> Value {
    let mut projection = json!({
        "type": json_type_label(value),
        "wire_bytes": json_wire_bytes(value),
    });
    match value {
        Value::String(text) => {
            projection["bytes"] = json!(text.len());
            projection["chars"] = json!(text.chars().count());
        }
        Value::Array(items) => projection["items"] = json!(items.len()),
        Value::Object(fields) => projection["keys"] = json!(fields.len()),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    projection
}

fn json_wire_bytes(value: &Value) -> usize {
    struct CountingWriter(usize);

    impl Write for CountingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = CountingWriter(0);
    if serde_json::to_writer(&mut writer, value).is_err() {
        return usize::MAX;
    }
    writer.0
}

pub(super) fn bounded_mcp_text(secret_redactor: &SecretRedactor, text: &str) -> TextBudgetResult {
    bounded_mcp_text_segments(secret_redactor, [text], "")
}

pub(super) fn bounded_mcp_text_segments<'a>(
    secret_redactor: &SecretRedactor,
    segments: impl IntoIterator<Item = &'a str>,
    separator: &str,
) -> TextBudgetResult {
    let mut collector = BoundedSourceCollector::new(MCP_OUTPUT_LIMIT_BYTES, MCP_OUTPUT_LIMIT_LINES);
    let mut first = true;
    for segment in segments {
        if !first {
            collector.observe(separator.as_bytes());
        }
        collector.observe(segment.as_bytes());
        first = false;
    }
    collector.finish(secret_redactor)
}

pub(super) fn bounded_mcp_json(
    secret_redactor: &SecretRedactor,
    value: &Value,
) -> Result<TextBudgetResult> {
    // Redact before JSON escaping. Otherwise credentials containing newlines, quotes, or control
    // characters would no longer match their configured carrier after serialization.
    let redacted_value = secret_redactor.redact_value(value);
    let mut collector = BoundedSourceCollector::new(MCP_OUTPUT_LIMIT_BYTES, MCP_OUTPUT_LIMIT_LINES);
    if json_wire_bytes(&redacted_value) <= MCP_JSON_PRETTY_THRESHOLD_BYTES {
        serde_json::to_writer_pretty(&mut collector, &redacted_value)
            .context("failed to serialize bounded MCP JSON output")?;
    } else {
        serde_json::to_writer(&mut collector, &redacted_value)
            .context("failed to serialize bounded MCP JSON output")?;
    }
    Ok(collector.finish(secret_redactor))
}

pub(super) fn bounded_mcp_tool_result(
    secret_redactor: &SecretRedactor,
    tool_name: &McpToolName,
    trust: &McpServerTrustPolicy,
    identity: &McpServerObservedIdentity,
    surface_kind: &str,
    operation: &str,
    budget: TextBudgetResult,
) -> (String, ToolResultMeta) {
    let server = bounded_mcp_metadata_text(secret_redactor, &tool_name.server_name);
    let tool = bounded_mcp_metadata_text(secret_redactor, &tool_name.original_name);
    let mut mcp_details = json!({
        "server": server.value,
        "tool": tool.value,
        "trust_class": trust.trust_class.as_str(),
        "kind": surface_kind,
        "operation": operation,
        "server_identity": bounded_mcp_identity_projection(secret_redactor, identity),
        "rendered_bytes": budget.content.len(),
    });
    add_bounded_text_evidence(&mut mcp_details, "server", &server);
    add_bounded_text_evidence(&mut mcp_details, "tool", &tool);
    if budget.truncated {
        mcp_details["truncation"] = json!({
            "omitted_bytes": budget.omitted_bytes,
            "retained_source_bytes": budget.returned_bytes,
            "limit_bytes": MCP_OUTPUT_LIMIT_BYTES,
            "limit_lines": MCP_OUTPUT_LIMIT_LINES,
        });
    }
    let details = secret_safe_mcp_metadata(secret_redactor, json!({ "mcp": mcp_details }));
    let metadata = ToolResultMeta {
        bytes: Some(to_u64(budget.content.len())),
        truncated: budget.truncated,
        omitted_bytes: budget.truncated.then_some(to_u64(budget.omitted_bytes)),
        limit_bytes: Some(to_u64(MCP_OUTPUT_LIMIT_BYTES)),
        limit_lines: Some(to_u64(MCP_OUTPUT_LIMIT_LINES)),
        returned_bytes: Some(to_u64(budget.returned_bytes)),
        returned_lines: Some(to_u64(budget.returned_lines)),
        total_bytes: Some(to_u64(budget.total_bytes)),
        total_lines: Some(to_u64(budget.total_lines)),
        details,
        ..ToolResultMeta::default()
    };
    (budget.content, metadata)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TextBudgetResult {
    pub(super) content: String,
    pub(super) truncated: bool,
    pub(super) total_bytes: usize,
    pub(super) total_lines: usize,
    /// Source bytes retained before secret-safe rendering; generated UI markers are never counted.
    pub(super) returned_bytes: usize,
    /// Source lines retained before secret-safe rendering.
    pub(super) returned_lines: usize,
    pub(super) omitted_bytes: usize,
}

pub(super) fn truncate_text_budget(
    text: &str,
    max_bytes: usize,
    max_lines: usize,
) -> TextBudgetResult {
    let total_bytes = text.len();
    let total_lines = text_line_count(text);
    let mut returned = String::new();
    let mut returned_lines = 0usize;

    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index >= max_lines {
            break;
        }
        if returned.len().saturating_add(line.len()) > max_bytes {
            let remaining = max_bytes.saturating_sub(returned.len());
            append_utf8_prefix(&mut returned, line, remaining);
            break;
        }
        returned.push_str(line);
        returned_lines = returned_lines.saturating_add(1);
    }

    let returned_bytes = returned.len();
    let truncated = returned_bytes < total_bytes || returned_lines < total_lines;
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
    let byte_count = json_wire_bytes(value);
    match value {
        Value::Object(object) => {
            let mut keys = Vec::with_capacity(MCP_EGRESS_FIELD_LIMIT.min(object.len()));
            let mut field_types = serde_json::Map::new();
            let mut retained_key_bytes = 0usize;
            for (key, nested) in object {
                if keys.len() >= MCP_EGRESS_FIELD_LIMIT
                    || key.len() > MCP_EGRESS_KEY_LIMIT_BYTES
                    || retained_key_bytes.saturating_add(key.len()) > MCP_EGRESS_KEYS_TOTAL_BYTES
                {
                    continue;
                }
                retained_key_bytes = retained_key_bytes.saturating_add(key.len());
                keys.push(key.clone());
                field_types.insert(key.clone(), Value::String(json_type_label(nested)));
            }
            let omitted_fields = object.len().saturating_sub(keys.len());
            json!({
                "type": "object",
                "byte_count": byte_count,
                "top_level_key_count": object.len(),
                "top_level_keys": keys,
                "field_types": field_types,
                "omitted_top_level_keys": omitted_fields,
                "truncated": omitted_fields > 0,
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

pub(super) fn bounded_mcp_identity_projection(
    secret_redactor: &SecretRedactor,
    identity: &McpServerObservedIdentity,
) -> Value {
    let mut projection = json!({
        "environment_grant_source": "parent_environment",
    });
    for (key, value) in [
        (
            "transport_fingerprint",
            identity.transport_fingerprint.as_str(),
        ),
        (
            "process_authorization_fingerprint",
            identity.process_authorization_fingerprint.as_str(),
        ),
        (
            "environment_static_fingerprint",
            identity.environment_static_fingerprint.as_str(),
        ),
        (
            "environment_live_fingerprint",
            identity.environment_live_fingerprint.as_str(),
        ),
        ("protocol_version", identity.protocol_version.as_str()),
        ("server_name", identity.server_name.as_str()),
        ("server_version", identity.server_version.as_str()),
    ] {
        let bounded = bounded_mcp_metadata_text(secret_redactor, value);
        projection[key] = Value::String(bounded.value.clone());
        add_bounded_text_evidence(&mut projection, key, &bounded);
    }

    if let Some(declaration) = &identity.declaration {
        projection["declaration"] = json!({
            "declared_name": declaration.declared_name,
            "effective_name": declaration.effective_name,
            "origin_kind": declaration.origin_kind,
            "origin_id": declaration.origin_id,
            "execution_base_kind": declaration.execution_base_kind,
            "manifest_hash": declaration.manifest_hash,
            "manifest_version": declaration.manifest_version,
            "capability_digest": declaration.capability_digest,
            "release_digest": declaration.release_digest,
            "trust": declaration.trust,
            "projection_fingerprint": declaration.projection_fingerprint,
            "authorization_fingerprint": declaration.authorization_fingerprint,
        });
    }

    let mut names = Vec::new();
    let mut retained_bytes = 0usize;
    for name in identity
        .environment_grant_names
        .iter()
        .take(MCP_METADATA_GRANT_NAMES_LIMIT)
    {
        if names.len() >= MCP_METADATA_GRANT_NAMES_LIMIT
            || name.len() > MCP_METADATA_GRANT_NAME_LIMIT_BYTES
        {
            continue;
        }
        let bounded = bounded_metadata_text_with_limit(
            secret_redactor,
            name,
            MCP_METADATA_GRANT_NAME_LIMIT_BYTES,
        );
        if bounded.omitted
            || retained_bytes.saturating_add(bounded.value.len())
                > MCP_METADATA_GRANT_NAMES_TOTAL_BYTES
        {
            continue;
        }
        retained_bytes = retained_bytes.saturating_add(bounded.value.len());
        names.push(bounded.value);
    }
    projection["environment_grant_names"] = json!(names);
    projection["environment_grant_name_count"] = json!(identity.environment_grant_names.len());
    projection["environment_grant_names_omitted"] = json!(
        identity.environment_grant_names.len().saturating_sub(
            projection["environment_grant_names"]
                .as_array()
                .map_or(0, Vec::len)
        )
    );
    secret_safe_mcp_metadata(secret_redactor, projection)
}

pub(super) fn bounded_mcp_metadata_text(
    secret_redactor: &SecretRedactor,
    text: &str,
) -> BoundedMetadataText {
    bounded_metadata_text_with_limit(secret_redactor, text, MCP_METADATA_TEXT_LIMIT_BYTES)
}

pub(super) fn bounded_mcp_destination(
    secret_redactor: &SecretRedactor,
    server_name: &str,
) -> String {
    let server = bounded_mcp_metadata_text(secret_redactor, server_name);
    if server.omitted {
        return String::new();
    }
    let destination = format!("mcp:{}", server.value);
    let redacted = secret_redactor.redact_text(&destination);
    if redacted.len() <= MCP_METADATA_TEXT_LIMIT_BYTES {
        redacted
    } else {
        String::new()
    }
}

pub(super) fn secret_safe_mcp_metadata(secret_redactor: &SecretRedactor, value: Value) -> Value {
    let Ok(encoded) = serde_json::to_string(&value) else {
        return Value::Null;
    };
    if secret_redactor.redact_text(&encoded) == encoded {
        value
    } else {
        // A short explicit carrier may occur in a schema key, JSON delimiter, or fixed local
        // label. Returning no metadata is safer than adding another conflicting marker.
        Value::Null
    }
}

#[derive(Debug, Clone)]
pub(super) struct BoundedMetadataText {
    pub(super) value: String,
    total_bytes: usize,
    omitted: bool,
}

fn bounded_metadata_text_with_limit(
    secret_redactor: &SecretRedactor,
    text: &str,
    limit: usize,
) -> BoundedMetadataText {
    if text.len() > limit {
        return BoundedMetadataText {
            value: String::new(),
            total_bytes: text.len(),
            omitted: true,
        };
    }
    let value = secret_redactor.redact_text(text);
    let omitted = value.len() > limit;
    BoundedMetadataText {
        value: if omitted { String::new() } else { value },
        total_bytes: text.len(),
        omitted,
    }
}

fn add_bounded_text_evidence(target: &mut Value, key: &str, text: &BoundedMetadataText) {
    if text.omitted {
        target[format!("{key}_omitted")] = Value::Bool(true);
        target[format!("{key}_total_bytes")] = json!(text.total_bytes);
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

fn text_line_count(text: &str) -> usize {
    text.lines().count().max(usize::from(!text.is_empty()))
}

struct BoundedSourceCollector {
    retained: Vec<u8>,
    max_bytes: usize,
    max_lines: usize,
    retained_newlines: usize,
    retaining: bool,
    total_bytes: usize,
    total_newlines: usize,
    last_byte: Option<u8>,
}

impl BoundedSourceCollector {
    fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            retained: Vec::with_capacity(max_bytes),
            max_bytes,
            max_lines,
            retained_newlines: 0,
            retaining: max_bytes > 0 && max_lines > 0,
            total_bytes: 0,
            total_newlines: 0,
            last_byte: None,
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.total_newlines = self
            .total_newlines
            .saturating_add(bytes.iter().filter(|byte| **byte == b'\n').count());
        self.last_byte = bytes.last().copied().or(self.last_byte);
        if !self.retaining {
            return;
        }

        for byte in bytes {
            if self.retained.len() >= self.max_bytes {
                self.retaining = false;
                break;
            }
            self.retained.push(*byte);
            if *byte == b'\n' {
                self.retained_newlines = self.retained_newlines.saturating_add(1);
                if self.retained_newlines >= self.max_lines {
                    self.retaining = false;
                    break;
                }
            }
        }
    }

    fn finish(mut self, secret_redactor: &SecretRedactor) -> TextBudgetResult {
        let valid_len = match std::str::from_utf8(&self.retained) {
            Ok(_) => self.retained.len(),
            Err(error) => error.valid_up_to(),
        };
        self.retained.truncate(valid_len);
        let retained = std::str::from_utf8(&self.retained).unwrap_or_default();
        let total_lines = if self.total_bytes == 0 {
            0
        } else {
            self.total_newlines
                .saturating_add(usize::from(self.last_byte != Some(b'\n')))
        };
        let returned_lines = text_line_count(retained);
        let source_truncated = valid_len < self.total_bytes || returned_lines < total_lines;
        let redacted = if source_truncated {
            secret_redactor.redact_truncated_bytes(&self.retained)
        } else {
            secret_redactor.redact_text(retained)
        };
        let rendered = truncate_text_budget(&redacted, self.max_bytes, self.max_lines);
        TextBudgetResult {
            content: rendered.content,
            truncated: source_truncated || rendered.truncated,
            total_bytes: self.total_bytes,
            total_lines,
            returned_bytes: valid_len,
            returned_lines,
            omitted_bytes: self.total_bytes.saturating_sub(valid_len),
        }
    }
}

impl Write for BoundedSourceCollector {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.observe(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
