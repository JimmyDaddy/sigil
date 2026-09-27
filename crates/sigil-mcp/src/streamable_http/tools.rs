use super::*;
use crate::McpToolAnnotations;
use crate::output::{
    attach_mcp_artifact, bounded_mcp_json, bounded_mcp_text_segments,
    bounded_mcp_tool_result_with_identity, capture_mcp_result_artifact,
};
use anyhow::Result;
use sigil_kernel::{McpServerTrustPolicy, SecretRedactor, ToolContext, ToolErrorKind, ToolResult};

/// Safe reason why one advertised tool was excluded from a completed discovery pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRemoteToolDiagnosticKind {
    MalformedDescriptor,
    UnsupportedContract,
}

/// A discovery diagnostic identifies the descriptor by its zero-based position across pages.
/// It retains no raw schema, credentials, or server-controlled diagnostic text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteToolDiagnostic {
    pub tool_index: usize,
    pub kind: McpRemoteToolDiagnosticKind,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct McpRemoteTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default, rename = "outputSchema")]
    pub output_schema: Option<Value>,
    #[serde(default, rename = "taskSupport")]
    pub task_support: Option<String>,
    #[serde(default, skip_serializing_if = "McpToolAnnotations::is_empty")]
    pub annotations: McpToolAnnotations,
}

impl<'de> Deserialize<'de> for McpRemoteTool {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Execution {
            #[serde(default, rename = "taskSupport")]
            task_support: Option<String>,
        }

        #[derive(Deserialize)]
        struct WireTool {
            name: String,
            #[serde(default)]
            description: Option<String>,
            #[serde(rename = "inputSchema")]
            input_schema: Value,
            #[serde(default, rename = "outputSchema")]
            output_schema: Option<Value>,
            #[serde(default, rename = "taskSupport")]
            task_support: Option<String>,
            #[serde(default)]
            execution: Option<Execution>,
            #[serde(default)]
            annotations: McpToolAnnotations,
        }

        let wire = WireTool::deserialize(deserializer)?;
        let execution_task = wire.execution.and_then(|execution| execution.task_support);
        let task_support = match (wire.task_support, execution_task) {
            (Some(left), Some(right)) if left != right => Some("__conflict__".to_owned()),
            (Some(value), _) | (_, Some(value)) => Some(value),
            (None, None) => None,
        };
        Ok(Self {
            name: wire.name,
            description: wire.description,
            input_schema: wire.input_schema,
            output_schema: wire.output_schema,
            task_support,
            annotations: wire.annotations,
        })
    }
}

impl McpRemoteTool {
    pub(super) fn validate(&self) -> Result<(), McpStreamableHttpError> {
        if self.name.is_empty()
            || self.name.len() > 256
            || !self.input_schema.is_object()
            || self
                .output_schema
                .as_ref()
                .is_some_and(|schema| !schema.is_object())
        {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        if self
            .task_support
            .as_deref()
            .is_some_and(|value| !matches!(value, "optional" | "forbidden"))
        {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        CompiledMcpSchema::compile(&self.input_schema)?;
        if let Some(schema) = self.output_schema.as_ref() {
            CompiledMcpSchema::compile(schema)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpCallToolResult {
    pub content: Vec<Value>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

/// Inputs owned by a concrete MCP tool adapter when it turns a validated remote result into the
/// kernel's transport-neutral [`ToolResult`]. The wire result and its redaction/artifact policy
/// remain in this crate so stdio and Streamable HTTP cannot drift in content or `isError` semantics.
pub struct McpCallToolResultContext<'a> {
    pub call_id: &'a str,
    pub provider_tool_name: &'a str,
    pub server_name: &'a str,
    pub remote_tool_name: &'a str,
    pub trust: &'a McpServerTrustPolicy,
    pub server_identity: Value,
    pub redactor: &'a SecretRedactor,
    pub tool_context: &'a ToolContext,
    pub surface_kind: &'a str,
    pub operation: &'a str,
}

impl McpCallToolResult {
    pub(crate) fn parse(value: &Value) -> Result<Self, McpStreamableHttpError> {
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .ok_or(McpStreamableHttpError::MissingRequiredContent)?;
        if serde_json::to_vec(&content)
            .map_err(|_| McpStreamableHttpError::MalformedEnvelope)?
            .len()
            > 8 * 1024 * 1024
        {
            return Err(McpStreamableHttpError::BodyLimitExceeded);
        }
        for block in &content {
            let block = block
                .as_object()
                .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
            let block_type = block
                .get("type")
                .and_then(Value::as_str)
                .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
            if !matches!(
                block_type,
                "text" | "image" | "audio" | "resource" | "resource_link"
            ) {
                return Err(McpStreamableHttpError::MalformedEnvelope);
            }
        }
        if value
            .get("isError")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        }
        Ok(Self {
            content,
            structured_content: value.get("structuredContent").cloned(),
            is_error: value
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// Parses the legacy stdio result shape that used a plain string for `content`.
    ///
    /// Streamable HTTP remains strict to the current MCP content-block contract, while the
    /// stdio adapter keeps compatibility with older local servers that returned a text scalar.
    pub(crate) fn parse_stdio(value: &Value) -> Result<Self, McpStreamableHttpError> {
        match Self::parse(value) {
            Ok(result) => Ok(result),
            Err(McpStreamableHttpError::MissingRequiredContent)
                if value.get("content").and_then(Value::as_str).is_some() =>
            {
                let mut normalized = value.clone();
                normalized["content"] = Value::Array(vec![serde_json::json!({
                    "type": "text",
                    "text": value.get("content").and_then(Value::as_str).unwrap_or_default(),
                })]);
                Self::parse(&normalized)
            }
            Err(error) => Err(error),
        }
    }

    /// Converts one validated MCP result using the shared bounded, redacted output contract.
    pub fn into_tool_result(
        &self,
        raw_result: &Value,
        context: McpCallToolResultContext<'_>,
    ) -> Result<ToolResult> {
        let artifact = capture_mcp_result_artifact(
            context.tool_context,
            context.call_id,
            context.provider_tool_name,
            context.redactor,
            raw_result,
        );
        let budget = if self
            .content
            .iter()
            .any(|item| item.get("text").and_then(Value::as_str).is_some())
        {
            bounded_mcp_text_segments(
                context.redactor,
                self.content
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str)),
                "\n",
            )
        } else {
            bounded_mcp_json(context.redactor, raw_result)?
        };
        let (content, metadata) = bounded_mcp_tool_result_with_identity(
            context.redactor,
            context.server_name,
            context.remote_tool_name,
            context.trust,
            context.server_identity,
            context.surface_kind,
            context.operation,
            budget,
        );
        let result = if self.is_error {
            ToolResult::error(
                context.call_id,
                context.provider_tool_name,
                ToolErrorKind::Protocol,
                content,
            )
            .with_error_details(false, metadata.details)
        } else {
            ToolResult::ok(
                context.call_id,
                context.provider_tool_name,
                content,
                metadata,
            )
        };
        Ok(attach_mcp_artifact(result, artifact))
    }
}
