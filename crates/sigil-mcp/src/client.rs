use super::*;

pub(super) struct McpServerObservedIdentity {
    pub(super) command_fingerprint: String,
    pub(super) environment_grant_names: Vec<String>,
    pub(super) environment_static_fingerprint: String,
    pub(super) environment_live_fingerprint: String,
    pub(super) protocol_version: String,
    pub(super) server_name: String,
    pub(super) server_version: String,
}

impl McpServerObservedIdentity {
    pub(super) fn as_pinned_identity(&self) -> McpServerPinnedIdentity {
        McpServerPinnedIdentity {
            command_fingerprint: self.command_fingerprint.clone(),
            protocol_version: self.protocol_version.clone(),
            server_name: self.server_name.clone(),
            server_version: self.server_version.clone(),
        }
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "command_fingerprint": self.command_fingerprint,
            "environment_grant_names": self.environment_grant_names,
            "environment_grant_source": "parent_environment",
            "environment_static_fingerprint": self.environment_static_fingerprint,
            "environment_live_fingerprint": self.environment_live_fingerprint,
            "protocol_version": self.protocol_version,
            "server_name": self.server_name,
            "server_version": self.server_version,
        })
    }

    pub(super) fn trust_subject(
        &self,
        server_name: impl Into<String>,
        trust_class: impl Into<String>,
    ) -> ToolSubject {
        ToolSubject::mcp_trust_class_with_process_binding(
            server_name,
            trust_class,
            &self.command_fingerprint,
            &self.environment_live_fingerprint,
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct McpInitializeResult {
    #[serde(default, rename = "protocolVersion")]
    pub(super) protocol_version: Option<String>,
    #[serde(default, rename = "serverInfo")]
    pub(super) server_info: Option<McpServerInfo>,
    #[serde(default)]
    pub(super) capabilities: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct McpServerInfo {
    pub(super) name: String,
    pub(super) version: String,
}

pub(super) struct McpInitializeOutcome {
    pub(super) identity: McpServerObservedIdentity,
    pub(super) capabilities: Value,
}

#[derive(Debug)]
pub(super) struct McpPostSpawnStartupError {
    receipt: McpProcessLaunchReceipt,
    source: anyhow::Error,
}

impl McpPostSpawnStartupError {
    fn new(receipt: McpProcessLaunchReceipt, source: anyhow::Error) -> Self {
        Self { receipt, source }
    }

    pub(super) fn receipt(&self) -> &McpProcessLaunchReceipt {
        &self.receipt
    }
}

impl std::fmt::Display for McpPostSpawnStartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MCP server {} failed after process spawn: {:#}",
            self.receipt.server_name, self.source
        )
    }
}

impl std::error::Error for McpPostSpawnStartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(super) struct McpClient {
    pub(super) _child: Mutex<Child>,
    pub(super) _process_receipt: McpProcessLaunchReceipt,
    pub(super) _stderr_task: JoinHandle<()>,
    pub(super) connection: Mutex<Connection>,
    pub(super) server_name: String,
    pub(super) trust: McpServerTrustPolicy,
    pub(super) secret_redactor: SecretRedactor,
    pub(super) elicitation_handler: Arc<dyn McpElicitationHandler>,
    pub(super) runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    pub(super) roots: Vec<PathBuf>,
    pub(super) identity: McpServerObservedIdentity,
    pub(super) server_capabilities: Value,
}

pub(super) struct Connection {
    pub(super) stdin: ChildStdin,
    pub(super) stdout: BufReader<ChildStdout>,
    pub(super) next_id: u64,
}

impl McpClient {
    pub(super) fn process_receipt(&self) -> &McpProcessLaunchReceipt {
        &self._process_receipt
    }

    pub(super) async fn spawn(
        config: McpServerConfig,
        roots: Vec<PathBuf>,
        working_dir: Option<PathBuf>,
        mut secret_redactor: SecretRedactor,
        elicitation_handler: Arc<dyn McpElicitationHandler>,
        runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
        process_launcher: Arc<dyn McpProcessLauncher>,
        expected_process_subject: Option<&ToolSubject>,
    ) -> Result<Self> {
        let launch_request = McpProcessLaunchRequest::from_config(&config, working_dir)?;
        validate_expected_process_subject(&config, &launch_request, expected_process_subject)?;
        validate_mcp_static_pin(&config, &launch_request.launch_static_fingerprint)?;
        for name in launch_request.environment.grant_names() {
            if let Some(secret) = launch_request.environment.variable(name) {
                secret_redactor.add_secret_carrier(secret.clone());
            }
        }
        let launch = process_launcher.launch(launch_request)?;
        let startup_receipt = launch.receipt.clone();
        let startup = async move {
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
                    environment_grant_names: Vec::new(),
                    environment_static_fingerprint: String::new(),
                    environment_live_fingerprint: String::new(),
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
        .await;
        startup.map_err(|source| McpPostSpawnStartupError::new(startup_receipt, source).into())
    }

    pub(super) async fn initialize(
        &self,
        _config: &McpServerConfig,
    ) -> Result<McpInitializeOutcome> {
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
                    "clientInfo": { "name": "sigil", "version": env!("CARGO_PKG_VERSION") }
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
                command_fingerprint: self._process_receipt.launch_static_fingerprint.clone(),
                environment_grant_names: self._process_receipt.environment_grant_names.clone(),
                environment_static_fingerprint: self
                    ._process_receipt
                    .environment_static_fingerprint
                    .clone(),
                environment_live_fingerprint: self
                    ._process_receipt
                    .environment_live_fingerprint
                    .clone(),
                protocol_version: self.secret_redactor.redact_text(
                    &initialize
                        .protocol_version
                        .unwrap_or_else(|| MCP_PROTOCOL_VERSION.to_owned()),
                ),
                server_name: self.secret_redactor.redact_text(&server_info.name),
                server_version: self.secret_redactor.redact_text(&server_info.version),
            },
            capabilities: initialize.capabilities,
        })
    }

    pub(super) fn supports_resources(&self) -> bool {
        self.server_capabilities
            .get("resources")
            .is_some_and(Value::is_object)
    }

    pub(super) fn supports_prompts(&self) -> bool {
        self.server_capabilities
            .get("prompts")
            .is_some_and(Value::is_object)
    }

    pub(super) async fn list_tools(&self) -> Result<Vec<McpToolDescriptor>> {
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

    pub(super) async fn call_tool_response(&self, name: &str, args: Value) -> Result<Value> {
        self.send_request_response(
            "tools/call",
            json!({
                "name": name,
                "arguments": args,
            }),
        )
        .await
    }

    pub(super) async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut connection = self.connection.lock().await;
        self.ensure_environment_binding_current().await?;
        write_message(&mut connection.stdin, &message).await
    }

    pub(super) async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        let response = self.send_request_response(method, params).await?;
        if let Some(error) = response.get("error") {
            bail!(
                "MCP request {} failed: {}",
                method,
                self.secret_redactor.redact_value(error)
            );
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response missing result"))
    }

    pub(super) async fn send_request_response(&self, method: &str, params: Value) -> Result<Value> {
        let mut connection = self.connection.lock().await;
        self.ensure_environment_binding_current().await?;
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
            self.ensure_environment_binding_current().await?;
            return Ok(self.secret_redactor.redact_value(&response));
        }
    }

    async fn ensure_environment_binding_current(&self) -> Result<()> {
        let current = match resolve_extension_process_environment(
            &self._process_receipt.environment_grant_names,
        ) {
            Ok(current) => current,
            Err(error) => {
                self.invalidate_process().await;
                return Err(error.into());
            }
        };
        if environment_binding_matches(&self._process_receipt, &current) {
            return Ok(());
        }
        self.invalidate_process().await;
        Err(ExtensionProcessLaunchError::environment_binding_changed(
            &self.server_name,
            format!(
                "MCP server {} environment binding changed; restart or refresh the server before retrying",
                self.server_name
            ),
        )
        .into())
    }

    async fn invalidate_process(&self) {
        let mut child = self._child.lock().await;
        let _ = child.start_kill();
    }

    pub(super) async fn handle_inbound_message(
        &self,
        connection: &mut Connection,
        message: &Value,
    ) -> Result<()> {
        self.ensure_environment_binding_current().await?;
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Ok(());
        };
        let safe_message = self.secret_redactor.redact_value(message);
        if method == "notifications/progress" {
            if let Some(notification) = mcp_progress_notification(&self.server_name, &safe_message)
            {
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
                    self.ensure_environment_binding_current().await?;
                    write_error_response(connection, id, -32000, message).await?;
                    bail!("MCP server {} {message}", self.server_name);
                }
                self.ensure_environment_binding_current().await?;
                write_success_response(connection, id, payload).await
            }
            "elicitation/create" => {
                if !self.elicitation_handler.supports_elicitation() {
                    self.ensure_environment_binding_current().await?;
                    return write_error_response(
                        connection,
                        id,
                        -32601,
                        "MCP elicitation is not supported by sigil yet",
                    )
                    .await;
                }
                let request = mcp_elicitation_request(&self.server_name, &safe_message)?;
                match self.elicitation_handler.elicit(request).await {
                    Ok(response) => {
                        let payload = response.into_result();
                        if !self.trust.allow_secrets
                            && self.secret_redactor.value_contains_secret(&payload)
                        {
                            let message = "MCP elicitation response contains a secret and this server has allow_secrets = false";
                            self.ensure_environment_binding_current().await?;
                            write_error_response(connection, id, -32000, message).await?;
                            bail!("MCP server {} {message}", self.server_name);
                        }
                        self.ensure_environment_binding_current().await?;
                        write_success_response(connection, id, payload).await
                    }
                    Err(error) => {
                        self.ensure_environment_binding_current().await?;
                        write_error_response(
                            connection,
                            id,
                            -32000,
                            self.secret_redactor.redact_text(&format!("{error:#}")),
                        )
                        .await
                    }
                }
            }
            _ => {
                self.ensure_environment_binding_current().await?;
                write_error_response(
                    connection,
                    id,
                    -32601,
                    format!(
                        "MCP client method is not supported: {}",
                        self.secret_redactor.redact_text(method)
                    ),
                )
                .await
            }
        }
    }
}

