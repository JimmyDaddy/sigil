use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum McpPromptToolKind {
    List,
    Get,
}

impl McpPromptToolKind {
    pub(super) fn all() -> [Self; 2] {
        [Self::List, Self::Get]
    }

    pub(super) fn provider_suffix(self) -> &'static str {
        match self {
            Self::List => "prompts_list",
            Self::Get => "prompts_get",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::List => "List MCP prompts exposed by this server",
            Self::Get => "Get one MCP prompt by name with optional arguments",
        }
    }

    pub(super) fn input_schema(self) -> Value {
        match self {
            Self::List => json!({
                "type": "object",
                "properties": {
                    "cursor": {
                        "type": "string",
                        "description": "Optional pagination cursor from a previous prompts/list response"
                    }
                },
                "additionalProperties": false
            }),
            Self::Get => json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "MCP prompt name returned by prompts/list"
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Optional prompt arguments matching the prompt argument schema"
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        }
    }

    pub(super) fn method(self) -> &'static str {
        match self {
            Self::List => "prompts/list",
            Self::Get => "prompts/get",
        }
    }

    pub(super) fn request_params(self, args: &Value) -> std::result::Result<Value, String> {
        match self {
            Self::List => {
                let Some(object) = args.as_object() else {
                    return Err("MCP prompts/list arguments must be an object".to_owned());
                };
                let mut params = serde_json::Map::new();
                if let Some(cursor) = object.get("cursor") {
                    let Some(cursor) = cursor.as_str() else {
                        return Err("MCP prompts/list cursor must be a string".to_owned());
                    };
                    params.insert("cursor".to_owned(), Value::String(cursor.to_owned()));
                }
                Ok(Value::Object(params))
            }
            Self::Get => {
                let Some(object) = args.as_object() else {
                    return Err("MCP prompts/get arguments must be an object".to_owned());
                };
                let Some(name) = object.get("name").and_then(Value::as_str) else {
                    return Err("MCP prompts/get requires a name string".to_owned());
                };
                if name.trim().is_empty() {
                    return Err("MCP prompts/get name must not be empty".to_owned());
                }
                let mut params = serde_json::Map::new();
                params.insert("name".to_owned(), Value::String(name.to_owned()));
                if let Some(arguments) = object.get("arguments") {
                    if !arguments.is_object() {
                        return Err("MCP prompts/get arguments must be an object".to_owned());
                    }
                    params.insert("arguments".to_owned(), arguments.clone());
                }
                Ok(Value::Object(params))
            }
        }
    }
}

pub(super) struct McpPromptTool {
    pub(super) client: Arc<McpClient>,
    pub(super) spec: ToolSpec,
    pub(super) tool_name: McpToolName,
    pub(super) kind: McpPromptToolKind,
    pub(super) trust: McpServerTrustPolicy,
    pub(super) secret_redactor: SecretRedactor,
}

#[async_trait]
impl Tool for McpPromptTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![
            ToolSubject::mcp_tool(self.spec.name.clone()),
            ToolSubject::mcp_trust_class(
                self.tool_name.server_name.clone(),
                self.trust.trust_class.as_str(),
            ),
        ])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(self.trust.approval_default))
    }

    fn egress_audit(&self, _ctx: &ToolContext, args: &Value) -> Result<Option<ToolEgressAudit>> {
        if !self.trust.egress_logging {
            return Ok(None);
        }
        let secret_detected = self.secret_redactor.value_contains_secret(args);
        Ok(Some(ToolEgressAudit {
            destination: format!("mcp:{}", self.tool_name.server_name),
            operation: self.kind.method().to_owned(),
            payload: json!({
                "server": self.tool_name.server_name,
                "trust_class": self.trust.trust_class.as_str(),
                "provider_tool": self.spec.name,
                "prompt_operation": self.kind.provider_suffix(),
                "allow_secrets": self.trust.allow_secrets,
                "secret_detected": secret_detected,
                "server_identity": self.client.identity.to_json(),
                "arguments": summarize_egress_json(args),
            }),
            redacted: secret_detected,
        }))
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        if !self.trust.allow_secrets && self.secret_redactor.value_contains_secret(&args) {
            return Ok(ToolResult::error(
                call_id,
                self.spec.name.clone(),
                ToolErrorKind::PermissionDenied,
                "MCP prompt arguments contain a secret and this server has allow_secrets = false",
            ));
        }
        let params = match self.kind.request_params(&args) {
            Ok(params) => params,
            Err(message) => {
                return Ok(ToolResult::error(
                    call_id,
                    self.spec.name.clone(),
                    ToolErrorKind::InvalidInput,
                    message,
                ));
            }
        };
        let response = self
            .client
            .send_request_response(self.kind.method(), params)
            .await?;
        if let Some(error) = response.get("error") {
            let redacted_error = self.secret_redactor.redact_value(error);
            return Ok(ToolResult::error(
                call_id,
                self.spec.name.clone(),
                ToolErrorKind::Protocol,
                format!("MCP {} failed: {redacted_error}", self.kind.method()),
            )
            .with_error_details(false, redacted_error));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response missing result"))?;
        let content = serde_json::to_string_pretty(&result)?;
        let (content, metadata) = bounded_mcp_tool_result(
            &self.secret_redactor,
            &self.tool_name,
            &self.trust,
            &self.client.identity,
            "prompt",
            self.kind.method(),
            content,
        );
        Ok(ToolResult::ok(
            call_id,
            self.spec.name.clone(),
            content,
            metadata,
        ))
    }
}
