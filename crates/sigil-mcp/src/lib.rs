use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, McpServerConfig, McpServerPinnedIdentity, McpServerStartup, McpServerTrustPolicy,
    ProviderCapabilities, SecretRedactor, Tool, ToolAccess, ToolCategory, ToolContext,
    ToolEgressAudit, ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tracing::warn;

const DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS: usize = 64;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone)]
pub struct McpToolRegistrationOptions {
    pub provider_tool_name_max_chars: usize,
    pub roots: Vec<PathBuf>,
    pub secret_redactor: SecretRedactor,
    pub elicitation_handler: Arc<dyn McpElicitationHandler>,
    pub startup: McpServerStartup,
}

impl McpToolRegistrationOptions {
    pub fn eager() -> Result<Self> {
        Self::for_startup(McpServerStartup::Eager)
    }

    pub fn lazy() -> Result<Self> {
        Self::for_startup(McpServerStartup::Lazy)
    }

    pub fn for_startup(startup: McpServerStartup) -> Result<Self> {
        Ok(Self {
            provider_tool_name_max_chars: DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS,
            roots: default_mcp_roots()?,
            secret_redactor: SecretRedactor::empty(),
            elicitation_handler: unsupported_mcp_elicitation_handler(),
            startup,
        })
    }

    pub fn with_capabilities(mut self, capabilities: &ProviderCapabilities) -> Self {
        self.provider_tool_name_max_chars = capabilities.tool_name_max_chars;
        self
    }

    pub fn with_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.roots = roots;
        self
    }

    pub fn with_secret_redactor(mut self, secret_redactor: SecretRedactor) -> Self {
        self.secret_redactor = secret_redactor;
        self
    }

    pub fn with_elicitation_handler(
        mut self,
        elicitation_handler: Arc<dyn McpElicitationHandler>,
    ) -> Self {
        self.elicitation_handler = elicitation_handler;
        self
    }
}

pub async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
) -> Result<()> {
    register_mcp_tools_with_options(registry, servers, McpToolRegistrationOptions::eager()?).await
}

pub async fn activate_lazy_mcp_tools(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
) -> Result<()> {
    register_mcp_tools_with_options(registry, servers, McpToolRegistrationOptions::lazy()?).await
}