fn validate_expected_process_subject(
    config: &McpServerConfig,
    request: &McpProcessLaunchRequest,
    expected: Option<&ToolSubject>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let observed = ToolSubject::mcp_trust_class_with_process_binding(
        &config.name,
        config.trust.trust_class.as_str(),
        &request.launch_static_fingerprint,
        request.environment.live_fingerprint(),
    );
    if &observed == expected {
        return Ok(());
    }
    Err(ExtensionProcessLaunchError::environment_binding_changed(
        &config.name,
        format!(
            "MCP server {} process binding changed after approval; retry activation to review the current binding",
            config.name
        ),
    )
    .into())
}

pub(super) fn environment_binding_matches(
    receipt: &McpProcessLaunchReceipt,
    current: &ResolvedProcessEnvironment,
) -> bool {
    current.static_fingerprint() == receipt.environment_static_fingerprint
        && current.live_fingerprint() == receipt.environment_live_fingerprint
}

pub(super) fn validate_mcp_static_pin(config: &McpServerConfig, observed: &str) -> Result<()> {
    if !config.trust.pin_version {
        return Ok(());
    }
    let Some(expected) = config.trust.pinned.as_ref() else {
        return Err(ExtensionProcessLaunchError::configuration_invalid(
            &config.name,
            format!(
                "MCP server {} has pin_version = true but no pinned identity; pre-spawn command_fingerprint={observed}",
                config.name
            ),
        )
        .into());
    };
    if expected.command_fingerprint != observed {
        return Err(ExtensionProcessLaunchError::configuration_invalid(
            &config.name,
            format!(
                "MCP server {} pre-spawn command_fingerprint mismatch: expected {} observed {}",
                config.name, expected.command_fingerprint, observed
            ),
        )
        .into());
    }
    Ok(())
}

pub(super) fn validate_mcp_pin(
    config: &McpServerConfig,
    observed: &McpServerObservedIdentity,
) -> Result<()> {
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

pub(super) async fn write_success_response(
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

pub(super) async fn write_error_response(
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

pub(super) async fn write_message(stdin: &mut ChildStdin, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
    stdin.flush().await?;
    Ok(())
}

pub(super) async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
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
