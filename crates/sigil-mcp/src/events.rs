use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct McpProgressNotification {
    pub server_name: String,
    pub progress_token: String,
    pub progress: Option<f64>,
    pub total: Option<f64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpListChangedKind {
    Tools,
    Resources,
    Prompts,
}

impl McpListChangedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Resources => "resources",
            Self::Prompts => "prompts",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpListChangedNotification {
    pub server_name: String,
    pub kind: McpListChangedKind,
}

#[async_trait]
pub trait McpRuntimeEventHandler: Send + Sync {
    async fn progress(&self, _notification: McpProgressNotification) -> Result<()> {
        Ok(())
    }

    async fn list_changed(&self, _notification: McpListChangedNotification) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct UnsupportedMcpRuntimeEventHandler;

#[async_trait]
impl McpRuntimeEventHandler for UnsupportedMcpRuntimeEventHandler {}

pub fn unsupported_mcp_runtime_event_handler() -> Arc<dyn McpRuntimeEventHandler> {
    Arc::new(UnsupportedMcpRuntimeEventHandler)
}

pub(super) fn mcp_progress_notification(
    server_name: &str,
    message: &Value,
) -> Option<McpProgressNotification> {
    let params = message.get("params").and_then(Value::as_object)?;
    let token = params.get("progressToken")?;
    let progress_token = match token {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        other => serde_json::to_string(other).ok()?,
    };
    Some(McpProgressNotification {
        server_name: server_name.to_owned(),
        progress_token,
        progress: params.get("progress").and_then(Value::as_f64),
        total: params.get("total").and_then(Value::as_f64),
        message: params
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

pub(super) fn mcp_list_changed_kind(method: &str) -> Option<McpListChangedKind> {
    match method {
        "notifications/tools/list_changed" => Some(McpListChangedKind::Tools),
        "notifications/resources/list_changed" => Some(McpListChangedKind::Resources),
        "notifications/prompts/list_changed" => Some(McpListChangedKind::Prompts),
        _ => None,
    }
}
