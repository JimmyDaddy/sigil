use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum McpResourceToolKind {
    List,
    Read,
}

impl McpResourceToolKind {
    pub(super) fn all() -> [Self; 2] {
        [Self::List, Self::Read]
    }

    pub(super) fn provider_suffix(self) -> &'static str {
        match self {
            Self::List => "resources_list",
            Self::Read => "resources_read",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Self::List => "List read-only MCP resources exposed by this server",
            Self::Read => "Read one MCP resource by URI",
        }
    }

    pub(super) fn input_schema(self) -> Value {
        match self {
            Self::List => json!({
                "type": "object",
                "properties": {
                    "cursor": {
                        "type": "string",
                        "description": "Optional pagination cursor from a previous resources/list response"
                    }
                },
                "additionalProperties": false
            }),
            Self::Read => json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "MCP resource URI returned by resources/list"
                    }
                },
                "required": ["uri"],
                "additionalProperties": false
            }),
        }
    }

    fn method(self) -> &'static str {
        match self {
            Self::List => "resources/list",
            Self::Read => "resources/read",
        }
    }

    fn request_params(self, args: &Value) -> std::result::Result<Value, String> {
        match self {
            Self::List => {
                let Some(object) = args.as_object() else {
                    return Err("MCP resources/list arguments must be an object".to_owned());
                };
                let mut params = serde_json::Map::new();
                if let Some(cursor) = object.get("cursor") {
                    let Some(cursor) = cursor.as_str() else {
                        return Err("MCP resources/list cursor must be a string".to_owned());
                    };
                    params.insert("cursor".to_owned(), Value::String(cursor.to_owned()));
                }
                Ok(Value::Object(params))
            }
            Self::Read => {
                let Some(object) = args.as_object() else {
                    return Err("MCP resources/read arguments must be an object".to_owned());
                };
                let Some(uri) = object.get("uri").and_then(Value::as_str) else {
                    return Err("MCP resources/read requires a uri string".to_owned());
                };
                if uri.trim().is_empty() {
                    return Err("MCP resources/read uri must not be empty".to_owned());
                }
                Ok(json!({ "uri": uri }))
            }
        }
    }
}

pub(super) struct McpResourceTool {
    pub(super) client: Arc<McpClient>,
    pub(super) spec: ToolSpec,
    pub(super) tool_name: McpToolName,
    pub(super) kind: McpResourceToolKind,
    pub(super) trust: McpServerTrustPolicy,
    pub(super) secret_redactor: SecretRedactor,
}

#[async_trait]
impl Tool for McpResourceTool {
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
                "resource_operation": self.kind.provider_suffix(),
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
                "MCP resource arguments contain a secret and this server has allow_secrets = false",
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
            "resource",
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
