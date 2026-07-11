use super::*;

#[derive(Debug, Clone)]
struct LiveSession {
    id: SecretString,
    version: McpRemoteProtocolVersion,
    tools_list_changed: bool,
    server_identity: Option<McpRemoteServerIdentity>,
}

fn valid_server_identity_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

fn server_identity_fingerprint(name: &str, version: &str) -> String {
    use sha2::Digest;

    format!(
        "{:x}",
        sha2::Sha256::digest(format!("{name}\0{version}").as_bytes())
    )
}

fn coarse_client_version() -> String {
    env!("CARGO_PKG_VERSION")
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".")
}

#[derive(Debug)]
struct ClientState {
    lifecycle: McpStreamableHttpLifecycle,
    session: Option<LiveSession>,
    next_id: u64,
    list_change_pending: bool,
    last_list_change_accepted: Option<tokio::time::Instant>,
    active_form: bool,
}

/// Internal-only MCP Streamable HTTP state machine. No root config or registry constructor exposes
/// this type; E21.17 owns public cutover.
pub struct McpStreamableHttpClient {
    endpoint: Url,
    authorizer: Arc<dyn McpStreamableHttpDestinationAuthorizer>,
    pending_plan: Mutex<Option<McpStreamableHttpAuthorizedDialPlan>>,
    headers: ResolvedHeaders,
    capabilities: McpRemoteClientCapabilities,
    roots: Vec<McpRemoteRoot>,
    form_handler: Option<Arc<dyn McpRemoteFormHandler>>,
    limits: McpStreamableHttpLimits,
    state: Mutex<ClientState>,
}

impl fmt::Debug for McpStreamableHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpClient")
            .field("safe_endpoint", &safe_origin(&self.endpoint))
            .field("headers", &self.headers)
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

impl McpStreamableHttpClient {
    pub async fn connect(
        authorizer: Arc<dyn McpStreamableHttpDestinationAuthorizer>,
        header_config: &McpStreamableHttpHeaderConfig,
        header_environment: &dyn McpStreamableHttpHeaderEnvironment,
        capabilities: McpRemoteClientCapabilities,
        limits: McpStreamableHttpLimits,
    ) -> Result<Arc<Self>, McpStreamableHttpError> {
        let prepared = PreparedMcpStreamableHttpHeaders::prepare(
            authorizer.endpoint(),
            header_config,
            header_environment,
        )?;
        Self::connect_prepared(authorizer, prepared, capabilities, limits).await
    }

    pub async fn connect_prepared(
        authorizer: Arc<dyn McpStreamableHttpDestinationAuthorizer>,
        prepared: PreparedMcpStreamableHttpHeaders,
        capabilities: McpRemoteClientCapabilities,
        limits: McpStreamableHttpLimits,
    ) -> Result<Arc<Self>, McpStreamableHttpError> {
        Self::connect_with_inbound_prepared(
            authorizer,
            prepared,
            capabilities,
            limits,
            Vec::new(),
            None,
        )
        .await
    }

    pub async fn connect_with_inbound(
        authorizer: Arc<dyn McpStreamableHttpDestinationAuthorizer>,
        header_config: &McpStreamableHttpHeaderConfig,
        header_environment: &dyn McpStreamableHttpHeaderEnvironment,
        capabilities: McpRemoteClientCapabilities,
        limits: McpStreamableHttpLimits,
        roots: Vec<McpRemoteRoot>,
        form_handler: Option<Arc<dyn McpRemoteFormHandler>>,
    ) -> Result<Arc<Self>, McpStreamableHttpError> {
        let prepared = PreparedMcpStreamableHttpHeaders::prepare(
            authorizer.endpoint(),
            header_config,
            header_environment,
        )?;
        Self::connect_with_inbound_prepared(
            authorizer,
            prepared,
            capabilities,
            limits,
            roots,
            form_handler,
        )
        .await
    }

