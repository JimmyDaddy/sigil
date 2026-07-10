use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    DEFAULT_PLUGIN_HOOK_OUTPUT_LIMIT_BYTES, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, ExecutionBackend,
    ExecutionCoverageLabel, ExecutionReceipt, ExecutionRequest, ExecutionSandboxProfile,
    MAX_PLUGIN_HOOK_ARTIFACT_REFS, MAX_PLUGIN_HOOK_OUTPUT_LIMIT_BYTES, McpServerConfig,
    MutationEventRecorder, PLUGIN_MANIFEST_DIGEST_PREFIX, PluginAgentRef,
    PluginHookExecutionFinishedEntry, PluginHookExecutionStartedEntry, PluginHookExecutionStatus,
    PluginHookOutputArtifactRef, PluginHookOutputEnvelope, PluginHookOutputStream, PluginHookRef,
    PluginManifest, PluginManifestSnapshot, PluginTrustDecision, PluginTrustEntry, RedactionState,
    SecretRedactor, SkillDescriptor, SkillIndexSnapshot, ToolEffect, VerificationScope,
    WorkspaceMutationScan, validate_plugin_id,
};
use uuid::Uuid;

use crate::skills::discover_plugin_skill_descriptors;

/// Result of workspace plugin discovery, including review snapshots and trusted registrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiscoveryReport {
    pub manifests: Vec<PluginManifestSnapshot>,
    pub registrations: PluginRegistrations,
    pub warnings: Vec<PluginDiscoveryWarning>,
}

/// Runtime registrations emitted by trusted plugin manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginRegistrations {
    pub agents: Vec<PluginAgentRegistration>,
    pub skills: Vec<SkillDescriptor>,
    pub hooks: Vec<PluginHookRegistration>,
    pub mcp_servers: Vec<PluginMcpServerRegistration>,
}

/// Agent profile registration with explicit plugin source attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAgentRegistration {
    pub plugin_id: String,
    pub plugin_root: PathBuf,
    pub agent: PluginAgentRef,
}

/// Hook registration with explicit plugin source attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookRegistration {
    pub plugin_id: String,
    pub plugin_root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_hash: String,
    pub manifest_version: String,
    pub capability_digest: String,
    pub trust: PluginTrustDecision,
    pub hook: PluginHookRef,
}

/// MCP registration with explicit plugin source attribution and a lifecycle-safe server config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMcpServerRegistration {
    pub plugin_id: String,
    pub original_name: String,
    pub server: McpServerConfig,
}

/// One non-fatal problem found while discovering workspace plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiscoveryWarning {
    pub kind: PluginDiscoveryWarningKind,
    pub path: PathBuf,
    pub message: String,
    pub entry_index: Option<usize>,
    pub server_name: Option<String>,
    pub field: Option<String>,
    pub remediation: Option<String>,
    pub trust_action_allowed: bool,
}

/// Stable warning categories for plugin diagnostics and future TUI review display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDiscoveryWarningKind {
    InvalidPath,
    InvalidManifest,
    McpEnvironmentGrantNotSupported,
    ReadFailed,
}

impl PluginDiscoveryWarningKind {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidPath => "plugin_invalid_path",
            Self::InvalidManifest => "plugin_invalid_manifest",
            Self::McpEnvironmentGrantNotSupported => "plugin_mcp_environment_grant_not_supported",
            Self::ReadFailed => "plugin_read_failed",
        }
    }
}

/// Typed plugin-manifest error for an MCP entry that attempts to inherit parent environment
/// values. Callers may downcast `anyhow::Error` to this type without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginMcpEnvironmentGrantNotSupported {
    pub entry_index: usize,
    pub server_name: Option<String>,
}

impl PluginMcpEnvironmentGrantNotSupported {
    #[must_use]
    pub fn code(&self) -> &'static str {
        PluginDiscoveryWarningKind::McpEnvironmentGrantNotSupported.code()
    }
}

impl std::fmt::Display for PluginMcpEnvironmentGrantNotSupported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "plugin MCP entry {}{} cannot declare inherit_env; move the credentialed server to the user root config",
            self.entry_index,
            self.server_name
                .as_deref()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default()
        )
    }
}

impl std::error::Error for PluginMcpEnvironmentGrantNotSupported {}

