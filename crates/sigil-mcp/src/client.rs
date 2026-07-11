use super::*;

pub(super) struct McpServerObservedIdentity {
    /// Stable command/executable pin material used only for version pin validation.
    pub(super) command_fingerprint: String,
    /// Exact runtime-keyed process binding used for permission subjects and approval continuity.
    pub(super) process_authorization_fingerprint: String,
    pub(super) declaration: Option<McpDeclarationLaunchMetadata>,
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

    pub(super) fn trust_subject(
        &self,
        server_name: impl Into<String>,
        trust_class: impl Into<String>,
    ) -> ToolSubject {
        ToolSubject::mcp_trust_class_with_process_binding(
            server_name,
            trust_class,
            &self.process_authorization_fingerprint,
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
    cleanup: McpCleanupEvidence,
}

impl McpPostSpawnStartupError {
    fn new(
        receipt: McpProcessLaunchReceipt,
        source: anyhow::Error,
        cleanup: McpCleanupEvidence,
    ) -> Self {
        Self {
            receipt,
            source,
            cleanup,
        }
    }

    pub(super) fn receipt(&self) -> &McpProcessLaunchReceipt {
        &self.receipt
    }

    pub(super) fn cleanup_completed(&self) -> bool {
        self.cleanup.completed
    }
}

impl std::fmt::Display for McpPostSpawnStartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MCP server {} failed after process spawn: {:#}; transport cleanup: {}",
            self.receipt.server_name,
            self.source,
            self.cleanup.summary()
        )
    }
}

impl std::error::Error for McpPostSpawnStartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(super) struct McpClient {
    pub(super) _child: Mutex<Option<Child>>,
    pub(super) _process_receipt: McpProcessLaunchReceipt,
    pub(super) _stderr_task: Mutex<Option<JoinHandle<McpStderrSummary>>>,
    pub(super) _stderr_monitor_task: std::sync::Mutex<Option<JoinHandle<()>>>,
    stderr_fault_receiver: Mutex<Option<tokio::sync::oneshot::Receiver<McpStderrFault>>>,
    stderr_faulted: Arc<std::sync::atomic::AtomicBool>,
    stderr_fault: Arc<std::sync::Mutex<Option<McpStderrFault>>>,
    pub(super) connection: Mutex<McpConnectionState>,
    pub(super) server_name: String,
    pub(super) trust: McpServerTrustPolicy,
    pub(super) secret_redactor: SecretRedactor,
    pub(super) elicitation_handler: Arc<dyn McpElicitationHandler>,
    pub(super) runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    pub(super) roots: Vec<PathBuf>,
    identity: std::sync::OnceLock<McpServerObservedIdentity>,
    server_capabilities: std::sync::OnceLock<Value>,
    pub(super) startup_deadline: McpOperationDeadline,
    terminal_state: std::sync::atomic::AtomicU8,
    terminal_record: std::sync::Mutex<Option<McpTerminalRecord>>,
    cleanup_outcome: Mutex<Option<McpProcessCleanupSummary>>,
    lifecycle_owner: ToolLifecycleOwner,
}

pub(super) enum McpConnectionState {
    Ready(Connection),
    Closing {
        reason: String,
    },
    Closed {
        reason: String,
        cleanup_completed: bool,
        stderr_total_bytes: u64,
        stderr_truncated: bool,
    },
}

pub(super) struct Connection {
    pub(super) stdin: ChildStdin,
    pub(super) stdout: BufReader<ChildStdout>,
    pub(super) next_id: u64,
}

struct McpStderrCaptureCompletion {
    summary: McpStderrSummary,
    failure: Option<String>,
}