    pub async fn connect_with_inbound_prepared(
        authorizer: Arc<dyn McpStreamableHttpDestinationAuthorizer>,
        prepared: PreparedMcpStreamableHttpHeaders,
        capabilities: McpRemoteClientCapabilities,
        limits: McpStreamableHttpLimits,
        roots: Vec<McpRemoteRoot>,
        form_handler: Option<Arc<dyn McpRemoteFormHandler>>,
    ) -> Result<Arc<Self>, McpStreamableHttpError> {
        let limits = limits.validate()?;
        if roots.len() > 32
            || (!capabilities.roots && !roots.is_empty())
            || (!capabilities.form_elicitation && form_handler.is_some())
        {
            return Err(McpStreamableHttpError::ConfigurationInvalid);
        }
        let PreparedMcpStreamableHttpHeaders {
            endpoint_secret,
            endpoint,
            headers,
        } = prepared;
        if authorizer.endpoint().expose_secret() != endpoint_secret.expose_secret() {
            return Err(McpStreamableHttpError::ConfigurationInvalid);
        }
        let expected_profile_fingerprint = authorizer.profile_config_proxy_fingerprint();
        let expected_live_header_fingerprint = authorizer.live_header_fingerprint();
        validate_fingerprint(&expected_profile_fingerprint)?;
        validate_fingerprint(&expected_live_header_fingerprint)?;
        if headers.live_fingerprint != expected_live_header_fingerprint {
            return Err(McpStreamableHttpError::ConfigurationInvalid);
        }
        let plan = authorizer.authorize_destination().await?;
        if plan.endpoint.expose_secret() != endpoint_secret.expose_secret() {
            return Err(McpStreamableHttpError::InvalidDialPlan);
        }
        if plan.profile_config_proxy_fingerprint != expected_profile_fingerprint
            || plan.live_header_fingerprint != expected_live_header_fingerprint
        {
            return Err(McpStreamableHttpError::InvalidDialPlan);
        }
        let instance = Arc::new(Self {
            endpoint,
            authorizer,
            pending_plan: Mutex::new(Some(plan)),
            headers,
            capabilities,
            roots,
            form_handler,
            limits,
            state: Mutex::new(ClientState {
                lifecycle: McpStreamableHttpLifecycle::Disconnected,
                session: None,
                next_id: 0,
                list_change_pending: false,
                last_list_change_accepted: None,
                active_form: false,
            }),
        });
        instance.initialize().await?;
        Ok(instance)
    }

    #[must_use]
    pub fn auth_state(&self) -> McpStreamableHttpAuthState {
        if self.headers.has_static_credential {
            McpStreamableHttpAuthState::StaticCredential
        } else {
            McpStreamableHttpAuthState::Anonymous
        }
    }

    #[must_use]
    pub fn live_header_fingerprint(&self) -> &str {
        &self.headers.live_fingerprint
    }

    pub async fn lifecycle(&self) -> McpStreamableHttpLifecycle {
        self.state.lock().await.lifecycle
    }

    pub async fn protocol_version(&self) -> Option<McpRemoteProtocolVersion> {
        self.state
            .lock()
            .await
            .session
            .as_ref()
            .map(|session| session.version)
    }

    pub async fn server_identity(&self) -> Option<McpRemoteServerIdentity> {
        self.state
            .lock()
            .await
            .session
            .as_ref()
            .and_then(|session| session.server_identity.clone())
    }

    #[must_use]
    pub fn transport_fingerprint(&self) -> String {
        self.authorizer.transport_fingerprint()
    }

    #[must_use]
    pub fn profile_config_proxy_fingerprint(&self) -> String {
        self.authorizer.profile_config_proxy_fingerprint()
    }

