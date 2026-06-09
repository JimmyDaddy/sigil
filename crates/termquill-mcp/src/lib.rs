use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use termquill_kernel::{
    McpServerConfig, Tool, ToolContext, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

pub async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
) -> Result<()> {
    for server in servers {
        let client = Arc::new(McpClient::spawn(server.clone()).await?);
        let tools = client.list_tools().await?;
        for tool in tools {
            registry.register(Arc::new(McpTool {
                client: Arc::clone(&client),
                spec: tool,
            }));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpToolDescriptor {
    name: String,
    description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    input_schema: Value,
}

struct McpClient {
    _child: Mutex<Child>,
    connection: Mutex<Connection>,
}

struct Connection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    async fn spawn(config: McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to spawn MCP server {}", config.name))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("missing stdin for MCP server {}", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing stdout for MCP server {}", config.name))?;

        let client = Self {
            _child: Mutex::new(child),
            connection: Mutex::new(Connection {
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 0,
            }),
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(config.startup_timeout_secs),
            client.initialize(),
        )
        .await
        .context("MCP initialize timed out")??;
        Ok(client)
    }

    async fn initialize(&self) -> Result<()> {
        let _ = self
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "termquill", "version": "0.1.0" }
                }),
            )
            .await?;
        self.send_notification("notifications/initialized", json!({}))
            .await
    }

    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let result = self.send_request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("MCP tools/list missing tools array"))?;
        tools
            .iter()
            .cloned()
            .map(serde_json::from_value::<McpToolDescriptor>)
            .map(|item| {
                item.map(|tool| ToolSpec {
                    name: tool.name,
                    description: tool.description.unwrap_or_else(|| "MCP tool".to_owned()),
                    input_schema: tool.input_schema,
                    read_only: false,
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        self.send_request(
            "tools/call",
            json!({
                "name": name,
                "arguments": args,
            }),
        )
        .await
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut connection = self.connection.lock().await;
        write_message(&mut connection.stdin, &message).await
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let mut connection = self.connection.lock().await;
        connection.next_id += 1;
        let id = connection.next_id;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        write_message(&mut connection.stdin, &message).await?;
        loop {
            let response = read_message(&mut connection.stdout).await?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                bail!("MCP request {} failed: {}", method, error);
            }
            return response
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow!("MCP response missing result"));
        }
    }
}

struct McpTool {
    client: Arc<McpClient>,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let result = self.client.call_tool(&self.spec.name, args).await?;
        let content = match result.get("content") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            Some(Value::String(value)) => value.clone(),
            _ => serde_json::to_string_pretty(&result)?,
        };
        Ok(ToolResult {
            call_id,
            tool_name: self.spec.name.clone(),
            content,
            is_error: false,
            metadata: ToolResultMeta::default(),
        })
    }
}

async fn write_message(stdin: &mut ChildStdin, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut content_length = None::<usize>;
    loop {
        let mut line = String::new();
        let bytes = stdout.read_line(&mut line).await?;
        if bytes == 0 {
            bail!("MCP server closed stdout");
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let normalized = line.trim();
        if let Some(value) = normalized.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse()?);
        }
    }
    let length = content_length.ok_or_else(|| anyhow!("missing Content-Length header"))?;
    let mut body = vec![0u8; length];
    stdout.read_exact(&mut body).await?;
    serde_json::from_slice(&body).context("invalid MCP JSON")
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