impl PluginRegistrations {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
            && self.skills.is_empty()
            && self.hooks.is_empty()
            && self.mcp_servers.is_empty()
    }

    #[must_use]
    pub fn mcp_server_configs(&self) -> Vec<McpServerConfig> {
        self.mcp_servers
            .iter()
            .map(|registration| registration.server.clone())
            .collect()
    }
}

/// Input for one trusted plugin hook command execution.
#[derive(Debug, Clone)]
pub struct PluginHookExecutionRequest {
    pub registration: PluginHookRegistration,
    pub workspace_root: PathBuf,
    pub output_limit_bytes: usize,
    pub redactor: SecretRedactor,
    pub artifact_refs: Vec<PluginHookOutputArtifactRef>,
    pub mutation_recorder: Option<MutationEventRecorder>,
}

impl PluginHookExecutionRequest {
    #[must_use]
    pub fn new(registration: PluginHookRegistration, workspace_root: PathBuf) -> Self {
        Self {
            registration,
            workspace_root,
            output_limit_bytes: DEFAULT_PLUGIN_HOOK_OUTPUT_LIMIT_BYTES,
            redactor: SecretRedactor::empty(),
            artifact_refs: Vec::new(),
            mutation_recorder: None,
        }
    }

    #[must_use]
    pub fn with_mutation_recorder(mut self, recorder: MutationEventRecorder) -> Self {
        self.mutation_recorder = Some(recorder);
        self
    }
}

/// Result of one hook command execution plus durable control entries the caller should append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookExecutionOutcome {
    pub started: PluginHookExecutionStartedEntry,
    pub finished: PluginHookExecutionFinishedEntry,
    pub output: PluginHookOutputEnvelope,
    pub receipt: ExecutionReceipt,
    pub mutation_event_id: Option<String>,
}

/// Runs trusted plugin hook commands through the configured non-interactive execution backend.
pub struct PluginHookExecutionRunner {
    backend: Arc<dyn ExecutionBackend>,
    sandbox_profile: ExecutionSandboxProfile,
}

impl PluginHookExecutionRunner {
    #[must_use]
    pub fn new(backend: Arc<dyn ExecutionBackend>) -> Self {
        Self {
            backend,
            sandbox_profile: ExecutionSandboxProfile::Unconfined,
        }
    }

    #[must_use]
    pub fn new_with_sandbox_profile(
        backend: Arc<dyn ExecutionBackend>,
        sandbox_profile: ExecutionSandboxProfile,
    ) -> Self {
        Self {
            backend,
            sandbox_profile,
        }
    }

