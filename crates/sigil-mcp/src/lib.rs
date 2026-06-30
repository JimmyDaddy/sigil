use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionSandboxProfile,
    McpServerConfig, McpServerPinnedIdentity, McpServerStartup, McpServerTrustPolicy,
    MutationEventRecorder, ProviderCapabilities, SecretRedactor, Tool, ToolAccess, ToolCategory,
    ToolContext, ToolEffect, ToolEgressAudit, ToolErrorKind, ToolPreviewCapability, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    task::JoinHandle,
};
use tracing::warn;

const DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS: usize = 64;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const MCP_OUTPUT_LIMIT_LINES: usize = 2_000;

#[derive(Clone)]
pub struct McpToolRegistrationOptions {
    pub provider_tool_name_max_chars: usize,
    pub roots: Vec<PathBuf>,
    pub working_dir: Option<PathBuf>,
    pub secret_redactor: SecretRedactor,
    pub elicitation_handler: Arc<dyn McpElicitationHandler>,
    pub runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    pub startup: McpServerStartup,
    pub mutation_recorder: Option<MutationEventRecorder>,
    pub mutation_workspace_root: Option<PathBuf>,
    pub process_launcher: Arc<dyn McpProcessLauncher>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolRegistrationReport {
    pub process_launch_receipts: Vec<McpProcessLaunchReceipt>,
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
            working_dir: None,
            secret_redactor: SecretRedactor::empty(),
            elicitation_handler: unsupported_mcp_elicitation_handler(),
            runtime_event_handler: unsupported_mcp_runtime_event_handler(),
            startup,
            mutation_recorder: None,
            mutation_workspace_root: None,
            process_launcher: Arc::new(LocalMcpProcessLauncher),
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

    pub fn with_working_dir(mut self, working_dir: PathBuf) -> Self {
        self.working_dir = Some(working_dir);
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

    pub fn with_runtime_event_handler(
        mut self,
        runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    ) -> Self {
        self.runtime_event_handler = runtime_event_handler;
        self
    }

    pub fn with_mutation_recorder(
        mut self,
        workspace_root: PathBuf,
        mutation_recorder: MutationEventRecorder,
    ) -> Self {
        self.mutation_workspace_root = Some(workspace_root);
        self.mutation_recorder = Some(mutation_recorder);
        self
    }

    pub fn with_process_launcher(mut self, process_launcher: Arc<dyn McpProcessLauncher>) -> Self {
        self.process_launcher = process_launcher;
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpProcessClass {
    LocalStdioConfigured,
    LocalStdioPluginDeclared,
    LocalStdioSandboxed,
    RemoteOrExternal,
    UnsupportedLongLivedBackend,
}

impl McpProcessClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalStdioConfigured => "local_stdio_configured",
            Self::LocalStdioPluginDeclared => "local_stdio_plugin_declared",
            Self::LocalStdioSandboxed => "local_stdio_sandboxed",
            Self::RemoteOrExternal => "remote_or_external",
            Self::UnsupportedLongLivedBackend => "unsupported_long_lived_backend",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpProcessCoverage {
    LocalStdioOutsideSandbox,
    LocalStdioSandboxed,
    RemoteOrExternal,
    Unsupported,
}

impl McpProcessCoverage {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalStdioOutsideSandbox => "local_stdio_outside_sandbox",
            Self::LocalStdioSandboxed => "local_stdio_sandboxed",
            Self::RemoteOrExternal => "remote_or_external",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProcessLaunchRequest {
    pub server_name: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub startup_timeout_secs: u64,
    pub classification: McpProcessClass,
}

impl McpProcessLaunchRequest {
    fn from_config(config: &McpServerConfig, working_dir: Option<PathBuf>) -> Self {
        Self {
            server_name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            working_dir,
            env: BTreeMap::new(),
            startup_timeout_secs: config.startup_timeout_secs,
            classification: McpProcessClass::LocalStdioConfigured,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpProcessLaunchReceipt {
    pub server_name: String,
    pub classification: McpProcessClass,
    pub coverage: McpProcessCoverage,
    pub backend: Option<ExecutionBackendKind>,
    pub backend_capabilities: Option<ExecutionBackendCapabilities>,
    pub sandbox_profile: Option<ExecutionSandboxProfile>,
}

impl McpProcessLaunchReceipt {
    #[must_use]
    pub fn local_outside_sandbox(request: &McpProcessLaunchRequest) -> Self {
        Self {
            server_name: request.server_name.clone(),
            classification: request.classification,
            coverage: McpProcessCoverage::LocalStdioOutsideSandbox,
            backend: Some(ExecutionBackendKind::Local),
            backend_capabilities: Some(ExecutionBackendCapabilities::default()),
            sandbox_profile: Some(ExecutionSandboxProfile::Unconfined),
        }
    }

    #[must_use]
    pub fn audit_metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::from([
            (
                "mcp_process_class".to_owned(),
                self.classification.as_str().to_owned(),
            ),
            (
                "mcp_process_coverage".to_owned(),
                self.coverage.as_str().to_owned(),
            ),
        ]);
        if let Some(backend) = self.backend {
            metadata.insert(
                "mcp_process_backend".to_owned(),
                backend.as_str().to_owned(),
            );
        }
        if let Some(profile) = self.sandbox_profile {
            metadata.insert(
                "mcp_process_profile".to_owned(),
                mcp_sandbox_profile_label(profile).to_owned(),
            );
        }
        if let Some(capabilities) = self.backend_capabilities {
            metadata.insert(
                "mcp_process_backend_capabilities".to_owned(),
                mcp_backend_capability_labels(capabilities).join(","),
            );
        }
        metadata
    }
}

fn mcp_sandbox_profile_label(profile: ExecutionSandboxProfile) -> &'static str {
    match profile {
        ExecutionSandboxProfile::Unconfined => "unconfined",
        ExecutionSandboxProfile::WorkspaceWrite => "workspace_write",
        ExecutionSandboxProfile::BuildOffline => "build_offline",
        ExecutionSandboxProfile::BuildNetworked => "build_networked",
    }
}

fn mcp_backend_capability_labels(capabilities: ExecutionBackendCapabilities) -> Vec<&'static str> {
    let mut labels = Vec::new();
    if capabilities.filesystem_isolation {
        labels.push("filesystem");
    }
    if capabilities.network_isolation {
        labels.push("network");
    }
    if capabilities.process_isolation {
        labels.push("process");
    }
    if capabilities.resource_limits {
        labels.push("resource");
    }
    if capabilities.persistent_pty {
        labels.push("persistent_pty");
    }
    if capabilities.workspace_snapshot {
        labels.push("workspace_snapshot");
    }
    if labels.is_empty() {
        labels.push("none");
    }
    labels
}

pub struct McpProcessLaunch {
    pub child: Child,
    pub receipt: McpProcessLaunchReceipt,
}

pub trait McpProcessLauncher: Send + Sync {
    /// Launches one local MCP stdio process and returns its coverage receipt.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured process cannot be spawned or a required sandbox
    /// coverage cannot be provided.
    fn launch(&self, request: McpProcessLaunchRequest) -> Result<McpProcessLaunch>;
}

#[derive(Debug)]
pub struct LocalMcpProcessLauncher;

impl McpProcessLauncher for LocalMcpProcessLauncher {
    fn launch(&self, request: McpProcessLaunchRequest) -> Result<McpProcessLaunch> {
        let mut command = Command::new(&request.command);
        command
            .args(&request.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(working_dir) = &request.working_dir {
            command.current_dir(working_dir);
        }
        command.envs(&request.env);
        let child = command
            .spawn()
            .with_context(|| format!("failed to spawn MCP server {}", request.server_name))?;
        Ok(McpProcessLaunch {
            child,
            receipt: McpProcessLaunchReceipt::local_outside_sandbox(&request),
        })
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
    register_mcp_tools_with_report(registry, servers, options)
        .await
        .map(|_| ())
}

pub async fn register_mcp_tools_with_report(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<McpToolRegistrationReport> {
    register_mcp_tools_for_startup(registry, servers, options).await
}

async fn register_mcp_tools_for_startup(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<McpToolRegistrationReport> {
    let mut used_provider_names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    let mut report = McpToolRegistrationReport::default();
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
            options.working_dir.clone(),
            options.secret_redactor.clone(),
            Arc::clone(&options.elicitation_handler),
            Arc::clone(&options.runtime_event_handler),
            Arc::clone(&options.process_launcher),
        )
        .await
        {
            Ok(client) => client,
            Err(error) if !server.required => {
                record_mcp_server_lifecycle_unknown_dirty(&options, &server.name, None)?;
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    error = %error,
                    "optional MCP server failed to start and will be skipped"
                );
                continue;
            }
            Err(error) => {
                record_mcp_server_lifecycle_unknown_dirty(&options, &server.name, None)?;
                return Err(error);
            }
        };
        let process_receipt = client.process_receipt().clone();
        record_mcp_server_lifecycle_unknown_dirty(&options, &server.name, Some(&process_receipt))?;
        report.process_launch_receipts.push(process_receipt);
        let client = Arc::new(client);
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
        if client.supports_resources() {
            for resource_kind in McpResourceToolKind::all() {
                let original_name = resource_kind.provider_suffix();
                let tool_name = McpToolName::new(
                    &server.name,
                    original_name,
                    options.provider_tool_name_max_chars,
                    &mut used_provider_names,
                );
                registry.register(Arc::new(McpResourceTool {
                    client: Arc::clone(&client),
                    spec: ToolSpec {
                        name: tool_name.provider_name.clone(),
                        description: resource_kind.description().to_owned(),
                        input_schema: resource_kind.input_schema(),
                        category: ToolCategory::Mcp,
                        access: ToolAccess::Read,
                        preview: ToolPreviewCapability::None,
                    },
                    tool_name,
                    kind: resource_kind,
                    trust: server.trust.clone(),
                    secret_redactor: options.secret_redactor.clone(),
                }));
            }
        }
        if client.supports_prompts() {
            for prompt_kind in McpPromptToolKind::all() {
                let original_name = prompt_kind.provider_suffix();
                let tool_name = McpToolName::new(
                    &server.name,
                    original_name,
                    options.provider_tool_name_max_chars,
                    &mut used_provider_names,
                );
                registry.register(Arc::new(McpPromptTool {
                    client: Arc::clone(&client),
                    spec: ToolSpec {
                        name: tool_name.provider_name.clone(),
                        description: prompt_kind.description().to_owned(),
                        input_schema: prompt_kind.input_schema(),
                        category: ToolCategory::Mcp,
                        access: ToolAccess::Read,
                        preview: ToolPreviewCapability::None,
                    },
                    tool_name,
                    kind: prompt_kind,
                    trust: server.trust.clone(),
                    secret_redactor: options.secret_redactor.clone(),
                }));
            }
        }
    }
    Ok(report)
}

fn record_mcp_server_lifecycle_unknown_dirty(
    options: &McpToolRegistrationOptions,
    server_name: &str,
    receipt: Option<&McpProcessLaunchReceipt>,
) -> Result<()> {
    let (Some(recorder), Some(workspace_root)) =
        (&options.mutation_recorder, &options.mutation_workspace_root)
    else {
        return Ok(());
    };
    let metadata = receipt
        .map(McpProcessLaunchReceipt::audit_metadata)
        .unwrap_or_else(|| {
            BTreeMap::from([(
                "mcp_process_coverage".to_owned(),
                McpProcessCoverage::Unsupported.as_str().to_owned(),
            )])
        });
    recorder
        .record_external_process_unknown_dirty_with_metadata(
            workspace_root,
            format!("mcp_server:{server_name}"),
            ToolEffect::Unknown,
            metadata,
        )
        .with_context(|| {
            format!("failed to record MCP server {server_name} lifecycle mutation evidence")
        })?;
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

#[derive(Debug, Clone, Copy)]
enum McpResourceToolKind {
    List,
    Read,
}

impl McpResourceToolKind {
    fn all() -> [Self; 2] {
        [Self::List, Self::Read]
    }

    fn provider_suffix(self) -> &'static str {
        match self {
            Self::List => "resources_list",
            Self::Read => "resources_read",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::List => "List read-only MCP resources exposed by this server",
            Self::Read => "Read one MCP resource by URI",
        }
    }

    fn input_schema(self) -> Value {
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

#[derive(Debug, Clone, Copy)]
enum McpPromptToolKind {
    List,
    Get,
}

impl McpPromptToolKind {
    fn all() -> [Self; 2] {
        [Self::List, Self::Get]
    }

    fn provider_suffix(self) -> &'static str {
        match self {
            Self::List => "prompts_list",
            Self::Get => "prompts_get",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::List => "List MCP prompts exposed by this server",
            Self::Get => "Get one MCP prompt by name with optional arguments",
        }
    }

    fn input_schema(self) -> Value {
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

    fn method(self) -> &'static str {
        match self {
            Self::List => "prompts/list",
            Self::Get => "prompts/get",
        }
    }

    fn request_params(self, args: &Value) -> std::result::Result<Value, String> {
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
struct UnsupportedMcpRuntimeEventHandler;

#[async_trait]
impl McpRuntimeEventHandler for UnsupportedMcpRuntimeEventHandler {}

pub fn unsupported_mcp_runtime_event_handler() -> Arc<dyn McpRuntimeEventHandler> {
    Arc::new(UnsupportedMcpRuntimeEventHandler)
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
    #[serde(default)]
    capabilities: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct McpServerInfo {
    name: String,
    version: String,
}

struct McpInitializeOutcome {
    identity: McpServerObservedIdentity,
    capabilities: Value,
}

struct McpClient {
    _child: Mutex<Child>,
    _process_receipt: McpProcessLaunchReceipt,
    _stderr_task: JoinHandle<()>,
    connection: Mutex<Connection>,
    server_name: String,
    trust: McpServerTrustPolicy,
    secret_redactor: SecretRedactor,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    roots: Vec<PathBuf>,
    identity: McpServerObservedIdentity,
    server_capabilities: Value,
}

struct Connection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn process_receipt(&self) -> &McpProcessLaunchReceipt {
        &self._process_receipt
    }

    async fn spawn(
        config: McpServerConfig,
        roots: Vec<PathBuf>,
        working_dir: Option<PathBuf>,
        secret_redactor: SecretRedactor,
        elicitation_handler: Arc<dyn McpElicitationHandler>,
        runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
        process_launcher: Arc<dyn McpProcessLauncher>,
    ) -> Result<Self> {
        let launch_request = McpProcessLaunchRequest::from_config(&config, working_dir);
        let launch = process_launcher.launch(launch_request)?;
        let mut child = launch.child;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("missing stdin for MCP server {}", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing stdout for MCP server {}", config.name))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("missing stderr for MCP server {}", config.name))?;
        let stderr_task = tokio::spawn(drain_mcp_stderr(stderr));

        let mut client = Self {
            _child: Mutex::new(child),
            _process_receipt: launch.receipt,
            _stderr_task: stderr_task,
            connection: Mutex::new(Connection {
                stdin,
                stdout: BufReader::new(stdout),
                next_id: 0,
            }),
            server_name: config.name.clone(),
            trust: config.trust.clone(),
            secret_redactor,
            elicitation_handler,
            runtime_event_handler,
            roots,
            identity: McpServerObservedIdentity {
                command_fingerprint: String::new(),
                protocol_version: String::new(),
                server_name: String::new(),
                server_version: String::new(),
            },
            server_capabilities: Value::Null,
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(config.startup_timeout_secs),
            client.initialize(&config),
        )
        .await
        .with_context(|| format!("MCP server {} initialize timed out", config.name))??;
        validate_mcp_pin(&config, &outcome.identity)?;
        client.identity = outcome.identity;
        client.server_capabilities = outcome.capabilities;
        Ok(client)
    }

    async fn initialize(&self, config: &McpServerConfig) -> Result<McpInitializeOutcome> {
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
        Ok(McpInitializeOutcome {
            identity: McpServerObservedIdentity {
                command_fingerprint: mcp_command_fingerprint(&config.command, &config.args)?,
                protocol_version: initialize
                    .protocol_version
                    .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_owned()),
                server_name: server_info.name,
                server_version: server_info.version,
            },
            capabilities: initialize.capabilities,
        })
    }

    fn supports_resources(&self) -> bool {
        self.server_capabilities
            .get("resources")
            .is_some_and(Value::is_object)
    }

    fn supports_prompts(&self) -> bool {
        self.server_capabilities
            .get("prompts")
            .is_some_and(Value::is_object)
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
            if let Some(notification) = mcp_progress_notification(&self.server_name, message) {
                self.runtime_event_handler.progress(notification).await?;
            }
            return Ok(());
        }
        if let Some(kind) = mcp_list_changed_kind(method) {
            self.runtime_event_handler
                .list_changed(McpListChangedNotification {
                    server_name: self.server_name.clone(),
                    kind,
                })
                .await?;
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

struct McpResourceTool {
    client: Arc<McpClient>,
    spec: ToolSpec,
    tool_name: McpToolName,
    kind: McpResourceToolKind,
    trust: McpServerTrustPolicy,
    secret_redactor: SecretRedactor,
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

struct McpPromptTool {
    client: Arc<McpClient>,
    spec: ToolSpec,
    tool_name: McpToolName,
    kind: McpPromptToolKind,
    trust: McpServerTrustPolicy,
    secret_redactor: SecretRedactor,
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

fn bounded_mcp_tool_result(
    secret_redactor: &SecretRedactor,
    tool_name: &McpToolName,
    trust: &McpServerTrustPolicy,
    identity: &McpServerObservedIdentity,
    surface_kind: &str,
    operation: &str,
    content: String,
) -> (String, ToolResultMeta) {
    let redacted = secret_redactor.redact_text(&content);
    let budget = truncate_text_budget(&redacted, MCP_OUTPUT_LIMIT_BYTES, MCP_OUTPUT_LIMIT_LINES);
    let mut metadata = ToolResultMeta {
        bytes: Some(to_u64(budget.returned_bytes)),
        truncated: budget.truncated,
        omitted_bytes: if budget.truncated {
            Some(to_u64(budget.omitted_bytes))
        } else {
            None
        },
        limit_bytes: Some(to_u64(MCP_OUTPUT_LIMIT_BYTES)),
        limit_lines: Some(to_u64(MCP_OUTPUT_LIMIT_LINES)),
        returned_bytes: Some(to_u64(budget.returned_bytes)),
        returned_lines: Some(to_u64(budget.returned_lines)),
        total_bytes: Some(to_u64(budget.total_bytes)),
        total_lines: Some(to_u64(budget.total_lines)),
        details: json!({
            "mcp": {
                "server": tool_name.server_name,
                "tool": tool_name.original_name,
                "trust_class": trust.trust_class.as_str(),
                "kind": surface_kind,
                "operation": operation,
                "server_identity": identity.to_json(),
            }
        }),
        ..ToolResultMeta::default()
    };
    if budget.truncated {
        metadata.details["mcp"]["truncation"] = json!({
            "omitted_bytes": budget.omitted_bytes,
            "limit_bytes": MCP_OUTPUT_LIMIT_BYTES,
            "limit_lines": MCP_OUTPUT_LIMIT_LINES,
        });
    }
    (budget.content, metadata)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextBudgetResult {
    content: String,
    truncated: bool,
    total_bytes: usize,
    total_lines: usize,
    returned_bytes: usize,
    returned_lines: usize,
    omitted_bytes: usize,
}

fn truncate_text_budget(text: &str, max_bytes: usize, max_lines: usize) -> TextBudgetResult {
    let total_bytes = text.len();
    let total_lines = text.lines().count().max(usize::from(!text.is_empty()));
    let mut returned = String::new();
    let mut returned_lines = 0usize;
    let mut truncated = false;

    for (index, line) in text.split_inclusive('\n').enumerate() {
        if index >= max_lines {
            truncated = true;
            break;
        }
        if returned.len().saturating_add(line.len()) > max_bytes {
            let remaining = max_bytes.saturating_sub(returned.len());
            append_utf8_prefix(&mut returned, line, remaining);
            truncated = true;
            break;
        }
        returned.push_str(line);
        returned_lines += 1;
    }

    if !truncated && returned.len() < total_bytes {
        truncated = true;
    }
    if truncated {
        let marker = "\n[MCP output truncated]";
        if returned.len().saturating_add(marker.len()) <= max_bytes {
            returned.push_str(marker);
        }
    }
    let returned_bytes = returned.len();
    TextBudgetResult {
        content: returned,
        truncated,
        total_bytes,
        total_lines,
        returned_bytes,
        returned_lines: returned_lines.max(usize::from(returned_bytes > 0)),
        omitted_bytes: total_bytes.saturating_sub(returned_bytes),
    }
}

fn append_utf8_prefix(output: &mut String, text: &str, byte_budget: usize) {
    if byte_budget == 0 {
        return;
    }
    let mut end = 0usize;
    for (index, ch) in text.char_indices() {
        let next = index + ch.len_utf8();
        if next > byte_budget {
            break;
        }
        end = next;
    }
    output.push_str(&text[..end]);
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

async fn drain_mcp_stderr(mut stderr: ChildStderr) {
    let mut buffer = [0_u8; 4096];
    while stderr.read(&mut buffer).await.is_ok_and(|read| read > 0) {}
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

fn mcp_progress_notification(
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

fn mcp_list_changed_kind(method: &str) -> Option<McpListChangedKind> {
    match method {
        "notifications/tools/list_changed" => Some(McpListChangedKind::Tools),
        "notifications/resources/list_changed" => Some(McpListChangedKind::Resources),
        "notifications/prompts/list_changed" => Some(McpListChangedKind::Prompts),
        _ => None,
    }
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