    async fn initialize(&self) -> Result<(), McpStreamableHttpError> {
        {
            let mut state = self.state.lock().await;
            if !matches!(
                state.lifecycle,
                McpStreamableHttpLifecycle::Disconnected | McpStreamableHttpLifecycle::Closed
            ) {
                return Err(McpStreamableHttpError::InvalidLifecycle);
            }
            state.lifecycle = McpStreamableHttpLifecycle::Initializing;
            state.session = None;
        }
        let request_id = self.next_id().await?;
        let response = self
            .post_request(
                request_id,
                "initialize",
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": self.capabilities.wire(McpRemoteProtocolVersion::V2025_11_25),
                    "clientInfo": { "name": "Sigil", "version": coarse_client_version() }
                }),
                None,
            )
            .await?;
        let result = rpc_result(&response, request_id)?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or(McpStreamableHttpError::UnsupportedProtocolVersion)
            .and_then(McpRemoteProtocolVersion::parse)?;
        let tools = result
            .get("capabilities")
            .and_then(Value::as_object)
            .and_then(|capabilities| capabilities.get("tools"))
            .and_then(Value::as_object)
            .ok_or(McpStreamableHttpError::MissingToolsCapability)?;
        let tools_list_changed = tools
            .get("listChanged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let server_info = result
            .get("serverInfo")
            .and_then(Value::as_object)
            .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
        let server_name = server_info
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| valid_server_identity_text(value))
            .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
        let server_version = server_info
            .get("version")
            .and_then(Value::as_str)
            .filter(|value| valid_server_identity_text(value))
            .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
        let server_identity = McpRemoteServerIdentity {
            name: server_name.to_owned(),
            version: server_version.to_owned(),
            fingerprint: server_identity_fingerprint(server_name, server_version),
        };
        let staged_session = response.session_id;
        {
            let mut state = self.state.lock().await;
            state.lifecycle = McpStreamableHttpLifecycle::InitializedNotificationPending;
        }
        self.post_notification(
            "notifications/initialized",
            json!({}),
            Some((version, staged_session.as_ref())),
        )
        .await
        .map_err(|error| match error {
            McpStreamableHttpError::UnexpectedHttpStatus { .. }
            | McpStreamableHttpError::UnexpectedContentType
            | McpStreamableHttpError::MalformedEnvelope => {
                McpStreamableHttpError::InitializedNotificationRejected
            }
            other => other,
        })?;
        let mut state = self.state.lock().await;
        state.session = Some(LiveSession {
            id: staged_session.unwrap_or_else(|| SecretString::new(String::new())),
            version,
            tools_list_changed,
            server_identity: Some(server_identity),
        });
        state.lifecycle = McpStreamableHttpLifecycle::Ready;
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpRemoteTool>, McpStreamableHttpError> {
        self.ensure_ready_for_new_operation().await?;
        let mut cursor = None::<String>;
        let mut seen = BTreeSet::new();
        let mut tools = Vec::new();
        let mut total_bytes = 0usize;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let response = self.request("tools/list", params).await?;
            let result = rpc_result(&response, response.expected_id)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
            total_bytes = total_bytes
                .checked_add(
                    serde_json::to_vec(page)
                        .map_err(|_| McpStreamableHttpError::MalformedEnvelope)?
                        .len(),
                )
                .ok_or(McpStreamableHttpError::InvalidPagination)?;
            if total_bytes > self.limits.max_body_bytes
                || tools.len().saturating_add(page.len()) > MAX_TOOLS
            {
                return Err(McpStreamableHttpError::InvalidPagination);
            }
            for tool in page {
                let parsed: McpRemoteTool = serde_json::from_value(tool.clone())
                    .map_err(|_| McpStreamableHttpError::MalformedEnvelope)?;
                parsed.validate()?;
                tools.push(parsed);
            }
            let next = match result.get("nextCursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(value)) if value.len() <= 4096 => Some(value.as_str()),
                _ => return Err(McpStreamableHttpError::InvalidPagination),
            };
            match next {
                None => return Ok(tools),
                Some(value) if value.is_empty() || !seen.insert(value.to_owned()) => {
                    return Err(McpStreamableHttpError::InvalidPagination);
                }
                Some(value) => cursor = Some(value.to_owned()),
            }
        }
        Err(McpStreamableHttpError::InvalidPagination)
    }

    pub async fn call_tool(
        &self,
        tool: &McpRemoteTool,
        arguments: Value,
        cancellation: Option<&sigil_kernel::RunCancellationHandle>,
        authorization_is_live: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<McpCallToolResult, McpStreamableHttpError> {
        self.call_tool_with_body_observer(
            tool,
            arguments,
            cancellation,
            authorization_is_live,
            None,
        )
        .await
    }

    pub async fn call_tool_with_body_observer(
        &self,
        tool: &McpRemoteTool,
        arguments: Value,
        cancellation: Option<&sigil_kernel::RunCancellationHandle>,
        authorization_is_live: &(dyn Fn() -> bool + Send + Sync),
        body_observer: Option<Arc<dyn McpRequestBodyObserver>>,
    ) -> Result<McpCallToolResult, McpStreamableHttpError> {
        self.ensure_ready_for_new_operation().await?;
        let input = CompiledMcpSchema::compile(&tool.input_schema)?;
        input.validate(&arguments)?;
        let id = self.next_id().await?;
        let session = self.live_session_snapshot().await?;
        let request_started = Arc::new(AtomicBool::new(false));
        let operation = self.post_request_with_started(
            id,
            "tools/call",
            json!({ "name": tool.name, "arguments": arguments }),
            session.as_ref(),
            Some(Arc::clone(&request_started)),
            body_observer,
        );
        tokio::pin!(operation);
        let response = match cancellation {
            Some(cancellation) => tokio::select! {
                biased;
                response = &mut operation => response,
                _ = cancellation.cancelled() => {
                    if request_started.load(Ordering::Acquire) && authorization_is_live() {
                        let _ = self.send_cancelled_once(id).await;
                    }
                    return Err(McpStreamableHttpError::Cancelled);
                }
            },
            None => operation.await,
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => return Err(error),
        };
        let result = rpc_result(&response, id)?;
        let parsed = McpCallToolResult::parse(result)?;
        if let Some(output_schema) = tool.output_schema.as_ref() {
            let compiled = CompiledMcpSchema::compile(output_schema)?;
            let structured = parsed
                .structured_content
                .as_ref()
                .ok_or(McpStreamableHttpError::SchemaDrift)?;
            compiled.validate(structured)?;
        }
        Ok(parsed)
    }

    pub async fn note_tools_list_changed(&self) -> Result<bool, McpStreamableHttpError> {
        let mut state = self.state.lock().await;
        let Some(session) = state.session.as_ref() else {
            return Err(McpStreamableHttpError::InvalidLifecycle);
        };
        if !session.tools_list_changed {
            return Err(McpStreamableHttpError::CapabilityNotNegotiated);
        }
        if state.list_change_pending {
            return Ok(false);
        }
        let now = tokio::time::Instant::now();
        if state
            .last_list_change_accepted
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(250))
        {
            return Ok(false);
        }
        state.list_change_pending = true;
        state.last_list_change_accepted = Some(now);
        Ok(true)
    }

    pub async fn take_tools_list_changed(&self) -> bool {
        let mut state = self.state.lock().await;
        std::mem::take(&mut state.list_change_pending)
    }

    pub async fn probe_get_listener(&self) -> Result<bool, McpStreamableHttpError> {
        let session = self.live_session_snapshot().await?;
        let response = self
            .send(
                Method::GET,
                None,
                session.as_ref(),
                Some("text/event-stream"),
                None,
                None,
            )
            .await?;
        if response.status == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(false);
        }
        self.normalize_response_status(response.status, &response.headers, session.is_some())
            .await?;
        if response.status.is_redirection() {
            return Err(McpStreamableHttpError::UnexpectedHttpStatus {
                status: response.status.as_u16(),
            });
        }
        if !response.status.is_success() {
            return Err(McpStreamableHttpError::UnexpectedHttpStatus {
                status: response.status.as_u16(),
            });
        }
        let content_type = single_header(&response.headers, CONTENT_TYPE)?
            .ok_or(McpStreamableHttpError::UnexpectedContentType)?;
        if !matches_content_type(&content_type, "text/event-stream")? {
            return Err(McpStreamableHttpError::UnexpectedContentType);
        }
        for message in parse_sse_messages(&response.body, self.limits)? {
            self.handle_inbound_message(&message, None).await?;
        }
        Ok(true)
    }

    pub async fn close(&self, authorization_is_live: bool) -> Result<(), McpStreamableHttpError> {
        let session = {
            let mut state = self.state.lock().await;
            if matches!(state.lifecycle, McpStreamableHttpLifecycle::Closed) {
                return Ok(());
            }
            state.lifecycle = McpStreamableHttpLifecycle::Closing;
            state.session.clone()
        };
        if authorization_is_live && session.is_some() {
            let response = self
                .send(Method::DELETE, None, session.as_ref(), None, None, None)
                .await?;
            self.normalize_response_status(response.status, &response.headers, session.is_some())
                .await?;
            if !response.status.is_success() && response.status != StatusCode::METHOD_NOT_ALLOWED {
                return Err(McpStreamableHttpError::UnexpectedHttpStatus {
                    status: response.status.as_u16(),
                });
            }
        }
        let mut state = self.state.lock().await;
        state.session = None;
        state.lifecycle = McpStreamableHttpLifecycle::Closed;
        Ok(())
    }

    pub async fn validate_form_request(
        &self,
        params: &Value,
    ) -> Result<ValidatedMcpFormRequest, McpStreamableHttpError> {
        if !self.capabilities.form_elicitation {
            return Err(McpStreamableHttpError::CapabilityNotNegotiated);
        }
        let mut state = self.state.lock().await;
        if state.active_form {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let version = state
            .session
            .as_ref()
            .ok_or(McpStreamableHttpError::InvalidLifecycle)?
            .version;
        let request = ValidatedMcpFormRequest::parse_for_version(params, version)?;
        state.active_form = true;
        Ok(request)
    }

    pub async fn finish_form(
        &self,
        request: &ValidatedMcpFormRequest,
        content: &Value,
    ) -> Result<(), McpStreamableHttpError> {
        let result = request.validate_response(content);
        self.state.lock().await.active_form = false;
        result
    }

    pub fn reject_server_method(method: &str, params: &Value) -> McpStreamableHttpError {
        if method == "notifications/elicitation/complete" {
            return McpStreamableHttpError::CapabilityNotNegotiated;
        }
        if method == "elicitation/create"
            && params.get("mode").and_then(Value::as_str) == Some("url")
        {
            return McpStreamableHttpError::UrlElicitationUnsupported;
        }
        if matches!(
            method,
            "sampling/createMessage" | "tasks/create" | "tasks/get"
        ) {
            return McpStreamableHttpError::CapabilityNotNegotiated;
        }
        McpStreamableHttpError::CapabilityNotNegotiated
    }

    /// Handles a validated inbound JSON-RPC server request and posts exactly one response.
    /// Raw roots and form values never enter this path unless the corresponding capability was
    /// explicitly enabled for this remote transport.
    pub async fn handle_inbound_request(
        &self,
        message: &Value,
    ) -> Result<(), McpStreamableHttpError> {
        let object = message
            .as_object()
            .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        };
        if method == "notifications/elicitation/complete" {
            return Err(McpStreamableHttpError::CapabilityNotNegotiated);
        }
        let id = validate_server_request_id(
            object
                .get("id")
                .ok_or(McpStreamableHttpError::MalformedEnvelope)?,
        )?;
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "ping" => json!({}),
            "roots/list" if self.capabilities.roots => json!({ "roots": self.roots }),
            "elicitation/create" if self.capabilities.form_elicitation => {
                let request = match self.validate_form_request(&params).await {
                    Ok(request) => request,
                    Err(McpStreamableHttpError::UrlElicitationUnsupported)
                    | Err(McpStreamableHttpError::InvalidForm) => {
                        return self.post_error_response(id, -32602).await;
                    }
                    Err(error) => return Err(error),
                };
                let Some(handler) = self.form_handler.as_ref() else {
                    self.state.lock().await.active_form = false;
                    return self.post_error_response(id, -32601).await;
                };
                let response = handler.handle_form(request.clone()).await;
                self.state.lock().await.active_form = false;
                match response? {
                    McpRemoteFormResponse::Accept(content) => {
                        request.validate_response(&content)?;
                        json!({ "action": "accept", "content": content })
                    }
                    McpRemoteFormResponse::Decline => json!({ "action": "decline" }),
                    McpRemoteFormResponse::Cancel => json!({ "action": "cancel" }),
                }
            }
            "elicitation/create" | "sampling/createMessage" | "tasks/create" | "tasks/get" => {
                return self.post_error_response(id, -32601).await;
            }
            _ => return self.post_error_response(id, -32601).await,
        };
        self.post_response(id, result).await
    }

    async fn handle_inbound_message(
        &self,
        message: &Value,
        staged_session: Option<&LiveSession>,
    ) -> Result<(), McpStreamableHttpError> {
        let method = message.get("method").and_then(Value::as_str);
        match (method, message.get("id")) {
            (Some("ping"), Some(id)) if staged_session.is_some() => {
                let id = validate_server_request_id(id)?;
                self.post_response_with_session(id, json!({}), staged_session)
                    .await
            }
            (Some("notifications/tools/list_changed"), None) => {
                self.note_tools_list_changed().await.map(|_| ())
            }
            (Some("notifications/elicitation/complete"), None) => Ok(()),
            (Some(_), Some(_)) => self.handle_inbound_request(message).await,
            (Some(_), None) => Ok(()),
            _ => Err(McpStreamableHttpError::MalformedEnvelope),
        }
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcResponse, McpStreamableHttpError> {
        let id = self.next_id().await?;
        self.post_request(
            id,
            method,
            params,
            self.live_session_snapshot().await?.as_ref(),
        )
        .await
    }

    async fn post_request(
        &self,
        id: u64,
        method: &str,
        params: Value,
        session: Option<&LiveSession>,
    ) -> Result<RpcResponse, McpStreamableHttpError> {
        self.post_request_with_started(id, method, params, session, None, None)
            .await
    }

    async fn post_request_with_started(
        &self,
        id: u64,
        method: &str,
        params: Value,
        session: Option<&LiveSession>,
        request_started: Option<Arc<AtomicBool>>,
        body_observer: Option<Arc<dyn McpRequestBodyObserver>>,
    ) -> Result<RpcResponse, McpStreamableHttpError> {
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let response = self
            .send(
                Method::POST,
                Some(body),
                session,
                None,
                request_started,
                body_observer,
            )
            .await?;
        self.normalize_response_status(response.status, &response.headers, session.is_some())
            .await?;
        if response.status != StatusCode::OK {
            return Err(McpStreamableHttpError::UnexpectedHttpStatus {
                status: response.status.as_u16(),
            });
        }
        let response_session_id = validate_session_header(&response.headers)?;
        let session_id = if method == "initialize" {
            response_session_id
        } else {
            if let Some(received) = response_session_id {
                let expected = session
                    .map(|session| session.id.expose_secret())
                    .ok_or(McpStreamableHttpError::InvalidSessionId)?;
                if received.expose_secret() != expected {
                    return Err(McpStreamableHttpError::InvalidSessionId);
                }
            }
            None
        };
        let content_type = single_header(&response.headers, CONTENT_TYPE)?
            .ok_or(McpStreamableHttpError::UnexpectedContentType)?;
        let body = response.body;
        let (envelope, inbound) = if matches_content_type(&content_type, "application/json")? {
            serde_json::from_slice::<Value>(&body)
                .map(|value| (value, Vec::new()))
                .map_err(|_| McpStreamableHttpError::MalformedEnvelope)?
        } else if matches_content_type(&content_type, "text/event-stream")? {
            parse_sse_response(&body, id, self.limits)?
        } else {
            return Err(McpStreamableHttpError::UnexpectedContentType);
        };
        validate_response_envelope(&envelope, id)?;
        let staged_inbound_session = if method == "initialize" {
            let version = envelope
                .get("result")
                .and_then(|result| result.get("protocolVersion"))
                .and_then(Value::as_str)
                .ok_or(McpStreamableHttpError::UnsupportedProtocolVersion)
                .and_then(McpRemoteProtocolVersion::parse)?;
            Some(LiveSession {
                id: session_id
                    .clone()
                    .unwrap_or_else(|| SecretString::new(String::new())),
                version,
                tools_list_changed: false,
                server_identity: None,
            })
        } else {
            None
        };
        for message in inbound {
            self.handle_inbound_message(&message, staged_inbound_session.as_ref())
                .await?;
        }
        Ok(RpcResponse {
            value: envelope,
            expected_id: id,
            session_id,
        })
    }

    async fn post_notification(
        &self,
        method: &str,
        params: Value,
        session: Option<(McpRemoteProtocolVersion, Option<&SecretString>)>,
    ) -> Result<(), McpStreamableHttpError> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let live = session.map(|(version, id)| LiveSession {
            id: id
                .cloned()
                .unwrap_or_else(|| SecretString::new(String::new())),
            version,
            tools_list_changed: false,
            server_identity: None,
        });
        let response = self
            .send(Method::POST, Some(body), live.as_ref(), None, None, None)
            .await?;
        self.normalize_response_status(response.status, &response.headers, live.is_some())
            .await?;
        if response.status != StatusCode::ACCEPTED {
            return Err(McpStreamableHttpError::UnexpectedHttpStatus {
                status: response.status.as_u16(),
            });
        }
        let body = response.body;
        if !body.is_empty() {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        }
        Ok(())
    }

    async fn send_cancelled_once(&self, request_id: u64) -> Result<(), McpStreamableHttpError> {
        let session = self.live_session_snapshot().await?;
        self.post_notification(
            "notifications/cancelled",
            json!({ "requestId": request_id }),
            session
                .as_ref()
                .map(|session| (session.version, Some(&session.id))),
        )
        .await
    }

    async fn post_response(&self, id: Value, result: Value) -> Result<(), McpStreamableHttpError> {
        self.post_response_with_session(id, result, None).await
    }

    async fn post_response_with_session(
        &self,
        id: Value,
        result: Value,
        staged_session: Option<&LiveSession>,
    ) -> Result<(), McpStreamableHttpError> {
        self.post_response_envelope_with_session(
            json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            staged_session,
        )
        .await
    }

    async fn post_error_response(
        &self,
        id: Value,
        code: i64,
    ) -> Result<(), McpStreamableHttpError> {
        self.post_response_envelope_with_session(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": "remote MCP request rejected" }
            }),
            None,
        )
        .await
    }

    async fn post_response_envelope_with_session(
        &self,
        body: Value,
        staged_session: Option<&LiveSession>,
    ) -> Result<(), McpStreamableHttpError> {
        let live = if let Some(session) = staged_session {
            Some(session.clone())
        } else {
            self.live_session_snapshot().await?
        };
        let response = self
            .send(Method::POST, Some(body), live.as_ref(), None, None, None)
            .await?;
        self.normalize_response_status(response.status, &response.headers, live.is_some())
            .await?;
        if response.status != StatusCode::ACCEPTED {
            return Err(McpStreamableHttpError::UnexpectedHttpStatus {
                status: response.status.as_u16(),
            });
        }
        if !response.body.is_empty() {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        }
        Ok(())
    }

    async fn send(
        &self,
        method: Method,
        body: Option<Value>,
        session: Option<&LiveSession>,
        accept_override: Option<&str>,
        request_started: Option<Arc<AtomicBool>>,
        body_observer: Option<Arc<dyn McpRequestBodyObserver>>,
    ) -> Result<McpHttpResponse, McpStreamableHttpError> {
        let mut plan = self
            .pending_plan
            .lock()
            .await
            .take()
            .map_or_else(|| None, Some);
        if plan.is_none() {
            plan = Some(self.authorizer.authorize_destination().await?);
        }
        let mut plan = plan.ok_or(McpStreamableHttpError::InvalidDialPlan)?;
        if Url::parse(plan.endpoint.expose_secret())
            .map_err(|_| McpStreamableHttpError::InvalidDialPlan)?
            != self.endpoint
        {
            return Err(McpStreamableHttpError::InvalidDialPlan);
        }
        if plan.profile_config_proxy_fingerprint
            != self.authorizer.profile_config_proxy_fingerprint()
            || plan.live_header_fingerprint != self.headers.live_fingerprint
            || plan.live_header_fingerprint != self.authorizer.live_header_fingerprint()
        {
            return Err(McpStreamableHttpError::InvalidDialPlan);
        }
        let client = build_client(&plan)?;
        let mut budget = plan.take_budget()?;
        let mut request = client.request(method, self.endpoint.clone());
        request = request.header(
            ACCEPT,
            accept_override.unwrap_or("application/json, text/event-stream"),
        );
        request = request.header(CONNECTION, "close");
        if body.is_some() {
            request = request.header(CONTENT_TYPE, "application/json");
        }
        for (name, value) in &self.headers.values {
            let value = HeaderValue::from_str(value.expose_secret())
                .map_err(|_| McpStreamableHttpError::ConfigurationInvalid)?;
            request = request.header(name, value);
        }
        if let Some(session) = session {
            request = request.header(MCP_VERSION_HEADER, session.version.as_str());
            if !session.id.expose_secret().is_empty() {
                request = request.header(MCP_SESSION_HEADER, session.id.expose_secret());
            }
        }
        if let Some(body) = body {
            let encoded =
                serde_json::to_vec(&body).map_err(|_| McpStreamableHttpError::MalformedEnvelope)?;
            if encoded.len() > self.limits.max_body_bytes {
                return Err(McpStreamableHttpError::BodyLimitExceeded);
            }
            budget
                .charge_chunk(WebBudgetByteKind::Wire, encoded.len() as u64)
                .map_err(|_| McpStreamableHttpError::BudgetExhausted)?;
            if let Some(started) = request_started {
                let mut encoded = Some(encoded);
                let mut observer = body_observer;
                let stream = futures::stream::poll_fn(move |_| {
                    if let Some(observer) = observer.take()
                        && observer.on_first_body_poll().is_err()
                    {
                        encoded.take();
                        return std::task::Poll::Ready(Some(Err(std::io::Error::other(
                            "request body authorization barrier failed",
                        ))));
                    }
                    started.store(true, Ordering::Release);
                    std::task::Poll::Ready(encoded.take().map(Ok::<Vec<u8>, std::io::Error>))
                });
                request = request.body(reqwest::Body::wrap_stream(stream));
            } else {
                request = request.body(encoded);
            }
        }
        let response =
            match tokio::time::timeout(self.limits.response_timeout, request.send()).await {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => {
                    return Err(McpStreamableHttpError::Transport);
                }
                Err(_) => {
                    return Err(McpStreamableHttpError::Timeout);
                }
            };
        let status = response.status();
        let headers = response.headers().clone();
        let body = match read_bounded_body(response, self.limits, &mut budget).await {
            Ok(body) => body,
            Err(error) => {
                return Err(error);
            }
        };
        Ok(McpHttpResponse {
            status,
            headers,
            body,
        })
    }

    async fn next_id(&self) -> Result<u64, McpStreamableHttpError> {
        let mut state = self.state.lock().await;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
        Ok(state.next_id)
    }

    async fn ensure_ready_for_new_operation(&self) -> Result<(), McpStreamableHttpError> {
        let lifecycle = self.state.lock().await.lifecycle;
        match lifecycle {
            McpStreamableHttpLifecycle::Ready => Ok(()),
            McpStreamableHttpLifecycle::Disconnected => self.initialize().await,
            _ => Err(McpStreamableHttpError::InvalidLifecycle),
        }
    }

    async fn expire_session(&self) {
        let mut state = self.state.lock().await;
        state.session = None;
        state.lifecycle = McpStreamableHttpLifecycle::Disconnected;
    }

    async fn normalize_response_status(
        &self,
        status: StatusCode,
        headers: &HeaderMap,
        session_sent: bool,
    ) -> Result<(), McpStreamableHttpError> {
        let result = normalize_status(
            status,
            headers,
            self.headers.has_static_credential,
            session_sent,
        );
        if matches!(result, Err(McpStreamableHttpError::SessionExpired)) {
            self.expire_session().await;
        }
        result
    }

    async fn live_session_snapshot(&self) -> Result<Option<LiveSession>, McpStreamableHttpError> {
        let state = self.state.lock().await;
        if state.lifecycle != McpStreamableHttpLifecycle::Ready {
            return Err(McpStreamableHttpError::InvalidLifecycle);
        }
        Ok(state.session.clone())
    }
}

fn validate_server_request_id(id: &Value) -> Result<Value, McpStreamableHttpError> {
    match id {
        Value::String(value) if !value.is_empty() && value.len() <= 512 => Ok(id.clone()),
        Value::Number(value) if value.as_i64().is_some() || value.as_u64().is_some() => {
            Ok(id.clone())
        }
        _ => Err(McpStreamableHttpError::MalformedEnvelope),
    }
}

pub(crate) struct RpcResponse {
    pub(crate) value: Value,
    pub(crate) expected_id: u64,
    pub(crate) session_id: Option<SecretString>,
}

struct McpHttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}