    /// Executes one plugin hook command and returns durable evidence entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the registration is not trusted, a mutating/unknown hook has no
    /// mutation recorder, the plugin root cannot be used as a process working directory, or the
    /// execution backend fails to spawn or collect the process.
    pub async fn execute(
        &self,
        request: PluginHookExecutionRequest,
    ) -> Result<PluginHookExecutionOutcome> {
        let registration = request.registration;
        if registration.trust != PluginTrustDecision::Trusted {
            bail!(
                "plugin {} hook {} is not trusted",
                registration.plugin_id,
                registration.hook.stable_id()
            );
        }
        registration.hook.validate()?;
        let plugin_root = registration.plugin_root.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin root {}",
                registration.plugin_root.display()
            )
        })?;
        if !plugin_root.is_dir() {
            bail!("plugin root is not a directory: {}", plugin_root.display());
        }
        let command = registration.hook.command_vector();
        let execution_id = format!("plugin_hook_{}", Uuid::new_v4());
        let hook_id = registration.hook.stable_id();
        let declared_effect = registration.hook.declared_effect;
        let tool_name = plugin_hook_tool_name(&registration.plugin_id, &hook_id);
        let backend = self.backend.kind();
        let backend_capabilities = self.backend.capabilities();
        let planned_network = self.backend.planned_network_receipt();
        sigil_kernel::validate_extension_process_isolation(
            self.sandbox_profile,
            backend_capabilities,
            &planned_network,
            format!("plugin_hook:{tool_name}"),
        )?;
        let execution_coverage = ExecutionCoverageLabel::LocalBackendEnforced;
        let sandbox_profile = self.sandbox_profile;
        let egress_logging = registration.hook.egress_logging;
        let allow_secrets = registration.hook.allow_secrets;
        let started = PluginHookExecutionStartedEntry {
            execution_id: execution_id.clone(),
            plugin_id: registration.plugin_id.clone(),
            manifest_hash: registration.manifest_hash.clone(),
            capability_digest: registration.capability_digest.clone(),
            hook_id: hook_id.clone(),
            hook_kind: registration.hook.kind,
            command: command.clone(),
            declared_effect,
            timeout_ms: registration.hook.timeout_ms,
            backend,
            backend_capabilities,
            execution_coverage,
            sandbox_profile,
            environment_policy: sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension,
            egress_logging,
            allow_secrets,
        };
        let mutation_scan = begin_plugin_hook_mutation_scan(
            request.mutation_recorder.as_ref(),
            &request.workspace_root,
            &tool_name,
            declared_effect,
        )?;

        let output_limit_bytes = request
            .output_limit_bytes
            .min(MAX_PLUGIN_HOOK_OUTPUT_LIMIT_BYTES);
        let redactor = request.redactor;
        let artifact_refs = request.artifact_refs;
        let resolved_environment = sigil_kernel::resolve_extension_process_environment(&[])?;
        let mut env = resolved_environment
            .variables()
            .map(|(name, value)| (name.to_owned(), value.expose_secret().to_owned()))
            .collect::<BTreeMap<_, _>>();
        env.insert(
            "SIGIL_WORKSPACE_ROOT".to_owned(),
            request.workspace_root.to_string_lossy().into_owned(),
        );
        env.insert("SIGIL_PLUGIN_ID".to_owned(), registration.plugin_id.clone());
        env.insert("SIGIL_PLUGIN_HOOK_ID".to_owned(), hook_id.clone());
        let receipt = match self
            .backend
            .execute(ExecutionRequest {
                program: registration.hook.command.clone(),
                args: registration.hook.args.clone(),
                cwd: plugin_root,
                env,
                environment_policy: sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension,
                timeout_ms: Some(registration.hook.timeout_ms),
                timeout_secs: 0,
                cpu_time_ms: None,
                memory_limit_bytes: None,
                process_count_limit: None,
            })
            .await
        {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Err(mutation_error) = finish_plugin_hook_mutation_scan(
                    request.mutation_recorder.as_ref(),
                    mutation_scan,
                    &request.workspace_root,
                    execution_id.clone(),
                    tool_name,
                    declared_effect,
                ) {
                    return Err(error).with_context(|| {
                        format!(
                            "plugin hook execution failed; additionally failed to record mutation evidence: {mutation_error}"
                        )
                    });
                }
                return Err(error);
            }
        };
        if let Err(error) = sigil_kernel::validate_extension_process_network_receipt(
            self.sandbox_profile,
            &receipt.network,
            format!("plugin_hook:{tool_name}"),
        ) {
            finish_plugin_hook_mutation_scan(
                request.mutation_recorder.as_ref(),
                mutation_scan,
                &request.workspace_root,
                execution_id,
                tool_name,
                declared_effect,
            )?;
            return Err(error.into());
        }
        let mutation_event_id = finish_plugin_hook_mutation_scan(
            request.mutation_recorder.as_ref(),
            mutation_scan,
            &request.workspace_root,
            execution_id.clone(),
            tool_name,
            declared_effect,
        )?;
        let status = if receipt.timed_out {
            PluginHookExecutionStatus::TimedOut
        } else if receipt.exit_code == Some(0) {
            PluginHookExecutionStatus::Succeeded
        } else {
            PluginHookExecutionStatus::Failed
        };
        let finished = PluginHookExecutionFinishedEntry {
            execution_id,
            plugin_id: registration.plugin_id,
            manifest_hash: registration.manifest_hash,
            capability_digest: registration.capability_digest,
            hook_id,
            hook_kind: registration.hook.kind,
            status,
            exit_code: receipt.exit_code,
            stdout_bytes: receipt.stdout.len() as u64,
            stderr_bytes: receipt.stderr.len() as u64,
            timed_out: receipt.timed_out,
            backend: receipt.backend,
            backend_capabilities: receipt.capabilities,
            execution_coverage,
            sandbox_profile,
            environment_policy: receipt.environment_policy,
            egress_logging,
            allow_secrets,
            network: receipt.network.clone(),
            resources: receipt.resources.clone(),
        };
        let output = plugin_hook_output_envelope(
            &finished,
            &receipt,
            output_limit_bytes,
            &redactor,
            artifact_refs,
        );

        Ok(PluginHookExecutionOutcome {
            started,
            finished,
            output,
            receipt,
            mutation_event_id,
        })
    }
}