#[derive(Clone)]
struct McpTerminalRecord {
    reason: String,
    cause: Option<McpTerminalCause>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct McpOperationDeadline {
    at: tokio::time::Instant,
    timeout_ms: u64,
}

impl McpOperationDeadline {
    pub(super) fn from_secs(timeout_secs: u64) -> Self {
        let timeout_secs = if timeout_secs == 0 {
            DEFAULT_MCP_OPERATION_TIMEOUT_SECS
        } else {
            timeout_secs.min(MAX_MCP_OPERATION_TIMEOUT_SECS)
        };
        let duration = Duration::from_secs(timeout_secs);
        let now = tokio::time::Instant::now();
        Self {
            at: now.checked_add(duration).unwrap_or(now),
            timeout_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

impl McpClient {
    pub(super) fn process_receipt(&self) -> &McpProcessLaunchReceipt {
        &self._process_receipt
    }

    pub(super) fn lifecycle_owner(&self) -> ToolLifecycleOwner {
        self.lifecycle_owner.clone()
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
        network_admission: ExtensionProcessNetworkAdmission,
    ) -> Result<Arc<Self>> {
        let startup_deadline = McpOperationDeadline::from_secs(config.startup_timeout_secs);
        let launch_request = process_launcher
            .resolve_launch_request(&config, working_dir)?
            .with_network_admission(network_admission);
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

            let stdin = child.stdin.take();
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let (Some(stdin), Some(stdout), Some(stderr)) = (stdin, stdout, stderr) else {
                let cleanup = terminate_mcp_process(&mut child).await;
                return Err(McpPostSpawnStartupError::new(
                    startup_receipt.clone(),
                    anyhow!("missing stdio pipe for MCP server {}", config.name),
                    McpCleanupEvidence {
                        completed: cleanup.completed,
                        reason: cleanup.reason,
                    },
                ));
            };
            let (stderr_fault_sender, stderr_fault_receiver) = tokio::sync::oneshot::channel();
            let stderr_faulted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stderr_fault = Arc::new(std::sync::Mutex::new(None));
            let stderr_task = tokio::spawn(drain_mcp_stderr(
                stderr,
                stderr_fault_sender,
                Arc::clone(&stderr_faulted),
                Arc::clone(&stderr_fault),
            ));

            let client = Arc::new(Self {
                _child: Mutex::new(Some(child)),
                _process_receipt: launch.receipt,
                _stderr_task: Mutex::new(Some(stderr_task)),
                _stderr_monitor_task: std::sync::Mutex::new(None),
                stderr_fault_receiver: Mutex::new(Some(stderr_fault_receiver)),
                stderr_faulted,
                stderr_fault,
                connection: Mutex::new(McpConnectionState::Ready(Connection {
                    stdin,
                    stdout: BufReader::new(stdout),
                    next_id: 0,
                })),
                server_name: config.name.clone(),
                trust: config.trust.clone(),
                secret_redactor,
                elicitation_handler,
                runtime_event_handler,
                roots,
                identity: std::sync::OnceLock::new(),
                server_capabilities: std::sync::OnceLock::new(),
                startup_deadline,
                terminal_state: std::sync::atomic::AtomicU8::new(0),
                terminal_record: std::sync::Mutex::new(None),
                cleanup_outcome: Mutex::new(None),
                lifecycle_owner: ToolLifecycleOwner::new(
                    MCP_TOOL_LIFECYCLE_NAMESPACE,
                    config.name.clone(),
                    Uuid::new_v4().to_string(),
                ),
            });
            client.start_stderr_monitor().await;
            let outcome = match client.initialize(&config, startup_deadline).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    let cleanup = client
                        .close_connection(format!("initialize failed: {error:#}"))
                        .await;
                    return Err(McpPostSpawnStartupError::new(
                        startup_receipt.clone(),
                        error.context("MCP initialize failed"),
                        cleanup,
                    ));
                }
            };
            if let Err(error) = validate_mcp_pin(&config, &outcome.identity) {
                let cleanup = client
                    .close_connection(format!("identity validation failed: {error:#}"))
                    .await;
                return Err(McpPostSpawnStartupError::new(
                    startup_receipt.clone(),
                    error.context("MCP identity validation failed"),
                    cleanup,
                ));
            }
            if client.identity.set(outcome.identity).is_err() {
                let cleanup = client
                    .close_connection("MCP client identity initialized more than once".to_owned())
                    .await;
                return Err(McpPostSpawnStartupError::new(
                    startup_receipt.clone(),
                    anyhow!("MCP client identity was initialized more than once"),
                    cleanup,
                ));
            }
            if client
                .server_capabilities
                .set(outcome.capabilities)
                .is_err()
            {
                let cleanup = client
                    .close_connection(
                        "MCP server capabilities initialized more than once".to_owned(),
                    )
                    .await;
                return Err(McpPostSpawnStartupError::new(
                    startup_receipt.clone(),
                    anyhow!("MCP server capabilities were initialized more than once"),
                    cleanup,
                ));
            }
            Ok(client)
        }
        .await;
        startup.map_err(Into::into)
    }

