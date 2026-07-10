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
}

#[async_trait]
impl Tool for McpPromptTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn shutdown(&self) -> Result<()> {
        self.client
            .shutdown_generation("MCP prompt generation was retired".to_owned())
            .await
    }

    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        Some(self.client.lifecycle_owner())
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![
            ToolSubject::mcp_tool(self.spec.name.clone()),
            self.client.identity().trust_subject(
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
        let secret_detected = self.client.secret_redactor.value_contains_secret(args);
        let argument_summary = self
            .client
            .secret_redactor
            .redact_value(&summarize_egress_json(args));
        let server =
            bounded_mcp_metadata_text(&self.client.secret_redactor, &self.tool_name.server_name);
        let provider_tool =
            bounded_mcp_metadata_text(&self.client.secret_redactor, &self.spec.name);
        let payload = secret_safe_mcp_metadata(
            &self.client.secret_redactor,
            json!({
                "server": server.value,
                "trust_class": self.trust.trust_class.as_str(),
                "provider_tool": provider_tool.value,
                "prompt_operation": self.kind.provider_suffix(),
                "allow_secrets": self.trust.allow_secrets,
                "secret_detected": secret_detected,
                "server_identity": bounded_mcp_identity_projection(
                    &self.client.secret_redactor,
                    self.client.identity(),
                ),
                "arguments": argument_summary,
            }),
        );
        Ok(Some(ToolEgressAudit {
            destination: bounded_mcp_destination(
                &self.client.secret_redactor,
                &self.tool_name.server_name,
            ),
            operation: self.kind.method().to_owned(),
            payload,
            redacted: secret_detected,
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let _transport_effect = ctx.begin_forward_effect(sigil_kernel::RunEffectKind::Socket)?;
        if !self.trust.allow_secrets && self.client.secret_redactor.value_contains_secret(&args) {
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
        let cancellation = ctx.cancellation_handle();
        let operation = {
            let operation = self.client.send_request_response(
                self.kind.method(),
                params,
                McpOperationDeadline::from_secs(ctx.timeout_secs),
            );
            tokio::pin!(operation);
            match cancellation.as_ref() {
                Some(cancellation) => tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => None,
                    response = &mut operation => Some(response),
                },
                None => Some(operation.await),
            }
        };
        let Some(operation) = operation else {
            let cleanup = self
                .client
                .close_connection("MCP prompt request interrupted by run cancellation".to_owned())
                .await;
            if !cleanup.completed
                && let Some(cancellation) = cancellation
            {
                cancellation.mark_cleanup_incomplete();
            }
            return Ok(ToolResult::error(
                call_id,
                self.spec.name.clone(),
                ToolErrorKind::Interrupted,
                "MCP prompt request interrupted by run cancellation",
            ));
        };
        let response = match operation {
            Ok(response) => response,
            Err(error) => {
                return Ok(error.to_tool_result(
                    call_id,
                    self.spec.name.clone(),
                    &self.client.server_name,
                ));
            }
        };
        if let Some(error) = response.get("error") {
            let projection = bounded_mcp_protocol_error(
                &self.client.secret_redactor,
                error,
                &format!("MCP {} failed", self.kind.method()),
            );
            return Ok(ToolResult::error(
                call_id,
                self.spec.name.clone(),
                ToolErrorKind::Protocol,
                projection.summary,
            )
            .with_error_details(false, projection.details));
        }
        let result = response
            .get("result")
            .ok_or_else(|| anyhow!("MCP response missing result"))?;
        let budget = bounded_mcp_json(&self.client.secret_redactor, result)?;
        let (content, metadata) = bounded_mcp_tool_result(
            &self.client.secret_redactor,
            &self.tool_name,
            &self.trust,
            self.client.identity(),
            "prompt",
            self.kind.method(),
            budget,
        );
        Ok(ToolResult::ok(
            call_id,
            self.spec.name.clone(),
            content,
            metadata,
        ))
    }
}