#[derive(Debug)]
enum PluginHookMutationScan {
    NotRequired,
    Captured(Box<WorkspaceMutationScan>),
    ScanUnavailable,
}

fn begin_plugin_hook_mutation_scan(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_name: &str,
    effect: ToolEffect,
) -> Result<PluginHookMutationScan> {
    if !effect.may_mutate_workspace() {
        return Ok(PluginHookMutationScan::NotRequired);
    }
    let Some(recorder) = recorder else {
        bail!(
            "plugin hook {tool_name} with effect {} requires mutation recorder",
            effect.as_str()
        );
    };
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    match recorder.capture_workspace_scan(workspace_root, &scope) {
        Ok(scan) => Ok(PluginHookMutationScan::Captured(Box::new(scan))),
        Err(_) => Ok(PluginHookMutationScan::ScanUnavailable),
    }
}

fn finish_plugin_hook_mutation_scan(
    recorder: Option<&MutationEventRecorder>,
    scan: PluginHookMutationScan,
    workspace_root: &Path,
    execution_id: String,
    tool_name: String,
    effect: ToolEffect,
) -> Result<Option<String>> {
    let Some(recorder) = recorder else {
        return Ok(None);
    };
    match scan {
        PluginHookMutationScan::NotRequired => Ok(None),
        PluginHookMutationScan::Captured(before) => {
            let event = match recorder.record_workspace_mutation_if_changed(
                before.as_ref(),
                workspace_root,
                execution_id.clone(),
                tool_name.clone(),
                effect,
            ) {
                Ok(event) => event,
                Err(_) => Some(recorder.record_workspace_scan_unavailable_after(
                    before.as_ref(),
                    execution_id,
                    tool_name,
                    effect,
                )?),
            };
            Ok(event.map(|event| event.event_id))
        }
        PluginHookMutationScan::ScanUnavailable => {
            let event = recorder.record_workspace_scan_unavailable(
                workspace_root,
                execution_id,
                tool_name,
                effect,
            )?;
            Ok(Some(event.event_id))
        }
    }
}

fn plugin_hook_tool_name(plugin_id: &str, hook_id: &str) -> String {
    format!("plugin_hook:{plugin_id}:{hook_id}")
}

fn plugin_hook_output_envelope(
    finished: &PluginHookExecutionFinishedEntry,
    receipt: &ExecutionReceipt,
    output_limit_bytes: usize,
    redactor: &SecretRedactor,
    artifact_refs: Vec<PluginHookOutputArtifactRef>,
) -> PluginHookOutputEnvelope {
    let stdout = bounded_hook_output_stream(&receipt.stdout, output_limit_bytes, redactor);
    let stderr = bounded_hook_output_stream(&receipt.stderr, output_limit_bytes, redactor);
    let redaction_state = combined_redaction_state(stdout.redaction_state, stderr.redaction_state);
    let artifact_refs_truncated = artifact_refs.len() > MAX_PLUGIN_HOOK_ARTIFACT_REFS;
    let artifact_refs = artifact_refs
        .into_iter()
        .take(MAX_PLUGIN_HOOK_ARTIFACT_REFS)
        .collect::<Vec<_>>();
    let model_visible_summary = format!(
        "plugin hook {} finished {}: stdout {} / {} bytes, stderr {} / {} bytes, artifact_refs {}{}",
        finished.hook_id,
        plugin_hook_execution_status_label(finished.status),
        stdout.returned_bytes,
        stdout.total_bytes,
        stderr.returned_bytes,
        stderr.total_bytes,
        artifact_refs.len(),
        if artifact_refs_truncated {
            " (truncated)"
        } else {
            ""
        }
    );
    PluginHookOutputEnvelope {
        execution_id: finished.execution_id.clone(),
        plugin_id: finished.plugin_id.clone(),
        hook_id: finished.hook_id.clone(),
        stdout,
        stderr,
        artifact_refs,
        artifact_refs_truncated,
        redaction_state,
        parse_error: None,
        model_visible_summary,
    }
}