    pub(super) async fn initialize(
        &self,
        _config: &McpServerConfig,
        deadline: McpOperationDeadline,
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
                deadline,
            )
            .await?;
        let initialize = serde_json::from_value::<McpInitializeResult>(result)
            .context("failed to decode MCP initialize result")?;
        self.send_notification("notifications/initialized", json!({}), deadline)
            .await?;
        let server_info = initialize.server_info.unwrap_or(McpServerInfo {
            name: String::new(),
            version: String::new(),
        });
        let declaration = self._process_receipt.declaration.clone();
        let process_authorization_fingerprint = declaration
            .as_ref()
            .map(|declaration| declaration.authorization_fingerprint.clone())
            .unwrap_or_else(|| self._process_receipt.launch_static_fingerprint.clone());
        Ok(McpInitializeOutcome {
            identity: McpServerObservedIdentity {
                command_fingerprint: self._process_receipt.launch_static_fingerprint.clone(),
                process_authorization_fingerprint,
                declaration,
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
        self.server_capabilities()
            .get("resources")
            .is_some_and(Value::is_object)
    }

    pub(super) fn supports_prompts(&self) -> bool {
        self.server_capabilities()
            .get("prompts")
            .is_some_and(Value::is_object)
    }

    pub(super) fn identity(&self) -> &McpServerObservedIdentity {
        self.identity
            .get()
            .expect("MCP identity must be initialized before tools are registered")
    }

    fn server_capabilities(&self) -> &Value {
        self.server_capabilities
            .get()
            .expect("MCP capabilities must be initialized before tools are registered")
    }

    pub(super) async fn list_tools(self: &Arc<Self>) -> Result<Vec<McpToolDescriptor>> {
        let result = self
            .send_request("tools/list", json!({}), self.startup_deadline)
            .await?;
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

    async fn start_stderr_monitor(self: &Arc<Self>) {
        let receiver = self.stderr_fault_receiver.lock().await.take();
        let Some(receiver) = receiver else {
            return;
        };
        let weak_client = Arc::downgrade(self);
        let monitor_task = tokio::spawn(async move {
            if let Ok(fault) = receiver.await
                && let Some(client) = weak_client.upgrade()
            {
                client.close_for_stderr_limit(fault).await;
            }
        });
        *self
            ._stderr_monitor_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(monitor_task);
    }

    pub(super) async fn call_tool_response(
        &self,
        name: &str,
        args: Value,
        deadline: McpOperationDeadline,
    ) -> std::result::Result<Value, McpClientError> {
        self.send_request_response(
            "tools/call",
            json!({
                "name": name,
                "arguments": args,
            }),
            deadline,
        )
        .await
    }

    pub(super) async fn send_notification(
        &self,
        method: &str,
        params: Value,
        deadline: McpOperationDeadline,
    ) -> std::result::Result<(), McpClientError> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let operation = method.to_owned();
        let outcome = tokio::time::timeout_at(
            deadline.at,
            self.send_notification_inner(&operation, &message),
        )
        .await;
        self.finish_deadlined_operation(operation, deadline, outcome)
            .await
    }

    pub(super) async fn send_request(
        &self,
        method: &str,
        params: Value,
        deadline: McpOperationDeadline,
    ) -> Result<Value> {
        let response = self.send_request_response(method, params, deadline).await?;
        if let Some(error) = response.get("error") {
            let projection = bounded_mcp_protocol_error(
                &self.secret_redactor,
                error,
                &format!("MCP request {method} failed"),
            );
            bail!(projection.summary);
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP response missing result"))
    }

    pub(super) async fn send_request_response(
        &self,
        method: &str,
        params: Value,
        deadline: McpOperationDeadline,
    ) -> std::result::Result<Value, McpClientError> {
        let operation = method.to_owned();
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let outcome = tokio::time::timeout_at(
            deadline.at,
            self.send_request_response_inner(&operation, message),
        )
        .await;
        self.finish_deadlined_operation(operation, deadline, outcome)
            .await
    }

    async fn send_notification_inner(
        &self,
        operation: &str,
        message: &Value,
    ) -> std::result::Result<(), McpClientError> {
        let mut state = self.connection.lock().await;
        let connection = self.ready_connection(&mut state)?;
        self.ensure_environment_binding_current()
            .await
            .map_err(|source| McpClientError::Inbound {
                operation: operation.to_owned(),
                source,
            })?;
        write_ndjson_message(&mut connection.stdin, message)
            .await
            .map(|_| ())
            .map_err(|source| McpClientError::Framing {
                operation: operation.to_owned(),
                source,
            })
    }

    async fn send_request_response_inner(
        &self,
        operation: &str,
        mut message: Value,
    ) -> std::result::Result<Value, McpClientError> {
        let mut state = self.connection.lock().await;
        let connection = self.ready_connection(&mut state)?;
        self.ensure_environment_binding_current()
            .await
            .map_err(|source| McpClientError::Inbound {
                operation: operation.to_owned(),
                source,
            })?;
        connection.next_id = connection
            .next_id
            .checked_add(1)
            .ok_or(McpClientError::RequestIdExhausted)?;
        let id = connection.next_id;
        message["id"] = Value::Number(id.into());
        write_ndjson_message(&mut connection.stdin, &message)
            .await
            .map_err(|source| McpClientError::Framing {
                operation: operation.to_owned(),
                source,
            })?;

        let mut message_count = 0usize;
        let mut cumulative_bytes = 0usize;
        loop {
            if message_count >= MCP_OPERATION_MESSAGE_LIMIT {
                return Err(McpClientError::MessageLimit {
                    operation: operation.to_owned(),
                    limit: MCP_OPERATION_MESSAGE_LIMIT,
                    observed_at_least: message_count,
                });
            }
            let remaining_wire_bytes =
                MCP_OPERATION_CUMULATIVE_BYTES_LIMIT.saturating_sub(cumulative_bytes);
            if remaining_wire_bytes == 0 {
                return Err(McpClientError::CumulativeBytesLimit {
                    operation: operation.to_owned(),
                    limit_bytes: MCP_OPERATION_CUMULATIVE_BYTES_LIMIT,
                    observed_at_least_bytes: cumulative_bytes,
                });
            }
            let McpFrame {
                value: response,
                wire_bytes,
            } = match read_ndjson_message_with_wire_limit(
                &mut connection.stdout,
                remaining_wire_bytes,
            )
            .await
            {
                Ok(frame) => frame,
                Err(McpFramingError::WireBudgetExceeded {
                    observed_at_least_bytes,
                    ..
                }) => {
                    return Err(McpClientError::CumulativeBytesLimit {
                        operation: operation.to_owned(),
                        limit_bytes: MCP_OPERATION_CUMULATIVE_BYTES_LIMIT,
                        observed_at_least_bytes: cumulative_bytes
                            .saturating_add(observed_at_least_bytes),
                    });
                }
                Err(source) => {
                    return Err(McpClientError::Framing {
                        operation: operation.to_owned(),
                        source,
                    });
                }
            };
            message_count = message_count.saturating_add(1);
            cumulative_bytes = cumulative_bytes.saturating_add(wire_bytes);

            let object = response
                .as_object()
                .ok_or_else(|| McpClientError::InvalidEnvelope {
                    operation: operation.to_owned(),
                    reason: "top-level JSON-RPC message must be an object".to_owned(),
                })?;
            if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err(McpClientError::InvalidEnvelope {
                    operation: operation.to_owned(),
                    reason: "jsonrpc must equal 2.0".to_owned(),
                });
            }
            if let Some(method) = object.get("method") {
                if !method.is_string()
                    || object.contains_key("result")
                    || object.contains_key("error")
                {
                    return Err(McpClientError::InvalidEnvelope {
                        operation: operation.to_owned(),
                        reason:
                            "request/notification must have a string method and no response fields"
                                .to_owned(),
                    });
                }
                if object.get("id").is_some_and(|id| {
                    !id.is_string() && id.as_i64().is_none() && id.as_u64().is_none()
                }) {
                    return Err(McpClientError::InvalidEnvelope {
                        operation: operation.to_owned(),
                        reason: "request id must be a string or integer when present".to_owned(),
                    });
                }
                if object
                    .get("params")
                    .is_some_and(|params| !params.is_object())
                {
                    return Err(McpClientError::InvalidEnvelope {
                        operation: operation.to_owned(),
                        reason: "request params must be an object when present".to_owned(),
                    });
                }
                self.handle_inbound_message(connection, &response)
                    .await
                    .map_err(|source| McpClientError::Inbound {
                        operation: operation.to_owned(),
                        source,
                    })?;
                continue;
            }
            let has_result = object.contains_key("result");
            let has_error = object.contains_key("error");
            if has_result == has_error {
                return Err(McpClientError::InvalidEnvelope {
                    operation: operation.to_owned(),
                    reason: "response must contain exactly one of result or error".to_owned(),
                });
            }
            if has_error {
                let Some(error) = object.get("error").and_then(Value::as_object) else {
                    return Err(McpClientError::InvalidEnvelope {
                        operation: operation.to_owned(),
                        reason: "response error must be an object".to_owned(),
                    });
                };
                if error.get("code").and_then(Value::as_i64).is_none() {
                    return Err(McpClientError::InvalidEnvelope {
                        operation: operation.to_owned(),
                        reason: "response error code must be an integer".to_owned(),
                    });
                }
                if error.get("message").and_then(Value::as_str).is_none() {
                    return Err(McpClientError::InvalidEnvelope {
                        operation: operation.to_owned(),
                        reason: "response error message must be a string".to_owned(),
                    });
                }
            } else if object
                .get("result")
                .is_some_and(|result| !result.is_object())
            {
                return Err(McpClientError::InvalidEnvelope {
                    operation: operation.to_owned(),
                    reason: "success response result must be an object".to_owned(),
                });
            }
            let observed_id = object
                .get("id")
                .ok_or_else(|| McpClientError::InvalidEnvelope {
                    operation: operation.to_owned(),
                    reason: "response id is missing".to_owned(),
                })?;
            if observed_id.as_u64() == Some(id) {
                self.ensure_environment_binding_current()
                    .await
                    .map_err(|source| McpClientError::Inbound {
                        operation: operation.to_owned(),
                        source,
                    })?;
                return Ok(self.secret_redactor.redact_value(&response));
            }
            let (observed_type, observed_preview) = bounded_json_value_preview(observed_id);
            return Err(McpClientError::UnexpectedResponseId {
                operation: operation.to_owned(),
                observed_type,
                observed_preview,
            });
        }
    }

    fn ready_connection<'a>(
        &self,
        state: &'a mut McpConnectionState,
    ) -> std::result::Result<&'a mut Connection, McpClientError> {
        if (self
            .terminal_state
            .load(std::sync::atomic::Ordering::Acquire)
            != 0
            || self
                .stderr_faulted
                .load(std::sync::atomic::Ordering::Acquire))
            && matches!(state, McpConnectionState::Ready(_))
        {
            let record = self.terminal_record();
            let reason = record
                .as_ref()
                .map(|record| record.reason.clone())
                .unwrap_or_else(|| "connection teardown is in progress".to_owned());
            return Err(McpClientError::ConnectionClosed {
                server_name: self.server_name.clone(),
                reason,
                cause: record.and_then(|record| record.cause),
            });
        }
        match state {
            McpConnectionState::Ready(connection) => Ok(connection),
            McpConnectionState::Closing { reason } => Err(McpClientError::ConnectionClosed {
                server_name: self.server_name.clone(),
                reason: reason.clone(),
                cause: self.terminal_record().and_then(|record| record.cause),
            }),
            McpConnectionState::Closed {
                reason,
                cleanup_completed,
                stderr_total_bytes,
                stderr_truncated,
            } => Err(McpClientError::ConnectionClosed {
                server_name: self.server_name.clone(),
                reason: if *cleanup_completed {
                    format!(
                        "{reason}; stderr_total_bytes={stderr_total_bytes}; stderr_truncated={stderr_truncated}"
                    )
                } else {
                    format!("{reason}; cleanup incomplete")
                },
                cause: self.terminal_record().and_then(|record| record.cause),
            }),
        }
    }

