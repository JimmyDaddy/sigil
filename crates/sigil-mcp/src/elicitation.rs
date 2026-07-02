use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct McpElicitationRequest {
    pub server_name: String,
    pub message: String,
    pub requested_schema: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

impl McpElicitationAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpElicitationResponse {
    pub action: McpElicitationAction,
    pub content: Option<Value>,
}

impl McpElicitationResponse {
    pub fn accept(content: Value) -> Self {
        Self {
            action: McpElicitationAction::Accept,
            content: Some(content),
        }
    }

    pub fn decline() -> Self {
        Self {
            action: McpElicitationAction::Decline,
            content: None,
        }
    }

    pub fn cancel() -> Self {
        Self {
            action: McpElicitationAction::Cancel,
            content: None,
        }
    }

    pub(super) fn into_result(self) -> Value {
        match (self.action, self.content) {
            (McpElicitationAction::Accept, Some(content)) => {
                json!({ "action": self.action.as_str(), "content": content })
            }
            (McpElicitationAction::Accept, None) => {
                json!({ "action": self.action.as_str(), "content": {} })
            }
            (action, _) => json!({ "action": action.as_str() }),
        }
    }
}

#[async_trait]
pub trait McpElicitationHandler: Send + Sync {
    fn supports_elicitation(&self) -> bool {
        false
    }

    async fn elicit(&self, _request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        bail!("MCP elicitation is not supported by sigil yet")
    }
}

#[derive(Debug)]
pub(super) struct UnsupportedMcpElicitationHandler;

#[async_trait]
impl McpElicitationHandler for UnsupportedMcpElicitationHandler {}

pub fn unsupported_mcp_elicitation_handler() -> Arc<dyn McpElicitationHandler> {
    Arc::new(UnsupportedMcpElicitationHandler)
}

pub(super) fn mcp_elicitation_request(
    server_name: &str,
    message: &Value,
) -> Result<McpElicitationRequest> {
    let params = message
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("MCP elicitation/create missing params object"))?;
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("MCP server requested input")
        .to_owned();
    let requested_schema = params
        .get("requestedSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    Ok(McpElicitationRequest {
        server_name: server_name.to_owned(),
        message,
        requested_schema,
    })
}