fn bounded_hook_output_stream(
    bytes: &[u8],
    output_limit_bytes: usize,
    redactor: &SecretRedactor,
) -> PluginHookOutputStream {
    let original = String::from_utf8_lossy(bytes);
    let redacted = redactor.redact_text(&original);
    let redaction_state = if redacted == original {
        RedactionState::None
    } else {
        RedactionState::Redacted
    };
    let limited = limit_plugin_hook_text_head_tail(&redacted, output_limit_bytes);
    PluginHookOutputStream {
        content: limited.content,
        total_bytes: bytes.len() as u64,
        returned_bytes: limited.returned_bytes,
        omitted_bytes: limited.omitted_bytes,
        total_lines: redacted.lines().count() as u64,
        returned_lines: limited.returned_lines,
        truncated: limited.truncated,
        redaction_state,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HookTextLimitResult {
    content: String,
    returned_bytes: u64,
    returned_lines: u64,
    omitted_bytes: u64,
    truncated: bool,
}

fn limit_plugin_hook_text_head_tail(input: &str, max_bytes: usize) -> HookTextLimitResult {
    if input.len() <= max_bytes {
        return HookTextLimitResult {
            content: input.to_owned(),
            returned_bytes: input.len() as u64,
            returned_lines: input.lines().count() as u64,
            omitted_bytes: 0,
            truncated: false,
        };
    }
    let head_budget = max_bytes / 2;
    let tail_budget = max_bytes.saturating_sub(head_budget);
    let head_end = floor_char_boundary(input, head_budget);
    let tail_start = ceil_char_boundary(input, input.len().saturating_sub(tail_budget));
    let omitted_bytes = tail_start.saturating_sub(head_end);
    let mut content = String::new();
    content.push_str(&input[..head_end]);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "[sigil: hook output truncated, omitted {omitted_bytes} bytes]\n"
    ));
    content.push_str(&input[tail_start..]);
    HookTextLimitResult {
        returned_bytes: (input.len() - omitted_bytes) as u64,
        returned_lines: content.lines().count() as u64,
        omitted_bytes: omitted_bytes as u64,
        truncated: true,
        content,
    }
}

fn floor_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn combined_redaction_state(left: RedactionState, right: RedactionState) -> RedactionState {
    if matches!(left, RedactionState::Redacted) || matches!(right, RedactionState::Redacted) {
        RedactionState::Redacted
    } else if matches!(left, RedactionState::ContainsSensitiveMetadata)
        || matches!(right, RedactionState::ContainsSensitiveMetadata)
    {
        RedactionState::ContainsSensitiveMetadata
    } else {
        RedactionState::None
    }
}

fn plugin_hook_execution_status_label(status: PluginHookExecutionStatus) -> &'static str {
    match status {
        PluginHookExecutionStatus::Succeeded => "succeeded",
        PluginHookExecutionStatus::Failed => "failed",
        PluginHookExecutionStatus::TimedOut => "timed_out",
    }
}

/// Discovers workspace plugins from `.sigil/plugins/<id>/plugin.toml`.
///
/// Untrusted or stale-trust manifests are still returned as review snapshots, but they do not emit
/// skill, hook, or MCP registrations.
///
/// # Errors
///
/// Returns an error if the workspace plugin directory cannot be listed.
pub fn discover_workspace_plugins(
    workspace_root: &Path,
    trust_entries: &[PluginTrustEntry],
) -> Result<PluginDiscoveryReport> {
    let mut discovery = PluginDiscovery::new(workspace_root);
    discovery.discover(trust_entries)?;
    Ok(discovery.finish())
}

/// Merges plugin skill descriptors into an existing deterministic skill snapshot.
///
/// # Errors
///
/// Returns an error when a plugin skill id duplicates an existing descriptor or fingerprinting
/// fails.
pub fn merge_plugin_skill_descriptors(
    snapshot: &SkillIndexSnapshot,
    plugin_skills: &[SkillDescriptor],
) -> Result<SkillIndexSnapshot> {
    let mut seen = snapshot
        .descriptors
        .iter()
        .map(|descriptor| descriptor.id.clone())
        .collect::<BTreeSet<_>>();
    let mut descriptors = snapshot.descriptors.clone();
    for skill in plugin_skills {
        if !seen.insert(skill.id.clone()) {
            bail!(
                "plugin skill {} conflicts with existing skill index",
                skill.id
            );
        }
        descriptors.push(skill.clone());
    }
    SkillIndexSnapshot::new(descriptors)
}