    async fn finish_deadlined_operation<T>(
        &self,
        operation: String,
        deadline: McpOperationDeadline,
        outcome: std::result::Result<
            std::result::Result<T, McpClientError>,
            tokio::time::error::Elapsed,
        >,
    ) -> std::result::Result<T, McpClientError> {
        let result = match outcome {
            Ok(result) => result,
            Err(_) => Err(McpClientError::Timeout {
                operation,
                timeout_ms: deadline.timeout_ms,
            }),
        };
        let terminal_started = self
            .terminal_state
            .load(std::sync::atomic::Ordering::Acquire)
            != 0;
        let stderr_faulted = self
            .stderr_faulted
            .load(std::sync::atomic::Ordering::Acquire);
        if terminal_started || stderr_faulted {
            if stderr_faulted && !terminal_started {
                let fault = self
                    .stderr_fault
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
                    .unwrap_or(McpStderrFault::ReaderFailed {
                        total_bytes: 0,
                        reason: "stderr capture reported a terminal fault without a typed cause"
                            .to_owned(),
                    });
                self.poison_and_reap(fault.reason(), Some(fault.terminal_cause()), true)
                    .await;
            }
            let cleanup = self.cleanup_evidence().await;
            let record = self.terminal_record();
            let reason = record
                .as_ref()
                .map(|record| record.reason.clone())
                .unwrap_or_else(|| "MCP connection teardown completed without a reason".to_owned());
            return Err(McpClientError::ConnectionClosed {
                server_name: self.server_name.clone(),
                reason,
                cause: record.and_then(|record| record.cause),
            }
            .with_cleanup(cleanup));
        }
        match result {
            Err(error) if !matches!(error, McpClientError::ConnectionClosed { .. }) => {
                self.poison_and_reap(error.to_string(), None, true).await;
                let cleanup = self.cleanup_evidence().await;
                Err(error.with_cleanup(cleanup))
            }
            result => result,
        }
    }