pub async fn register_mcp_tools_with_options(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<()> {
    register_mcp_tools_for_startup(registry, servers, options).await
}

async fn register_mcp_tools_for_startup(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<()> {
    let mut used_provider_names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    for server in servers {
        if server.startup != options.startup {
            if options.startup == McpServerStartup::Eager
                && server.startup == McpServerStartup::Lazy
            {
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    "lazy MCP server startup is deferred until explicit activation"
                );
            }
            continue;
        }

        let client = match McpClient::spawn(
            server.clone(),
            options.roots.clone(),
            options.secret_redactor.clone(),
            Arc::clone(&options.elicitation_handler),
        )
        .await
        {
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
                options.provider_tool_name_max_chars,
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
                trust: server.trust.clone(),
                secret_redactor: options.secret_redactor.clone(),
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

pub fn mcp_provider_tool_name_prefix(server_name: &str) -> String {
    format!("mcp__{}__", sanitize_provider_name_part(server_name))
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

    fn into_result(self) -> Value {
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
struct UnsupportedMcpElicitationHandler;

#[async_trait]
impl McpElicitationHandler for UnsupportedMcpElicitationHandler {}

pub fn unsupported_mcp_elicitation_handler() -> Arc<dyn McpElicitationHandler> {
    Arc::new(UnsupportedMcpElicitationHandler)
}

#[derive(Debug, Clone)]
struct McpServerObservedIdentity {
    command_fingerprint: String,
    protocol_version: String,
    server_name: String,
    server_version: String,
}

impl McpServerObservedIdentity {
    fn as_pinned_identity(&self) -> McpServerPinnedIdentity {
        McpServerPinnedIdentity {
            command_fingerprint: self.command_fingerprint.clone(),
            protocol_version: self.protocol_version.clone(),
            server_name: self.server_name.clone(),
            server_version: self.server_version.clone(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "command_fingerprint": self.command_fingerprint,
            "protocol_version": self.protocol_version,
            "server_name": self.server_name,
            "server_version": self.server_version,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct McpInitializeResult {
    #[serde(default, rename = "protocolVersion")]
    protocol_version: Option<String>,
    #[serde(default, rename = "serverInfo")]
    server_info: Option<McpServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpServerInfo {
    name: String,
    version: String,
}

struct McpClient {
    _child: Mutex<Child>,
    connection: Mutex<Connection>,
    server_name: String,
    trust: McpServerTrustPolicy,
    secret_redactor: SecretRedactor,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    roots: Vec<PathBuf>,
    identity: McpServerObservedIdentity,
}

struct Connection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    async fn spawn(
        config: McpServerConfig,
        roots: Vec<PathBuf>,
        secret_redactor: SecretRedactor,
        elicitation_handler: Arc<dyn McpElicitationHandler>,
    ) -> Result<Self> {
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

        let mut client = Self {
            _child: Mutex::new(child),
            connection: Mutex::new(Connection {
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 0,
            }),
            server_name: config.name.clone(),
            trust: config.trust.clone(),
            secret_redactor,
            elicitation_handler,
            roots,
            identity: McpServerObservedIdentity {
                command_fingerprint: String::new(),
                protocol_version: String::new(),
                server_name: String::new(),
                server_version: String::new(),
            },
        };
        let identity = tokio::time::timeout(
            std::time::Duration::from_secs(config.startup_timeout_secs),
            client.initialize(&config),
        )
        .await
        .with_context(|| format!("MCP server {} initialize timed out", config.name))??;
        validate_mcp_pin(&config, &identity)?;
        client.identity = identity;
        Ok(client)
    }

    async fn initialize(&self, config: &McpServerConfig) -> Result<McpServerObservedIdentity> {
        let mut capabilities = json!({
            "roots": { "listChanged": true }
        });
        if self.elicitation_handler.supports_elicitation()
            && let Some(object) = capabilities.as_object_mut()
        {
            object.insert("elicitation".to_owned(), json!({}));
        }
        let result = self
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": capabilities,
                    "clientInfo": { "name": "sigil", "version": "0.1.0" }
                }),
            )
            .await?;
        let initialize = serde_json::from_value::<McpInitializeResult>(result)
            .context("failed to decode MCP initialize result")?;
        self.send_notification("notifications/initialized", json!({}))
            .await?;
        let server_info = initialize.server_info.unwrap_or(McpServerInfo {
            name: String::new(),
            version: String::new(),
        });
        Ok(McpServerObservedIdentity {
            command_fingerprint: mcp_command_fingerprint(&config.command, &config.args)?,
            protocol_version: initialize
                .protocol_version
                .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_owned()),
            server_name: server_info.name,
            server_version: server_info.version,
        })
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
                let payload = json!({ "roots": roots });
                if !self.trust.allow_secrets && self.secret_redactor.value_contains_secret(&payload)
                {
                    let message = "MCP roots/list would expose a secret and this server has allow_secrets = false";
                    write_error_response(connection, id, -32000, message).await?;
                    bail!("MCP server {} {message}", self.server_name);
                }
                write_success_response(connection, id, payload).await
            }
            "elicitation/create" => {
                if !self.elicitation_handler.supports_elicitation() {
                    return write_error_response(
                        connection,
                        id,
                        -32601,
                        "MCP elicitation is not supported by sigil yet",
                    )
                    .await;
                }
                let request = mcp_elicitation_request(&self.server_name, message)?;
                match self.elicitation_handler.elicit(request).await {
                    Ok(response) => {
                        let payload = response.into_result();
                        if !self.trust.allow_secrets
                            && self.secret_redactor.value_contains_secret(&payload)
                        {
                            let message = "MCP elicitation response contains a secret and this server has allow_secrets = false";
                            write_error_response(connection, id, -32000, message).await?;
                            bail!("MCP server {} {message}", self.server_name);
                        }
                        write_success_response(connection, id, payload).await
                    }
                    Err(error) => {
                        write_error_response(connection, id, -32000, format!("{error:#}")).await
                    }
                }
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
    trust: McpServerTrustPolicy,
    secret_redactor: SecretRedactor,
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
        Ok(ToolResult::ok(
            call_id,
            self.spec.name.clone(),
            self.secret_redactor.redact_text(&content),
            ToolResultMeta::default(),
        ))
    }
}

fn validate_mcp_pin(config: &McpServerConfig, observed: &McpServerObservedIdentity) -> Result<()> {
    if !config.trust.pin_version {
        return Ok(());
    }
    let observed_pin = observed.as_pinned_identity();
    let Some(expected) = config.trust.pinned.as_ref() else {
        bail!(
            "MCP server {} has pin_version = true but no pinned identity; observed pin: {}",
            config.name,
            serde_json::to_string(&observed_pin)?
        );
    };

    let mut mismatches = Vec::new();
    if expected.command_fingerprint != observed_pin.command_fingerprint {
        mismatches.push(format!(
            "command_fingerprint expected {} observed {}",
            expected.command_fingerprint, observed_pin.command_fingerprint
        ));
    }
    if expected.protocol_version != observed_pin.protocol_version {
        mismatches.push(format!(
            "protocol_version expected {} observed {}",
            expected.protocol_version, observed_pin.protocol_version
        ));
    }
    if expected.server_name != observed_pin.server_name {
        mismatches.push(format!(
            "server_name expected {} observed {}",
            expected.server_name, observed_pin.server_name
        ));
    }
    if expected.server_version != observed_pin.server_version {
        mismatches.push(format!(
            "server_version expected {} observed {}",
            expected.server_version, observed_pin.server_version
        ));
    }

    if !mismatches.is_empty() {
        bail!(
            "MCP server {} pinned identity mismatch: {}",
            config.name,
            mismatches.join("; ")
        );
    }
    Ok(())
}

fn mcp_elicitation_request(server_name: &str, message: &Value) -> Result<McpElicitationRequest> {
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

fn mcp_command_fingerprint(command: &str, args: &[String]) -> Result<String> {
    let encoded = serde_json::to_vec(&json!({
        "command": command,
        "args": args,
    }))
    .context("failed to serialize MCP command fingerprint material")?;
    Ok(format!("sha256:{:x}", Sha256::digest(&encoded)))
}

fn summarize_egress_json(value: &Value) -> Value {
    let byte_count = serde_json::to_vec(value).map_or(0, |bytes| bytes.len());
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let field_types = keys
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        Value::String(json_type_label(object.get(key).unwrap_or(&Value::Null))),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            json!({
                "type": "object",
                "byte_count": byte_count,
                "top_level_keys": keys,
                "field_types": field_types,
            })
        }
        Value::Array(items) => json!({
            "type": "array",
            "byte_count": byte_count,
            "item_count": items.len(),
        }),
        other => json!({
            "type": json_type_label(other),
            "byte_count": byte_count,
        }),
    }
}

fn json_type_label(value: &Value) -> String {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_owned()
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