/// Appends plugin-provided MCP server configs to an existing MCP registry input.
///
/// Plugin server names are already namespaced during discovery, so the returned configs can be
/// handed to the existing MCP eager/lazy lifecycle without starting plugin servers early.
///
/// # Errors
///
/// Returns a typed error if a programmatically constructed plugin registration attempts to carry
/// an environment grant that plugin manifests are not allowed to declare.
pub fn merge_plugin_mcp_servers(
    base: &[McpServerConfig],
    plugin_servers: &[PluginMcpServerRegistration],
) -> Result<Vec<McpServerConfig>> {
    let mut used_names = base
        .iter()
        .map(|server| server.name.clone())
        .collect::<BTreeSet<_>>();
    let mut merged = base.to_vec();
    for (index, registration) in plugin_servers.iter().enumerate() {
        if !registration.server.inherit_env.is_empty() {
            return Err(anyhow::Error::new(PluginMcpEnvironmentGrantNotSupported {
                entry_index: index,
                server_name: Some(registration.original_name.clone()),
            }));
        }
        let mut server = registration.server.clone();
        if !used_names.insert(server.name.clone()) {
            server.name = unique_plugin_mcp_server_name(
                &registration.plugin_id,
                &registration.original_name,
                &used_names,
            );
            used_names.insert(server.name.clone());
        }
        merged.push(server);
    }
    Ok(merged)
}

struct PluginDiscovery {
    workspace_root: PathBuf,
    canonical_workspace_root: PathBuf,
    plugin_dir: PathBuf,
    manifests: Vec<PluginManifestSnapshot>,
    registrations: PluginRegistrations,
    warnings: Vec<PluginDiscoveryWarning>,
}

impl PluginDiscovery {
    fn new(workspace_root: &Path) -> Self {
        let canonical_workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        Self {
            workspace_root: workspace_root.to_path_buf(),
            canonical_workspace_root,
            plugin_dir: workspace_root
                .join(crate::DEFAULT_PROJECT_ASSETS_DIR)
                .join(crate::DEFAULT_WORKSPACE_PLUGINS_LEAF),
            manifests: Vec::new(),
            registrations: PluginRegistrations::default(),
            warnings: Vec::new(),
        }
    }

    fn discover(&mut self, trust_entries: &[PluginTrustEntry]) -> Result<()> {
        let plugin_dir = self.plugin_dir.clone();
        if !plugin_dir.exists() {
            return Ok(());
        }
        if !plugin_dir.is_dir() {
            self.warn(
                PluginDiscoveryWarningKind::InvalidPath,
                plugin_dir,
                "plugin discovery path is not a directory",
            );
            return Ok(());
        }

        for entry in sorted_dir_entries(&plugin_dir)? {
            let plugin_root = entry.path();
            if !plugin_root.is_dir() {
                continue;
            }
            self.discover_plugin(&plugin_root, trust_entries);
        }
        Ok(())
    }