    pub(super) async fn close_connection(&self, reason: String) -> McpCleanupEvidence {
        self.poison_and_reap(reason, None, false).await;
        self.cleanup_evidence().await
    }

    pub(super) async fn shutdown_generation(&self, reason: String) -> Result<()> {
        self.poison_and_reap(reason, None, true).await;
        let cleanup = self.cleanup_evidence().await;
        if cleanup.completed {
            Ok(())
        } else {
            bail!("MCP generation shutdown was incomplete: {}", cleanup.reason)
        }
    }

    async fn cleanup_evidence(&self) -> McpCleanupEvidence {
        const SHUTDOWN_COMPLETION_GRACE: Duration = Duration::from_secs(4);
        const MAX_CLEANUP_REASON_CHARS: usize = 512;
        let deadline = tokio::time::Instant::now() + SHUTDOWN_COMPLETION_GRACE;
        while self
            .terminal_state
            .load(std::sync::atomic::Ordering::Acquire)
            != 2
        {
            if tokio::time::Instant::now() >= deadline {
                return McpCleanupEvidence {
                    completed: false,
                    reason: "MCP generation shutdown did not reach a terminal cleanup state"
                        .to_owned(),
                };
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let Some(cleanup) = self.cleanup_outcome.lock().await.clone() else {
            return McpCleanupEvidence {
                completed: false,
                reason: "MCP generation shutdown completed without a process cleanup receipt"
                    .to_owned(),
            };
        };
        McpCleanupEvidence {
            completed: cleanup.completed,
            reason: cleanup
                .reason
                .chars()
                .take(MAX_CLEANUP_REASON_CHARS)
                .collect(),
        }
    }

    async fn close_for_stderr_limit(&self, fault: McpStderrFault) {
        self.poison_and_reap(fault.reason(), Some(fault.terminal_cause()), true)
            .await;
    }

    fn terminal_record(&self) -> Option<McpTerminalRecord> {
        self.terminal_record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn poison_and_reap(
        &self,
        reason: String,
        cause: Option<McpTerminalCause>,
        interrupt_in_flight: bool,
    ) {
        if !self.begin_terminal(reason.clone(), cause) {
            return;
        }

        let connection = if interrupt_in_flight {
            self.connection
                .try_lock()
                .ok()
                .and_then(|mut state| take_ready_connection(&mut state, &reason))
        } else {
            let mut state = self.connection.lock().await;
            take_ready_connection(&mut state, &reason)
        };
        let connection_was_busy = connection.is_none();
        #[cfg(windows)]
        let connection_until_tree_cleanup = connection;
        #[cfg(not(windows))]
        drop(connection);

        let mut cleanup = {
            let mut child_slot = self._child.lock().await;
            match child_slot.take() {
                Some(mut child) => terminate_mcp_process(&mut child).await,
                None => super::process::McpProcessCleanupSummary {
                    completed: true,
                    reason: "MCP process cleanup already completed".to_owned(),
                },
            }
        };
        #[cfg(windows)]
        drop(connection_until_tree_cleanup);

        let stderr = self.finish_stderr_capture().await;
        merge_stderr_capture_into_cleanup(&mut cleanup, &stderr);
        *self.cleanup_outcome.lock().await = Some(cleanup.clone());
        let closed_reason = if cleanup.completed {
            reason
        } else {
            format!("{reason}; cleanup incomplete: {}", cleanup.reason)
        };
        if interrupt_in_flight && connection_was_busy {
            if let Ok(mut state) = self.connection.try_lock() {
                drop(take_ready_connection(&mut state, &closed_reason));
                *state = McpConnectionState::Closed {
                    reason: closed_reason,
                    cleanup_completed: cleanup.completed,
                    stderr_total_bytes: stderr.summary.total_bytes,
                    stderr_truncated: stderr.summary.truncated,
                };
            }
        } else {
            let mut state = self.connection.lock().await;
            *state = McpConnectionState::Closed {
                reason: closed_reason,
                cleanup_completed: cleanup.completed,
                stderr_total_bytes: stderr.summary.total_bytes,
                stderr_truncated: stderr.summary.truncated,
            };
        }
        self.terminal_state
            .store(2, std::sync::atomic::Ordering::Release);
    }

    fn begin_terminal(&self, reason: String, cause: Option<McpTerminalCause>) -> bool {
        publish_terminal_record(&self.terminal_state, &self.terminal_record, reason, cause)
    }

    async fn finish_stderr_capture(&self) -> McpStderrCaptureCompletion {
        let stderr_task = self._stderr_task.lock().await.take();
        finish_stderr_task(stderr_task, Duration::from_secs(1)).await
    }

    async fn ensure_environment_binding_current(&self) -> Result<()> {
        let current = match resolve_extension_process_environment(
            &self._process_receipt.environment_grant_names,
        ) {
            Ok(current) => current,
            Err(error) => return Err(error.into()),
        };
        if environment_binding_matches(&self._process_receipt, &current) {
            return Ok(());
        }
        Err(ExtensionProcessLaunchError::environment_binding_changed(
            &self.server_name,
            format!(
                "MCP server {} environment binding changed; restart or refresh the server before retrying",
                self.server_name
            ),
        )
        .into())
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

fn publish_terminal_record(
    terminal_state: &std::sync::atomic::AtomicU8,
    terminal_record: &std::sync::Mutex<Option<McpTerminalRecord>>,
    reason: String,
    cause: Option<McpTerminalCause>,
) -> bool {
    let mut terminal_record = terminal_record
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if terminal_state
        .compare_exchange(
            0,
            1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    *terminal_record = Some(McpTerminalRecord { reason, cause });
    true
}

async fn finish_stderr_task(
    stderr_task: Option<JoinHandle<McpStderrSummary>>,
    max_wait: Duration,
) -> McpStderrCaptureCompletion {
    const MAX_STDERR_FAILURE_CHARS: usize = 512;
    let Some(mut stderr_task) = stderr_task else {
        return McpStderrCaptureCompletion {
            summary: McpStderrSummary::default(),
            failure: Some("MCP stderr drain task was unavailable during cleanup".to_owned()),
        };
    };
    match tokio::time::timeout(max_wait, &mut stderr_task).await {
        Ok(Ok(summary)) => McpStderrCaptureCompletion {
            summary,
            failure: None,
        },
        Ok(Err(error)) => McpStderrCaptureCompletion {
            summary: McpStderrSummary::default(),
            failure: Some(
                format!("MCP stderr drain task failed: {error}")
                    .chars()
                    .take(MAX_STDERR_FAILURE_CHARS)
                    .collect(),
            ),
        },
        Err(_) => {
            stderr_task.abort();
            let _ = stderr_task.await;
            McpStderrCaptureCompletion {
                summary: McpStderrSummary::default(),
                failure: Some(
                    "MCP stderr drain exceeded its bounded grace and was aborted".to_owned(),
                ),
            }
        }
    }
}

fn merge_stderr_capture_into_cleanup(
    cleanup: &mut McpProcessCleanupSummary,
    stderr: &McpStderrCaptureCompletion,
) {
    if let Some(stderr_failure) = &stderr.failure {
        cleanup.completed = false;
        cleanup.reason = format!("{}; {stderr_failure}", cleanup.reason);
    }
}

fn take_ready_connection(state: &mut McpConnectionState, reason: &str) -> Option<Connection> {
    match std::mem::replace(
        state,
        McpConnectionState::Closing {
            reason: reason.to_owned(),
        },
    ) {
        McpConnectionState::Ready(connection) => Some(connection),
        existing @ (McpConnectionState::Closing { .. } | McpConnectionState::Closed { .. }) => {
            *state = existing;
            None
        }
    }
}

fn bounded_json_value_preview(value: &Value) -> (&'static str, String) {
    match value {
        Value::Null => ("null", "null".to_owned()),
        Value::Bool(value) => ("boolean", value.to_string()),
        Value::Number(value) => ("number", value.to_string()),
        Value::String(value) => (
            "string",
            format!(
                "string(bytes={},chars={})",
                value.len(),
                value.chars().count()
            ),
        ),
        Value::Array(values) => ("array", format!("array(len={})", values.len())),
        Value::Object(values) => ("object", format!("object(keys={})", values.len())),
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
        request
            .declaration
            .as_ref()
            .map(|declaration| declaration.authorization_fingerprint.as_str())
            .unwrap_or(&request.launch_static_fingerprint),
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
    write_ndjson_message(stdin, value).await?;
    Ok(())
}

#[cfg(test)]
pub(super) async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Value> {
    read_ndjson_message(stdout)
        .await
        .map(|frame| frame.value)
        .map_err(Into::into)
}

#[cfg(test)]
#[path = "tests/client_tests.rs"]
mod tests;
