use super::*;

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
        })
    }
}

impl McpRemoteTool {
    pub(super) fn validate(&self) -> Result<(), McpStreamableHttpError> {
        if self.name.is_empty() || self.name.len() > 256 {
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

impl McpCallToolResult {
    pub(super) fn parse(value: &Value) -> Result<Self, McpStreamableHttpError> {
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
}