    fn discover_plugin(&mut self, plugin_root: &Path, trust_entries: &[PluginTrustEntry]) {
        let manifest_path = plugin_root.join("plugin.toml");
        if !manifest_path.is_file() {
            self.warn(
                PluginDiscoveryWarningKind::InvalidPath,
                manifest_path,
                "plugin directory is missing plugin.toml",
            );
            return;
        }

        let outcome = match self.read_manifest(plugin_root, &manifest_path) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.warn_manifest_error(manifest_path, &error);
                return;
            }
        };

        let mut snapshot = PluginManifestSnapshot {
            plugin_id: outcome.manifest.id.clone(),
            name: outcome.manifest.name.clone(),
            version: outcome.manifest.version.clone(),
            description: outcome.manifest.description.clone(),
            manifest_path: display_path(&self.workspace_root, &manifest_path),
            manifest_hash: outcome.manifest_hash,
            capabilities: outcome.manifest.capabilities(),
            trust: PluginTrustDecision::NeedsReview,
        };
        if let Some(trust) = matching_trust_entry(&snapshot, trust_entries) {
            snapshot.trust = trust.decision;
        }
        if snapshot.trust == PluginTrustDecision::Trusted
            && let Err(error) = self.register_trusted_plugin(&outcome.manifest, &snapshot)
        {
            self.warn(
                warning_kind_for_manifest_error(&error),
                manifest_path,
                error.to_string(),
            );
            return;
        }
        self.manifests.push(snapshot);
    }

    fn read_manifest(
        &self,
        plugin_root: &Path,
        manifest_path: &Path,
    ) -> Result<PluginManifestReadOutcome> {
        let canonical_plugin_root = plugin_root
            .canonicalize()
            .with_context(|| format!("failed to resolve plugin root {}", plugin_root.display()))?;
        if !canonical_plugin_root.starts_with(&self.canonical_workspace_root) {
            bail!(
                "plugin root escapes workspace root: {}",
                plugin_root.display()
            );
        }
        let canonical_manifest = manifest_path.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin manifest {}",
                manifest_path.display()
            )
        })?;
        if !canonical_manifest.starts_with(&canonical_plugin_root) {
            bail!(
                "plugin manifest escapes plugin root: {}",
                manifest_path.display()
            );
        }

        let bytes = fs::read(manifest_path).with_context(|| {
            format!("failed to read plugin manifest {}", manifest_path.display())
        })?;
        let manifest_hash = format!(
            "{}{:x}",
            PLUGIN_MANIFEST_DIGEST_PREFIX,
            Sha256::digest(&bytes)
        );
        let raw = std::str::from_utf8(&bytes).with_context(|| {
            format!("plugin manifest is not utf-8: {}", manifest_path.display())
        })?;
        reject_plugin_mcp_environment_grants(raw)?;
        let mut manifest = toml::from_str::<PluginManifest>(raw)
            .with_context(|| format!("invalid plugin manifest {}", manifest_path.display()))?;
        let directory_id = plugin_root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        validate_plugin_id(&directory_id)?;
        if manifest.id != directory_id {
            bail!(
                "plugin manifest id {} does not match directory {}",
                manifest.id,
                directory_id
            );
        }
        manifest.root = plugin_root.to_path_buf();
        manifest.validate()?;
        validate_plugin_agent_paths(&manifest, &canonical_plugin_root)?;
        validate_plugin_skill_paths(&manifest, &canonical_plugin_root)?;
        Ok(PluginManifestReadOutcome {
            manifest,
            manifest_hash,
        })
    }

    fn register_trusted_plugin(
        &mut self,
        manifest: &PluginManifest,
        snapshot: &PluginManifestSnapshot,
    ) -> Result<()> {
        let capability_digest = snapshot.capability_digest()?;
        self.registrations
            .agents
            .extend(
                manifest
                    .agents
                    .iter()
                    .cloned()
                    .map(|agent| PluginAgentRegistration {
                        plugin_id: manifest.id.clone(),
                        plugin_root: manifest.root.clone(),
                        agent,
                    }),
            );
        let skills = discover_plugin_skill_descriptors(
            &self.workspace_root,
            &manifest.id,
            &manifest.root,
            &manifest.skills,
        )?;
        self.registrations.skills.extend(skills);
        self.registrations
            .hooks
            .extend(
                manifest
                    .hooks
                    .iter()
                    .cloned()
                    .map(|hook| PluginHookRegistration {
                        plugin_id: manifest.id.clone(),
                        plugin_root: manifest.root.clone(),
                        manifest_path: snapshot.manifest_path.clone(),
                        manifest_hash: snapshot.manifest_hash.clone(),
                        manifest_version: snapshot.version.clone(),
                        capability_digest: capability_digest.clone(),
                        trust: snapshot.trust,
                        hook,
                    }),
            );
        self.registrations
            .mcp_servers
            .extend(
                manifest
                    .mcp_servers
                    .iter()
                    .map(|server| PluginMcpServerRegistration {
                        plugin_id: manifest.id.clone(),
                        original_name: server.name.clone(),
                        server: namespaced_mcp_server(&manifest.id, server),
                    }),
            );
        Ok(())
    }

    fn warn(
        &mut self,
        kind: PluginDiscoveryWarningKind,
        path: impl AsRef<Path>,
        message: impl Into<String>,
    ) {
        self.warnings.push(PluginDiscoveryWarning {
            kind,
            path: path.as_ref().to_path_buf(),
            message: message.into(),
            entry_index: None,
            server_name: None,
            field: None,
            remediation: None,
            trust_action_allowed: false,
        });
    }

    fn warn_manifest_error(&mut self, path: impl AsRef<Path>, error: &anyhow::Error) {
        if let Some(diagnostic) = error.downcast_ref::<PluginMcpEnvironmentGrantNotSupported>() {
            self.warnings.push(PluginDiscoveryWarning {
                kind: PluginDiscoveryWarningKind::McpEnvironmentGrantNotSupported,
                path: path.as_ref().to_path_buf(),
                message: diagnostic.to_string(),
                entry_index: Some(diagnostic.entry_index),
                server_name: diagnostic.server_name.clone(),
                field: Some("inherit_env".to_owned()),
                remediation: Some(
                    "remove inherit_env from plugin.toml and configure the credentialed stdio MCP server in the user root sigil.toml"
                        .to_owned(),
                ),
                trust_action_allowed: false,
            });
            return;
        }
        self.warn(
            warning_kind_for_manifest_error(error),
            path,
            error.to_string(),
        );
    }

    fn finish(self) -> PluginDiscoveryReport {
        PluginDiscoveryReport {
            manifests: self.manifests,
            registrations: self.registrations,
            warnings: self.warnings,
        }
    }
}

