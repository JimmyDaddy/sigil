use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use termquill_kernel::{
    McpServerConfig, McpServerStartup, ProviderCapabilities, Tool, ToolAccess, ToolCategory,
    ToolContext, ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec, ToolSubject,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tracing::warn;

const DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS: usize = 64;

pub async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
) -> Result<()> {
    register_mcp_tools_with_name_limit(registry, servers, DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS)
        .await
}

pub async fn register_mcp_tools_with_capabilities(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
) -> Result<()> {
    register_mcp_tools_with_name_limit(registry, servers, capabilities.tool_name_max_chars).await
}

pub async fn register_mcp_tools_with_capabilities_and_roots(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    capabilities: &ProviderCapabilities,
    roots: Vec<PathBuf>,
) -> Result<()> {
    register_mcp_tools_with_name_limit_and_roots(
        registry,
        servers,
        capabilities.tool_name_max_chars,
        roots,
    )
    .await
}

async fn register_mcp_tools_with_name_limit(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    provider_tool_name_max_chars: usize,
) -> Result<()> {
    register_mcp_tools_with_name_limit_and_roots(
        registry,
        servers,
        provider_tool_name_max_chars,
        default_mcp_roots()?,
    )
    .await
}

async fn register_mcp_tools_with_name_limit_and_roots(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    provider_tool_name_max_chars: usize,
    roots: Vec<PathBuf>,
) -> Result<()> {
    let mut used_provider_names = BTreeSet::new();
    for server in servers {
        if server.startup == McpServerStartup::Lazy {
            if server.required {
                bail!(
                    "MCP server {} is required but lazy startup is not implemented yet",
                    server.name
                );
            }
            warn!(
                server = %server.name,
                trust_class = server.trust.trust_class.as_str(),
                "lazy MCP server startup is configured but lazy activation is not implemented yet"
            );
            continue;
        }

        let client = match McpClient::spawn(server.clone(), roots.clone()).await {
            Ok(client) => Arc::new(client),
            Err(error) if !server.required => {
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    error = %error,
                    "optional MCP server failed to start and will be skipped"
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        let tools = match client.list_tools().await {
            Ok(tools) => tools,
            Err(error) if !server.required => {
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    error = %error,
                    "optional MCP server tools/list failed and will be skipped"
                );
                continue;
            }
            Err(error) => {
                bail!("MCP server {} tools/list failed: {error:#}", server.name);
            }
        };
        for tool in tools {
            let tool_name = McpToolName::new(
                &server.name,
                &tool.name,
                provider_tool_name_max_chars,
                &mut used_provider_names,
            );
            registry.register(Arc::new(McpTool {
                client: Arc::clone(&client),
                spec: ToolSpec {
                    name: tool_name.provider_name.clone(),
                    description: tool.description.unwrap_or_else(|| "MCP tool".to_owned()),
                    input_schema: tool.input_schema,
                    category: ToolCategory::Mcp,
                    access: ToolAccess::Network,
                    preview: ToolPreviewCapability::None,
                },
                tool_name,
            }));
        }
    }
    Ok(())
}

fn default_mcp_roots() -> Result<Vec<PathBuf>> {
    let cwd =
        std::env::current_dir().context("failed to resolve current directory for MCP roots")?;
    Ok(vec![canonical_root(cwd)])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolName {
    pub provider_name: String,
    pub server_name: String,
    pub original_name: String,
}

impl McpToolName {
    fn new(
        server_name: &str,
        original_name: &str,
        max_provider_name_chars: usize,
        used_provider_names: &mut BTreeSet<String>,
    ) -> Self {
        let base = format!(
            "mcp__{}__{}",
            sanitize_provider_name_part(server_name),
            sanitize_provider_name_part(original_name)
        );
        let identity = format!("{server_name}\0{original_name}");
        let mut provider_name =
            fit_provider_name_with_hash(&base, &identity, max_provider_name_chars);
        let mut attempt = 0usize;
        while used_provider_names.contains(&provider_name) {
            attempt += 1;
            provider_name = provider_name_with_hash(
                &base,
                &format!("{identity}\0{attempt}"),
                max_provider_name_chars,
            );
        }
        used_provider_names.insert(provider_name.clone());

        Self {
            provider_name,
            server_name: server_name.to_owned(),
            original_name: original_name.to_owned(),
        }
    }
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
    roots: Vec<PathBuf>,
}

struct Connection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    async fn spawn(config: McpServerConfig, roots: Vec<PathBuf>) -> Result<Self> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
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
            roots,
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(config.startup_timeout_secs),
            client.initialize(),
        )
        .await
        .with_context(|| format!("MCP server {} initialize timed out", config.name))??;
        Ok(client)
    }

    async fn initialize(&self) -> Result<()> {
        let _ = self
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "roots": { "listChanged": true }
                    },
                    "clientInfo": { "name": "termquill", "version": "0.1.0" }
                }),
            )
            .await?;
        self.send_notification("notifications/initialized", json!({}))
            .await
    }

    async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
        let result = self.send_request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("MCP tools/list missing tools array"))?;
        tools
            .iter()
            .cloned()
            .map(serde_json::from_value::<McpToolDescriptor>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    async fn call_tool_response(&self, name: &str, args: Value) -> Result<Value> {
        self.send_request_response(
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
        let response = self.send_request_response(method, params).await?;
        if let Some(error) = response.get("error") {
            bail!("MCP request {} failed: {}", method, error);
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response missing result"))
    }

    async fn send_request_response(&self, method: &str, params: Value) -> Result<Value> {
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
                self.handle_inbound_message(&mut connection, &response)
                    .await?;
                continue;
            }
            return Ok(response);
        }
    }

    async fn handle_inbound_message(
        &self,
        connection: &mut Connection,
        message: &Value,
    ) -> Result<()> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(());
        };
        if method == "notifications/progress" {
            return Ok(());
        }
        let Some(id) = message.get("id").cloned() else {
            return Ok(());
        };

        match method {
            "roots/list" => {
                let roots = self
                    .roots
                    .iter()
                    .map(|root| {
                        json!({
                            "uri": file_uri(root),
                            "name": root_name(root),
                        })
                    })
                    .collect::<Vec<_>>();
                write_success_response(connection, id, json!({ "roots": roots })).await
            }
            "elicitation/create" => {
                write_error_response(
                    connection,
                    id,
                    -32601,
                    "MCP elicitation is not supported by termquill yet",
                )
                .await
            }
            _ => {
                write_error_response(
                    connection,
                    id,
                    -32601,
                    format!("MCP client method is not supported: {method}"),
                )
                .await
            }
        }
    }
}

