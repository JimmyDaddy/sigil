use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct McpToolDescriptor {
    pub(super) name: String,
    pub(super) description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub(super) input_schema: Value,
}

pub(super) struct McpTool {
    pub(super) client: Arc<McpClient>,
    pub(super) spec: ToolSpec,
    pub(super) tool_name: McpToolName,
    pub(super) trust: McpServerTrustPolicy,
    pub(super) secret_redactor: SecretRedactor,
}

#[async_trait]
impl Tool for McpTool {
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
            operation: "tools/call".to_owned(),
            payload: json!({
                "server": self.tool_name.server_name,
                "trust_class": self.trust.trust_class.as_str(),
                "provider_tool": self.spec.name,
                "remote_tool": self.tool_name.original_name,
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
                "MCP tool arguments contain a secret and this server has allow_secrets = false",
            ));
        }
        let response = self
            .client
            .call_tool_response(&self.tool_name.original_name, args)
            .await?;
        if let Some(error) = response.get("error") {
            let redacted_error = self.secret_redactor.redact_value(error);
            return Ok(ToolResult::error(
                call_id,
                self.spec.name.clone(),
                ToolErrorKind::Protocol,
                format!("MCP tools/call failed: {redacted_error}"),
            )
            .with_error_details(false, redacted_error));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response missing result"))?;
        let content = match result.get("content") {
            Some(Value::Array(items)) => {
                let text_items = items
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>();
                if text_items.is_empty() {
                    serde_json::to_string_pretty(&result)?
                } else {
                    text_items.join("\n")
                }
            }
            Some(Value::String(value)) => value.clone(),
            _ => serde_json::to_string_pretty(&result)?,
        };
        let (content, metadata) = bounded_mcp_tool_result(
            &self.secret_redactor,
            &self.tool_name,
            &self.trust,
            &self.client.identity,
            "tool",
            "tools/call",
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

pub(super) fn mcp_command_fingerprint(command: &str, args: &[String]) -> Result<String> {
    let encoded = serde_json::to_vec(&json!({
        "command": command,
        "args": args,
    }))
    .context("failed to serialize MCP command fingerprint material")?;
    Ok(format!("sha256:{:x}", Sha256::digest(&encoded)))
}