fn reject_plugin_mcp_environment_grants(raw: &str) -> Result<()> {
    let value = toml::from_str::<toml::Value>(raw).context("invalid plugin manifest TOML")?;
    let Some(entries) = value.get("mcp_servers").and_then(toml::Value::as_array) else {
        return Ok(());
    };
    for (index, entry) in entries.iter().enumerate() {
        let Some(table) = entry.as_table() else {
            continue;
        };
        if table.contains_key("inherit_env") {
            return Err(anyhow::Error::new(PluginMcpEnvironmentGrantNotSupported {
                entry_index: index,
                server_name: table
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .map(ToOwned::to_owned),
            }));
        }
    }
    Ok(())
}

struct PluginManifestReadOutcome {
    manifest: PluginManifest,
    manifest_hash: String,
}

fn validate_plugin_agent_paths(
    manifest: &PluginManifest,
    canonical_plugin_root: &Path,
) -> Result<()> {
    for agent in &manifest.agents {
        let path = manifest.root.join(&agent.path);
        let canonical_path = path.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin {} agent {}",
                manifest.id,
                agent.path.display()
            )
        })?;
        if !canonical_path.starts_with(canonical_plugin_root) {
            bail!(
                "plugin {} agent path escapes plugin root: {}",
                manifest.id,
                agent.path.display()
            );
        }
    }
    Ok(())
}

fn validate_plugin_skill_paths(
    manifest: &PluginManifest,
    canonical_plugin_root: &Path,
) -> Result<()> {
    for skill in &manifest.skills {
        let path = manifest.root.join(&skill.path);
        let canonical_path = path.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin {} skill {}",
                manifest.id,
                skill.path.display()
            )
        })?;
        if !canonical_path.starts_with(canonical_plugin_root) {
            bail!(
                "plugin {} skill path escapes plugin root: {}",
                manifest.id,
                skill.path.display()
            );
        }
    }
    Ok(())
}

fn matching_trust_entry<'a>(
    snapshot: &PluginManifestSnapshot,
    trust_entries: &'a [PluginTrustEntry],
) -> Option<&'a PluginTrustEntry> {
    trust_entries
        .iter()
        .rev()
        .find(|entry| entry.matches_snapshot(snapshot))
}

fn namespaced_mcp_server(plugin_id: &str, server: &McpServerConfig) -> McpServerConfig {
    let mut server = server.clone();
    server.name = format!("{plugin_id}.{}", server.name);
    server
}

fn unique_plugin_mcp_server_name(
    plugin_id: &str,
    original_name: &str,
    used_names: &BTreeSet<String>,
) -> String {
    let identity = format!("{plugin_id}\0{original_name}");
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let mut candidate = format!("{plugin_id}.{original_name}.{}", &hash[..8]);
    let mut attempt = 0usize;
    while used_names.contains(&candidate) {
        attempt += 1;
        candidate = format!("{plugin_id}.{original_name}.{}.{attempt}", &hash[..8]);
    }
    candidate
}

fn sorted_dir_entries(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| {
            format!(
                "failed to read plugin discovery directory {}",
                dir.display()
            )
        })?
        .filter_map(std::result::Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn warning_kind_for_manifest_error(error: &anyhow::Error) -> PluginDiscoveryWarningKind {
    let message = error.to_string();
    if message.contains("failed to read") || message.contains("not utf-8") {
        PluginDiscoveryWarningKind::ReadFailed
    } else if message.contains("escapes") || message.contains("failed to resolve") {
        PluginDiscoveryWarningKind::InvalidPath
    } else {
        PluginDiscoveryWarningKind::InvalidManifest
    }
}

fn display_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "tests/plugins_tests.rs"]
mod tests;