struct McpTool {
    client: Arc<McpClient>,
    spec: ToolSpec,
    tool_name: McpToolName,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![ToolSubject::mcp_tool(self.spec.name.clone())])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let response = self
            .client
            .call_tool_response(&self.tool_name.original_name, args)
            .await?;
        if let Some(error) = response.get("error") {
            return Ok(ToolResult::error(
                call_id,
                self.spec.name.clone(),
                ToolErrorKind::Protocol,
                format!("MCP tools/call failed: {error}"),
            )
            .with_error_details(false, error.clone()));
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
        Ok(ToolResult::ok(
            call_id,
            self.spec.name.clone(),
            content,
            ToolResultMeta::default(),
        ))
    }
}

fn sanitize_provider_name_part(value: &str) -> String {
    let mut sanitized = String::new();
    let mut previous_underscore = false;
    for ch in value.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '_';
        if safe {
            sanitized.push(ch);
            previous_underscore = false;
        } else if !previous_underscore {
            sanitized.push('_');
            previous_underscore = true;
        }
    }
    let trimmed = sanitized.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "tool".to_owned()
    } else {
        trimmed
    }
}

fn fit_provider_name_with_hash(base: &str, identity: &str, max_chars: usize) -> String {
    if base.len() <= max_chars {
        return base.to_owned();
    }
    provider_name_with_hash(base, identity, max_chars)
}

fn provider_name_with_hash(base: &str, identity: &str, max_chars: usize) -> String {
    let suffix = format!("__{:08x}", stable_hash(identity));
    let prefix_len = max_chars.saturating_sub(suffix.len()).max(1);
    let mut output = base.chars().take(prefix_len).collect::<String>();
    output.push_str(&suffix);
    output
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn canonical_root(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn root_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_owned()
}

fn file_uri(path: &std::path::Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let normalized = absolute.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{}", percent_encode_uri_path(&normalized))
    } else {
        format!("file:///{}", percent_encode_uri_path(&normalized))
    }
}

fn percent_encode_uri_path(path: &str) -> String {
    let mut output = String::new();
    for byte in path.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~');
        if keep {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

async fn write_success_response(
    connection: &mut Connection,
    id: Value,
    result: Value,
) -> Result<()> {
    write_message(
        &mut connection.stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
    )
    .await
}

async fn write_error_response(
    connection: &mut Connection,
    id: Value,
    code: i64,
    message: impl Into<String>,
) -> Result<()> {
    write_message(
        &mut connection.stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message.into(),
            },
        }),
    )
    .await
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
