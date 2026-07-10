use super::*;
use std::{
    ffi::OsStr,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::ffi::OsString;

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
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![
            ToolSubject::mcp_tool(self.spec.name.clone()),
            self.client.identity.trust_subject(
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
        let secret_detected = self.client.secret_redactor.value_contains_secret(args);
        let argument_summary = self
            .client
            .secret_redactor
            .redact_value(&summarize_egress_json(args));
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
                "arguments": argument_summary,
            }),
            redacted: secret_detected,
        }))
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        if !self.trust.allow_secrets && self.client.secret_redactor.value_contains_secret(&args) {
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
            let redacted_error = self.client.secret_redactor.redact_value(error);
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
            &self.client.secret_redactor,
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

pub(super) struct McpLaunchStaticBinding {
    pub(super) fingerprint: String,
    pub(super) executable: PathBuf,
    pub(super) working_dir: Option<PathBuf>,
}

/// Computes the pre-spawn MCP launch fingerprint in the current working directory.
///
/// Credentialed servers bind the canonical working directory, the executable selected through the
/// isolated baseline `PATH`, and a digest of that executable's bytes. Command arguments are bound
/// as text; this function does not interpret an interpreter's script arguments or attest their
/// contents.
///
/// # Errors
///
/// Returns an error when environment grants cannot be resolved, the working directory or executable
/// cannot be resolved, executable bytes cannot be read, or fingerprint material cannot be encoded.
pub fn mcp_launch_static_fingerprint(config: &McpServerConfig) -> Result<String> {
    if config.inherit_env.is_empty() {
        return mcp_command_fingerprint(&config.command, &config.args);
    }
    let working_dir = std::env::current_dir().context("failed to resolve MCP fingerprint cwd")?;
    mcp_launch_static_fingerprint_at(config, &working_dir)
}

/// Computes the pre-spawn MCP launch fingerprint relative to an explicit execution base.
///
/// # Errors
///
/// Returns an error when environment grants cannot be resolved, the execution base or executable
/// cannot be resolved, executable bytes cannot be read, or fingerprint material cannot be encoded.
pub fn mcp_launch_static_fingerprint_at(
    config: &McpServerConfig,
    working_dir: &Path,
) -> Result<String> {
    if config.inherit_env.is_empty() {
        return mcp_command_fingerprint(&config.command, &config.args);
    }
    let environment = resolve_extension_process_environment(&config.inherit_env)?;
    Ok(mcp_launch_static_binding(config, working_dir, &environment)?.fingerprint)
}

pub(super) fn mcp_launch_static_binding(
    config: &McpServerConfig,
    working_dir: &Path,
    environment: &ResolvedProcessEnvironment,
) -> Result<McpLaunchStaticBinding> {
    if config.inherit_env.is_empty() {
        return Ok(McpLaunchStaticBinding {
            fingerprint: mcp_command_fingerprint(&config.command, &config.args)?,
            executable: PathBuf::from(&config.command),
            working_dir: None,
        });
    }
    let canonical_working_dir = working_dir.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize MCP execution base {}",
            working_dir.display()
        )
    })?;
    let executable = resolve_mcp_executable(&config.command, &canonical_working_dir, environment)?;
    let executable_digest = digest_mcp_executable(&executable)?;
    let encoded = serde_json::to_vec(&json!({
        "version": EXTENSION_ENVIRONMENT_POLICY_VERSION,
        "command": config.command,
        "args": config.args,
        "working_dir": canonical_working_dir.to_string_lossy(),
        "environment_static_fingerprint": environment.static_fingerprint(),
        "executable": {
            "canonical_path": executable.to_string_lossy(),
            "content_sha256": executable_digest,
        },
    }))
    .context("failed to serialize MCP launch fingerprint material")?;
    Ok(McpLaunchStaticBinding {
        fingerprint: format!("sha256:{:x}", Sha256::digest(&encoded)),
        executable,
        working_dir: Some(canonical_working_dir),
    })
}

fn resolve_mcp_executable(
    command: &str,
    working_dir: &Path,
    environment: &ResolvedProcessEnvironment,
) -> Result<PathBuf> {
    if command.is_empty() {
        bail!("credentialed MCP command must not be empty");
    }
    let command_path = Path::new(command);
    if command_path.is_absolute() || command_path.components().count() > 1 {
        let candidate = if command_path.is_absolute() {
            command_path.to_path_buf()
        } else {
            working_dir.join(command_path)
        };
        return canonical_mcp_executable(candidate, command);
    }

    let path = environment
        .variable("PATH")
        .ok_or_else(|| anyhow!("isolated MCP environment is missing baseline PATH"))?;
    for path_entry in std::env::split_paths(OsStr::new(path.expose_secret())) {
        let directory = if path_entry.is_absolute() {
            path_entry
        } else {
            working_dir.join(path_entry)
        };
        for candidate in mcp_executable_candidates(&directory, command, environment) {
            if candidate.is_file() {
                return canonical_mcp_executable(candidate, command);
            }
        }
    }
    bail!("credentialed MCP command {command:?} was not found on the isolated baseline PATH")
}

fn canonical_mcp_executable(candidate: PathBuf, command: &str) -> Result<PathBuf> {
    let canonical = candidate.canonicalize().with_context(|| {
        format!(
            "failed to resolve credentialed MCP command {command:?} at {}",
            candidate.display()
        )
    })?;
    if !canonical.is_file() {
        bail!(
            "credentialed MCP command {command:?} does not resolve to a file: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}

#[cfg(not(windows))]
fn mcp_executable_candidates(
    directory: &Path,
    command: &str,
    _environment: &ResolvedProcessEnvironment,
) -> Vec<PathBuf> {
    vec![directory.join(command)]
}

#[cfg(windows)]
fn mcp_executable_candidates(
    directory: &Path,
    command: &str,
    environment: &ResolvedProcessEnvironment,
) -> Vec<PathBuf> {
    let command_path = Path::new(command);
    if command_path.extension().is_some() {
        return vec![directory.join(command_path)];
    }
    let extensions = environment
        .variable("PATHEXT")
        .map(|value| value.expose_secret())
        .unwrap_or(".COM;.EXE;.BAT;.CMD");
    std::iter::once(directory.join(command_path))
        .chain(extensions.split(';').filter_map(|extension| {
            let extension = extension.trim();
            (!extension.is_empty()).then(|| {
                let mut name = OsString::from(command);
                name.push(extension);
                directory.join(name)
            })
        }))
        .collect()
}

fn digest_mcp_executable(executable: &Path) -> Result<String> {
    let mut file = File::open(executable).with_context(|| {
        format!(
            "failed to open credentialed MCP executable {}",
            executable.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).with_context(|| {
            format!(
                "failed to read credentialed MCP executable {}",
                executable.display()
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}
