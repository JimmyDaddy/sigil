use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    EnvironmentContainment, ExecutionBackend, ExecutionCleanupStatus, ExecutionContainmentRequest,
    ExecutionOutputReceipt, ExecutionReceipt, ExecutionRequest, ExecutionStreamCapture,
    ExecutionTerminationCause, FilesystemContainment, NetworkContainment, ProcessContainment, Tool,
    ToolAccess, ToolAnalysisReason, ToolAnalysisReasonCode, ToolAnalysisStatus, ToolCategory,
    ToolContext, ToolErrorKind, ToolExecutionId, ToolOperation, ToolPermissionEffect,
    ToolPermissionPlanDraft, ToolPermissionSummary, ToolPreviewCapability, ToolProgressEvent,
    ToolResult, ToolResultMeta, ToolSemanticScope, ToolSpec, ToolSubject, ToolSubjectScope,
    safe_persistence_text,
};
use tree_sitter::{Node, Parser};

use crate::{
    constants::{
        DEFAULT_TEXT_LIMIT_BYTES, HARD_TEXT_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV, WORKSPACE_TEMP_ROOT,
    },
    path::{
        ResolvedToolPath, absolute_path_from, canonical_workspace_root, lexically_normalize_path,
        resolve_existing_prefix, resolve_tool_path_from_base,
    },
    scratch_namespace::{
        ScratchNamespaceControl, ScratchNamespaceLeaseRegistry, ScratchQuota,
        scratch_provision_error_result, session_scratch_key,
    },
    shell_runtime::{ResolvedShell, ShellDialect},
    support::{
        TextLimitResult, ceil_char_boundary, floor_char_boundary, limit_text_head_tail,
        required_string, sha256_hex,
    },
};

const SHELL_SEMANTIC_REGISTRY_VERSION: u32 = 2;
const SHELL_ENVIRONMENT_POLICY_VERSION: u32 = 1;
const FILE_PRESENCE_EXECUTION_BINDING_KEY: &str = "file_presence_execution_binding";
const FILE_PRESENCE_EXECUTION_PROFILE_VERSION: u32 = 1;
const WORKSPACE_CHECK_MIN_AVAILABLE_BYTES: u64 = 1024 * 1024 * 1024;
const WORKSPACE_CHECK_TARGET_HEADROOM_DIVISOR: u64 = 16;
const WORKSPACE_CHECK_MAX_TARGET_HEADROOM_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const WORKSPACE_CHECK_TARGET_SCAN_CEILING_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
struct FilePresenceExecutionProfile {
    binding: String,
    shell_program: PathBuf,
    git_program: PathBuf,
}

impl FilePresenceExecutionProfile {
    fn apply_to_environment(&self, environment: &mut BTreeMap<String, String>) {
        let git_directory = self
            .git_program
            .parent()
            .expect("trusted git executable must have a parent directory");
        environment.insert(
            "PATH".to_owned(),
            git_directory.to_string_lossy().into_owned(),
        );
        for (name, value) in [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_NO_LAZY_FETCH", "1"),
            ("GIT_NO_REPLACE_OBJECTS", "1"),
            ("GIT_PAGER", "cat"),
            ("PAGER", "cat"),
            ("GIT_CONFIG_COUNT", "5"),
            ("GIT_CONFIG_KEY_0", "core.pager"),
            ("GIT_CONFIG_VALUE_0", "cat"),
            ("GIT_CONFIG_KEY_1", "pager.log"),
            ("GIT_CONFIG_VALUE_1", "false"),
            ("GIT_CONFIG_KEY_2", "log.showSignature"),
            ("GIT_CONFIG_VALUE_2", "false"),
            ("GIT_CONFIG_KEY_3", "core.fsmonitor"),
            ("GIT_CONFIG_VALUE_3", "false"),
            ("GIT_CONFIG_KEY_4", "core.hooksPath"),
            ("GIT_CONFIG_VALUE_4", "/dev/null"),
        ] {
            environment.insert(name.to_owned(), value.to_owned());
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct TrustedExecutableIdentity {
    program: PathBuf,
    binding: Value,
}

pub(crate) struct BashTool {
    pub(crate) scratch_label: String,
    pub(crate) scratch_quota: ScratchQuota,
    pub(crate) scratch_control: ScratchNamespaceControl,
    pub(crate) scratch_namespaces: Arc<ScratchNamespaceLeaseRegistry>,
    pub(crate) backend: Arc<dyn ExecutionBackend>,
    pub(crate) shell: ResolvedShell,
}

impl BashTool {
    fn session_scratch_dir(&self, ctx: &ToolContext) -> PathBuf {
        self.scratch_control
            .session_scratch_dir(ctx.session_scope_id())
    }

    fn analyze_command(&self, ctx: &ToolContext, command: &str) -> Result<ShellCommandAnalysis> {
        let path_policy = ShellPathPolicyBinding::for_runtime(
            &ctx.workspace_root,
            &self.session_scratch_dir(ctx),
            true,
        )?;
        analyze_shell_command_with_path_policy(
            &ctx.workspace_root,
            command,
            &self.shell,
            &path_policy,
        )
    }
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_owned(),
            description: format!(
                "Run a shell command from the workspace root using {} syntax (the tool name remains `bash`). Use ${SIGIL_SCRATCH_DIR_ENV} for temporary shell files that must survive across tool calls in this session (shown as {}). The scratch directory is scoped to the current session, private to this user, capped by a size quota, and reclaimed after a TTL; do not rely on it for long-term storage. OS temp directories are outside the workspace and require permission.external_directory.",
                shell_syntax_guidance(&self.shell),
                self.scratch_label
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let command = required_string(args, "command")?;
        reject_non_finite_bash_command(command, &self.shell)?;
        let analysis = self.analyze_command(ctx, command)?;
        let file_presence_profile = file_presence_execution_profile_for_binding(
            &ctx.workspace_root,
            &self.shell,
            analysis
                .analysis_bindings
                .get(FILE_PRESENCE_EXECUTION_BINDING_KEY),
        )?;
        let mut plan = analysis.permission_plan();
        let capabilities = self.backend.capabilities();
        let network_receipt = self.backend.planned_network_receipt();
        let filesystem_proven = matches!(
            plan.containment.filesystem,
            FilesystemContainment::Unspecified
        ) || capabilities.filesystem_isolation;
        let network_proven = match plan.containment.network {
            NetworkContainment::Unspecified | NetworkContainment::Allow => true,
            NetworkContainment::Deny => network_receipt.is_denied(),
            NetworkContainment::ReadOnly => false,
        };
        let process_proven = matches!(plan.containment.process, ProcessContainment::Unspecified)
            || capabilities.process_isolation;
        let environment_proven = plan.containment.environment == EnvironmentContainment::Restricted;
        let containment_proven = filesystem_proven
            && network_proven
            && process_proven
            && environment_proven
            && !plan.containment.persistent_process;
        plan.analysis_bindings.insert(
            "containment_proven".to_owned(),
            containment_proven.to_string(),
        );
        plan.analysis_bindings.insert(
            "execution_backend".to_owned(),
            format!(
                "{}:{}:{}",
                self.backend.kind().as_str(),
                serde_json::to_string(&capabilities)?,
                serde_json::to_string(&network_receipt)?
            ),
        );
        plan.analysis_bindings.insert(
            "execution_profile".to_owned(),
            serde_json::to_string(&plan.containment)?,
        );
        plan.analysis_bindings.insert(
            "environment_binding".to_owned(),
            shell_environment_binding_with_profile(
                ctx,
                &self.session_scratch_dir(ctx),
                &self.shell,
                plan.containment.environment,
                file_presence_profile.as_ref(),
            )?,
        );
        Ok(plan)
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let command = required_string(&args, "command")?;
        if let Err(error) = reject_non_finite_bash_command(command, &self.shell) {
            return Ok(ToolResult::error(
                call_id,
                self.spec().name,
                ToolErrorKind::InvalidInput,
                error.to_string(),
            )
            .with_error_details(
                false,
                json!({
                    "category": "persistent_command",
                    "retryable": false,
                    "next_tool": "terminal_start"
                }),
            ));
        }
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(ctx.timeout_secs);
        let session_scope_id = ctx.session_scope_id().map(str::to_owned);
        let session_scratch = self
            .scratch_control
            .session_scratch_dir(session_scope_id.as_deref());
        let (request, fallback_analysis) = if let Some(plan) = ctx.prepared_permission_plan() {
            anyhow::ensure!(
                plan.tool_name == "bash",
                "prepared permission plan belongs to a different tool"
            );
            let file_presence_profile = file_presence_execution_profile_for_binding(
                &ctx.workspace_root,
                &self.shell,
                plan.analysis_bindings
                    .get(FILE_PRESENCE_EXECUTION_BINDING_KEY),
            )?;
            let expected_environment_binding = shell_environment_binding_with_profile(
                &ctx,
                &session_scratch,
                &self.shell,
                plan.containment.environment,
                file_presence_profile.as_ref(),
            )?;
            anyhow::ensure!(
                plan.analysis_bindings.get("environment_binding")
                    == Some(&expected_environment_binding),
                "prepared shell environment binding changed before execution"
            );
            let expected_path_policy_binding =
                ShellPathPolicyBinding::for_runtime(&ctx.workspace_root, &session_scratch, true)?
                    .stable_hash();
            anyhow::ensure!(
                plan.analysis_bindings.get("path_policy_binding")
                    == Some(&expected_path_policy_binding),
                "prepared shell symbolic path binding changed before execution"
            );
            (
                bash_execution_request_from_containment(
                    command,
                    &ctx.workspace_root,
                    &session_scratch,
                    timeout_secs,
                    &self.shell,
                    plan.containment.environment,
                    file_presence_profile.as_ref(),
                ),
                None,
            )
        } else {
            // Direct tool invocations used by diagnostics/tests have no agent-prepared envelope.
            // Analyze once here and use that same result for execution and receipt projection.
            let analysis = self.analyze_command(&ctx, command)?;
            let file_presence_profile = file_presence_execution_profile_for_binding(
                &ctx.workspace_root,
                &self.shell,
                analysis
                    .analysis_bindings
                    .get(FILE_PRESENCE_EXECUTION_BINDING_KEY),
            )?;
            let request = bash_execution_request_from_containment(
                command,
                &ctx.workspace_root,
                &session_scratch,
                timeout_secs,
                &self.shell,
                analysis.containment.environment,
                file_presence_profile.as_ref(),
            );
            (request, Some(analysis))
        };
        let workspace_check = fallback_analysis
            .as_ref()
            .is_some_and(|analysis| analysis.command_family.is_workspace_check())
            || ctx
                .prepared_permission_plan()
                .is_some_and(|plan| plan.operation == ToolOperation::ExecuteWorkspaceCheckCommand);
        if workspace_check {
            let workspace_root = ctx.workspace_root.clone();
            let resource_probe = tokio::task::spawn_blocking(move || {
                workspace_check_resource_probe(&workspace_root)
            })
            .await
            .context("workspace resource preflight task panicked")?;
            match resource_probe {
                Ok(probe) => {
                    if let Some(result) =
                        workspace_check_resource_error(&call_id, &self.spec().name, command, &probe)
                    {
                        return Ok(result);
                    }
                }
                Err(error) => {
                    return Ok(ToolResult::error(
                        call_id,
                        self.spec().name,
                        ToolErrorKind::Io,
                        "workspace validation could not inspect local disk capacity",
                    )
                    .with_error_details(
                        true,
                        json!({
                            "code": "disk_capacity_probe_failed",
                            "resource": "disk_space",
                            "reason": error.to_string(),
                            "action": "verify that the workspace volume is available, then retry"
                        }),
                    ));
                }
            }
        }
        // RFC-0062 14.1: provision the session-scoped scratch namespace (owner-only, quota
        // checked) before any child can write into it. Quota failures are recoverable tool
        // errors, never a silent fallback to the system temp directory.
        let scratch_control = self.scratch_control.clone();
        let provision_scope = session_scope_id.clone();
        let provision_quota = self.scratch_quota;
        let provision = tokio::task::spawn_blocking(move || {
            scratch_control.ensure_session_scratch(provision_scope.as_deref(), &provision_quota)
        })
        .await
        .context("scratch provisioning task panicked")?;
        let session_key = session_scratch_key(session_scope_id.as_deref());
        match provision {
            Ok(_provision) => {}
            Err(error) => {
                return Ok(scratch_provision_error_result(
                    call_id,
                    self.spec().name,
                    &self.scratch_label,
                    error,
                ));
            }
        };
        let _scratch_lease = self.scratch_namespaces.acquire(&session_key);
        // RFC-0062 8.1: create the harness-owned capture plan and staging sink BEFORE spawn so
        // stdout/stderr chunks are tee'd as they arrive instead of being reconstructed from the
        // bounded post-execution content.
        let mut request = request;
        let capture_plan = ctx.tool_artifact_store().map(|store| {
            sigil_kernel::ToolExecutionCapturePlanV1::process_defaults(
                store.session_scope_id_hash().to_owned(),
                &call_id,
                "bash",
            )
        });
        let mut capture_setup_failed = false;
        if let Some(plan) = capture_plan.as_ref()
            && let Some(sink) = ctx.create_policy_safe_tool_output_sink(
                &call_id,
                "bash",
                "text/plain; charset=utf-8",
                sigil_kernel::ToolArtifactEncoding::Utf8,
                sigil_kernel::ToolArtifactSensitivity::Ordinary,
            )
        {
            let config = plan.process_capture_config();
            match sink.begin_process_capture(config) {
                Ok(staged) => {
                    request.capture = Some(sigil_kernel::ExecutionCaptureHandle {
                        sink: staged,
                        config,
                    });
                }
                Err(_error) => {
                    // Capture storage is secondary to process execution. Keep the command
                    // running, but retain a typed diagnostic so settlement never pretends that
                    // the missing artifact was a pipe-reader failure or a complete capture.
                    capture_setup_failed = true;
                }
            }
        }
        if capture_plan.is_some() && request.capture.is_none() {
            capture_setup_failed = true;
        }
        let _process_effect = ctx.begin_forward_effect(sigil_kernel::RunEffectKind::Process)?;
        let execution_hash = sigil_kernel::stable_event_hash(call_id.as_bytes());
        let execution_digest = execution_hash
            .strip_prefix("sha256:")
            .unwrap_or(execution_hash.as_str());
        ctx.emit_progress(ToolProgressEvent {
            execution_id: ToolExecutionId::new(format!("bash-{execution_digest}"))?,
            call_id: call_id.clone(),
            tool_name: "bash".to_owned(),
            sequence: 1,
            status: "running".to_owned(),
            message: Some("foreground shell command is running".to_owned()),
            output_preview: None,
            output_log_ref: None,
            total_bytes: Some(0),
            updated_at_ms: None,
            details: json!({ "execution_mode": "foreground" }),
        })?;
        let receipt = self
            .backend
            .execute_with_cancellation(request, ctx.cancellation_handle())
            .await?;
        if matches!(
            receipt.effective_output().termination,
            ExecutionTerminationCause::Cancelled
        ) && receipt.resources.cleanup.status != ExecutionCleanupStatus::Completed
            && let Some(cancellation) = ctx.cancellation_handle()
        {
            cancellation.mark_cleanup_incomplete();
        }
        let observed_bytes = receipt.effective_output().combined_total_bytes;
        let mut result = if let Some(analysis) = fallback_analysis.as_ref() {
            bash_tool_result_from_execution_receipt_with_analysis(
                call_id,
                self.spec().name,
                receipt,
                analysis,
            )?
        } else {
            bash_tool_result_from_execution_receipt_with_plan(
                call_id,
                self.spec().name,
                receipt,
                command,
                &self.shell,
                ctx.prepared_permission_plan()
                    .context("prepared permission plan disappeared before receipt projection")?,
            )?
        };
        if capture_setup_failed {
            attach_capture_storage_failure(&mut result, observed_bytes, "capture_setup_failed");
            result = result.with_unavailable_artifact_capture(observed_bytes);
        }
        let capture_outcome = result.take_capture_outcome();
        if let Some(outcome) = capture_outcome {
            // RFC-0062 9.2/9.3: settle the harness-owned capture into the canonical dual-segment
            // artifact. The observed resource meter comes from the backend, not the bounded text.
            match outcome.sink.finish_process_capture(
                outcome.observed_bytes,
                u32::from(result.content != safe_persistence_text(&result.content)),
                outcome.source,
            ) {
                Ok((descriptor, segments, completeness)) => {
                    if completeness.storage
                        == sigil_kernel::session::ToolStorageCompletenessV1::Unavailable
                    {
                        attach_capture_storage_failure(
                            &mut result,
                            observed_bytes,
                            "capture_write_failed",
                        );
                    }
                    if let Some(plan) = capture_plan.as_ref() {
                        match sigil_kernel::ToolResultRecordedV3::from_process_capture(
                            &result,
                            descriptor,
                            plan,
                            segments,
                            completeness,
                            sigil_kernel::tool_model_view_initial_limit("bash"),
                        ) {
                            Ok((recorded, display)) => {
                                result.set_durable_v3_projection(recorded, display);
                            }
                            Err(_error) => {
                                attach_capture_storage_failure(
                                    &mut result,
                                    observed_bytes,
                                    "capture_settlement_failed",
                                );
                                result = result.with_unavailable_artifact_capture(observed_bytes);
                            }
                        }
                    } else {
                        result = result.with_unavailable_artifact_capture(observed_bytes);
                    }
                }
                Err(_error) => {
                    attach_capture_storage_failure(
                        &mut result,
                        observed_bytes,
                        "capture_settlement_failed",
                    );
                    result = result.with_unavailable_artifact_capture(observed_bytes);
                }
            }
        }
        Ok(result)
    }
}

fn attach_capture_storage_failure(result: &mut ToolResult, observed_bytes: u64, stage: &str) {
    if !result.metadata.details.is_object() {
        result.metadata.details = json!({});
    }
    result.metadata.details["capture"] = json!({
        "code": "capture_storage_failed",
        "stage": stage,
        "observed_bytes": observed_bytes,
        "command_completed": true,
        "action": "free local disk space before requesting the full saved output"
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceCheckResourceProbe {
    available_bytes: u64,
    target_bytes_lower_bound: u64,
    target_scan_truncated: bool,
}

impl WorkspaceCheckResourceProbe {
    fn required_available_bytes(self) -> u64 {
        WORKSPACE_CHECK_MIN_AVAILABLE_BYTES.saturating_add(
            self.target_bytes_lower_bound
                .checked_div(WORKSPACE_CHECK_TARGET_HEADROOM_DIVISOR)
                .unwrap_or_default()
                .min(WORKSPACE_CHECK_MAX_TARGET_HEADROOM_BYTES),
        )
    }

    fn has_capacity(self) -> bool {
        self.available_bytes >= self.required_available_bytes()
    }
}

fn workspace_check_resource_probe(workspace_root: &Path) -> Result<WorkspaceCheckResourceProbe> {
    let available_bytes = fs2::available_space(workspace_root).with_context(|| {
        format!(
            "failed to inspect free space for {}",
            workspace_root.display()
        )
    })?;
    let (target_bytes_lower_bound, target_scan_truncated) =
        bounded_directory_size(&workspace_root.join("target"));
    Ok(WorkspaceCheckResourceProbe {
        available_bytes,
        target_bytes_lower_bound,
        target_scan_truncated,
    })
}

fn bounded_directory_size(path: &Path) -> (u64, bool) {
    if !path.is_dir() {
        return (0, false);
    }
    let mut total = 0_u64;
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() {
            total = total.saturating_add(entry.metadata().map_or(0, |metadata| metadata.len()));
            if total >= WORKSPACE_CHECK_TARGET_SCAN_CEILING_BYTES {
                return (total, true);
            }
        }
    }
    (total, false)
}

fn workspace_check_resource_error(
    call_id: &str,
    tool_name: &str,
    command: &str,
    probe: &WorkspaceCheckResourceProbe,
) -> Option<ToolResult> {
    if probe.has_capacity() {
        return None;
    }
    let required_available_bytes = probe.required_available_bytes();
    Some(
        ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::ResourceExhausted,
            "workspace validation was paused because the workspace volume is low on disk space",
        )
        .with_error_details(
            true,
            json!({
                "code": "disk_space_exhausted",
                "resource": "disk_space",
                "available_bytes": probe.available_bytes,
                "required_available_bytes": required_available_bytes,
                "target_bytes_lower_bound": probe.target_bytes_lower_bound,
                "target_scan_truncated": probe.target_scan_truncated,
                "command_sha256": sha256_hex(command.as_bytes()),
                "action": "free disk space or remove disposable build artifacts, then resume the Task"
            }),
        ),
    )
}

fn reject_non_finite_bash_command(command: &str, shell: &ResolvedShell) -> Result<()> {
    let reason = match shell.dialect() {
        ShellDialect::Posix => {
            if posix_shell_contains_background_operator(command) {
                Some("background operator `&`")
            } else {
                persistent_shell_command_reason(command, 0)
            }
        }
        ShellDialect::PowerShell => powershell_persistent_command_reason(command),
        ShellDialect::Cmd => cmd_persistent_command_reason(command),
    };
    if let Some(reason) = reason {
        bail!(
            "bash only supports finite foreground commands; {reason} requires terminal_start with an explicit background or interactive mode"
        );
    }
    Ok(())
}

fn posix_shell_contains_background_operator(command: &str) -> bool {
    let mut parser = Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return false;
    }
    parser
        .parse(command, None)
        .is_some_and(|tree| shell_ast_contains_kind(tree.root_node(), "&"))
}

fn shell_ast_contains_kind(node: Node<'_>, target: &str) -> bool {
    node.kind() == target
        || (0..node.child_count()).any(|index| {
            node.child(index as u32)
                .is_some_and(|child| shell_ast_contains_kind(child, target))
        })
}

fn persistent_shell_command_reason(command: &str, depth: usize) -> Option<&'static str> {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return Some("shell wrapper recursion exceeds the finite-command limit");
    }
    let tokens = tokenize_shell_subject_words(command);
    for segment in split_shell_command_segments(&tokens) {
        for pipeline_segment in split_shell_pipeline(segment) {
            if let Some(reason) = persistent_shell_segment_reason(pipeline_segment, depth) {
                return Some(reason);
            }
        }
    }
    None
}

fn persistent_shell_segment_reason(words: &[String], depth: usize) -> Option<&'static str> {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return Some("shell wrapper recursion exceeds the finite-command limit");
    }
    let (program, args) = shell_segment_command_and_args(words)?;
    match program {
        "nohup" => return Some("nohup launches persistence-oriented work"),
        "setsid" => return Some("setsid detaches process lifecycle ownership"),
        "watch" => return Some("watch is a persistent command runner"),
        "tail" | "journalctl" if args.iter().any(|arg| shell_follow_option(arg)) => {
            return Some("follow mode is persistent");
        }
        "docker" | "podman" | "nerdctl" | "kubectl"
            if args.first().is_some_and(|arg| arg == "logs")
                && args.iter().skip(1).any(|arg| shell_follow_option(arg)) =>
        {
            return Some("log follow mode is persistent");
        }
        "sh" | "bash" | "zsh" => {
            return static_shell_payload(args)
                .and_then(|payload| persistent_shell_command_reason(payload, depth + 1));
        }
        "sudo" | "doas" | "env" | "command" if !args.is_empty() => {
            return persistent_shell_segment_reason(args, depth + 1);
        }
        _ => {}
    }
    if let Some(inner) = static_wrapper_inner(program, args) {
        return persistent_shell_segment_reason(inner, depth + 1);
    }
    None
}

fn shell_follow_option(arg: &str) -> bool {
    matches!(arg, "-f" | "-F" | "--follow") || arg.starts_with("--follow=")
}

fn powershell_persistent_command_reason(command: &str) -> Option<&'static str> {
    if command
        .trim_end()
        .strip_suffix('&')
        .is_some_and(|prefix| !prefix.ends_with('&'))
    {
        return Some("PowerShell background operator `&`");
    }
    command
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ';' | '|'))
        .any(|word| word.eq_ignore_ascii_case("start-job"))
        .then_some("Start-Job creates background work")
}

fn cmd_persistent_command_reason(command: &str) -> Option<&'static str> {
    let words = command.split_whitespace().collect::<Vec<_>>();
    (words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("start"))
        && words
            .iter()
            .skip(1)
            .any(|word| word.eq_ignore_ascii_case("/b")))
    .then_some("start /b creates background work")
}

pub(crate) fn known_finite_terminal_command_reason(
    command: &str,
    shell: &ResolvedShell,
    analysis: &ShellCommandAnalysis,
) -> Option<String> {
    if persistent_shell_command_reason(command, 0).is_some() {
        return None;
    }
    if analysis.command_family.is_known_finite() {
        return Some(format!(
            "known finite command family `{}`",
            analysis.command_family.as_str()
        ));
    }
    known_finite_terminal_command_reason_from_tokens(
        &tokenize_shell_subject_words(command),
        shell,
        0,
    )
}

fn known_finite_terminal_command_reason_from_tokens(
    tokens: &[String],
    shell: &ResolvedShell,
    depth: usize,
) -> Option<String> {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return None;
    }
    let mut first_reason = None;
    for segment in split_shell_command_segments(tokens) {
        for pipeline_segment in split_shell_pipeline(segment) {
            match terminal_segment_duration_evidence(pipeline_segment, shell, depth) {
                TerminalSegmentDurationEvidence::KnownFinite(reason) => {
                    first_reason.get_or_insert(reason);
                }
                TerminalSegmentDurationEvidence::Persistent
                | TerminalSegmentDurationEvidence::Unknown => return None,
            }
        }
    }
    first_reason
}

enum TerminalSegmentDurationEvidence {
    KnownFinite(String),
    Persistent,
    Unknown,
}

fn terminal_segment_duration_evidence(
    words: &[String],
    shell: &ResolvedShell,
    depth: usize,
) -> TerminalSegmentDurationEvidence {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return TerminalSegmentDurationEvidence::Unknown;
    }
    if persistent_shell_segment_reason(words, depth).is_some() {
        return TerminalSegmentDurationEvidence::Persistent;
    }
    let Some((program, args)) = shell_segment_command_and_args(words) else {
        return TerminalSegmentDurationEvidence::Unknown;
    };
    if matches!(program, "sh" | "bash" | "zsh") {
        return static_shell_payload(args)
            .and_then(|payload| {
                known_finite_terminal_command_reason_from_tokens(
                    &tokenize_shell_subject_words(payload),
                    shell,
                    depth + 1,
                )
            })
            .map_or(
                TerminalSegmentDurationEvidence::Unknown,
                TerminalSegmentDurationEvidence::KnownFinite,
            );
    }
    if let Some(inner) = static_wrapper_inner(program, args) {
        return terminal_segment_duration_evidence(inner, shell, depth + 1);
    }
    let has_watch_flag = args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--watch" | "--watch-all" | "--watchAll" | "-w"
        ) || arg.starts_with("--watch=")
    });
    if has_watch_flag {
        return TerminalSegmentDurationEvidence::Persistent;
    }
    let known_reason = match program {
        "cargo" => cargo_finite_subcommand(args)
            .map(|subcommand| format!("finite cargo `{subcommand}` command")),
        "npm" | "pnpm" | "yarn" | "bun" => package_manager_finite_script(args)
            .map(|script| format!("finite {program} `{script}` package script")),
        _ if shell.dialect() != ShellDialect::Posix => None,
        _ => None,
    };
    if let Some(reason) = known_reason {
        return TerminalSegmentDurationEvidence::KnownFinite(reason);
    }
    let family = command_family_for_simple_segment_with_depth(words, depth);
    if family.is_known_finite() {
        TerminalSegmentDurationEvidence::KnownFinite(format!(
            "known finite command family `{}`",
            family.as_str()
        ))
    } else {
        TerminalSegmentDurationEvidence::Unknown
    }
}

fn cargo_finite_subcommand(args: &[String]) -> Option<&str> {
    args.iter()
        .find(|arg| !arg.starts_with('-') && !arg.starts_with('+'))
        .map(String::as_str)
        .filter(|subcommand| {
            matches!(
                *subcommand,
                "build" | "check" | "clippy" | "doc" | "fmt" | "test"
            )
        })
}

fn package_manager_finite_script(args: &[String]) -> Option<&str> {
    let (script, remaining) = match args.first().map(String::as_str) {
        Some("run") => (args.get(1)?.as_str(), &args[2..]),
        Some(script) => (script, &args[1..]),
        None => return None,
    };
    if remaining.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--watch" | "--watch-all" | "--watchAll" | "-w"
        ) || arg.starts_with("--watch=")
    }) {
        return None;
    }
    let normalized = script
        .rsplit([':', '/'])
        .next()
        .unwrap_or(script)
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "build" | "check" | "ci" | "fmt" | "format" | "lint" | "test" | "typecheck" | "type-check"
    )
    .then_some(script)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellCommandAnalysis {
    pub(crate) command: String,
    pub(crate) normalized_command: String,
    pub(crate) command_family: CommandFamily,
    pub(crate) classification_source: ShellClassificationSource,
    pub(crate) access: ToolAccess,
    pub(crate) operation: ToolOperation,
    pub(crate) subjects: Vec<ToolSubject>,
    pub(crate) grant_scope: Option<CommandGrantScope>,
    pub(crate) explanation: ShellApprovalReason,
    pub(crate) shell_program: String,
    pub(crate) shell_dialect: ShellDialect,
    pub(crate) permission_effects: BTreeSet<ToolPermissionEffect>,
    pub(crate) analysis_status: ToolAnalysisStatus,
    pub(crate) containment: ExecutionContainmentRequest,
    pub(crate) semantic_scope: Option<ToolSemanticScope>,
    pub(crate) safe_summary: ToolPermissionSummary,
    pub(crate) analysis_bindings: BTreeMap<String, String>,
}

impl ShellCommandAnalysis {
    pub(crate) fn permission_plan(&self) -> ToolPermissionPlanDraft {
        ToolPermissionPlanDraft {
            access: self.access,
            operation: self.operation,
            effects: self.permission_effects.clone(),
            subjects: self.subjects.clone(),
            analysis: self.analysis_status.clone(),
            containment: self.containment.clone(),
            semantic_scope: self.semantic_scope.clone(),
            tool_default_mode: None,
            analysis_bindings: self.analysis_bindings.clone(),
            safe_summary: self.safe_summary.clone(),
            managed_file_access: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandFamily {
    CargoCheck,
    CargoFmtCheck,
    CargoTest,
    CargoClippy,
    CargoValidationChain { steps: Vec<CargoValidationStep> },
    CheckTouched { tier: Option<String> },
    GitReadOnly,
    Search,
    ListRead,
    ReadOnlyChain { step_count: usize },
    FilePresenceCheck,
    StaticFileWrite,
    FindWrite,
    FindDelete,
    ShellNoop,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CargoValidationStep {
    FmtCheck,
    Check,
    Test,
    Clippy,
}

impl CargoValidationStep {
    fn as_str(self) -> &'static str {
        match self {
            Self::FmtCheck => "cargo_fmt_check",
            Self::Check => "cargo_check",
            Self::Test => "cargo_test",
            Self::Clippy => "cargo_clippy",
        }
    }

    fn executes_workspace_code(self) -> bool {
        !matches!(self, Self::FmtCheck)
    }
}

impl CommandFamily {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::CargoCheck => "cargo_check",
            Self::CargoFmtCheck => "cargo_fmt_check",
            Self::CargoTest => "cargo_test",
            Self::CargoClippy => "cargo_clippy",
            Self::CargoValidationChain { .. } => "cargo_validation_chain",
            Self::CheckTouched { .. } => "check_touched",
            Self::GitReadOnly => "git_read_only",
            Self::Search => "search",
            Self::ListRead => "list_read",
            Self::ReadOnlyChain { .. } => "read_only_chain",
            Self::FilePresenceCheck => "file_presence_check",
            Self::StaticFileWrite => "static_file_write",
            Self::FindWrite => "find_write",
            Self::FindDelete => "find_delete",
            Self::ShellNoop => "shell_noop",
            Self::Unknown => "unknown",
        }
    }

    fn stable_subject(&self) -> String {
        match self {
            Self::CheckTouched { tier } => tier
                .as_deref()
                .map(|tier| {
                    format!(
                        "family:check_touched:tier_sha256={}",
                        sha256_hex(tier.as_bytes())
                    )
                })
                .unwrap_or_else(|| "family:check_touched".to_owned()),
            Self::CargoValidationChain { steps } => format!(
                "family:workspace_validation:{}",
                steps
                    .iter()
                    .map(|step| step.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::ReadOnlyChain { .. } => "family:workspace_read_only_shell".to_owned(),
            _ => format!("family:{}", self.as_str()),
        }
    }

    pub(crate) fn is_workspace_check(&self) -> bool {
        matches!(
            self,
            Self::CargoCheck
                | Self::CargoFmtCheck
                | Self::CargoTest
                | Self::CargoClippy
                | Self::CargoValidationChain { .. }
                | Self::CheckTouched { .. }
        )
    }

    pub(crate) fn is_known_finite(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    fn is_workspace_read_only(&self) -> bool {
        matches!(
            self,
            Self::GitReadOnly
                | Self::Search
                | Self::ListRead
                | Self::ReadOnlyChain { .. }
                | Self::FilePresenceCheck
                | Self::ShellNoop
        )
    }

    fn is_workspace_mutating(&self) -> bool {
        matches!(
            self,
            Self::StaticFileWrite | Self::FindWrite | Self::FindDelete
        )
    }

    fn is_recognized(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    fn step_count(&self) -> u32 {
        match self {
            Self::CargoValidationChain { steps } => steps.len() as u32,
            Self::ReadOnlyChain { step_count } => *step_count as u32,
            _ => 1,
        }
    }

    fn workspace_code_steps(&self) -> u32 {
        match self {
            Self::CargoCheck | Self::CargoTest | Self::CargoClippy | Self::CheckTouched { .. } => 1,
            Self::CargoValidationChain { steps } => steps
                .iter()
                .filter(|step| step.executes_workspace_code())
                .count() as u32,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellClassificationSource {
    BuiltinFamily,
    KnownReadonlyFastPath,
    AstKnownReadonly,
    DestructivePattern,
    Unknown,
}

impl ShellClassificationSource {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::BuiltinFamily => "builtin_family",
            Self::KnownReadonlyFastPath => "known_readonly_fast_path",
            Self::AstKnownReadonly => "ast_known_readonly",
            Self::DestructivePattern => "destructive_pattern",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandGrantScope {
    ExactCommand,
    WorkspaceCheckFamily,
    WorkspaceReadOnlyShell,
    WorkspaceScript {
        path: String,
        args_family: Option<String>,
    },
}

impl CommandGrantScope {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::ExactCommand => "exact_command",
            Self::WorkspaceCheckFamily => "workspace_check_family",
            Self::WorkspaceReadOnlyShell => "workspace_read_only_shell",
            Self::WorkspaceScript { .. } => "workspace_script",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellApprovalReason {
    WorkspaceCheck,
    WorkspaceReadOnly,
    WorkspaceMutation,
    UnknownCommand,
    DestructiveCommand,
}

impl ShellApprovalReason {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::WorkspaceCheck => "workspace_check",
            Self::WorkspaceReadOnly => "workspace_read_only",
            Self::WorkspaceMutation => "workspace_mutation",
            Self::UnknownCommand => "unknown_command",
            Self::DestructiveCommand => "destructive_command",
        }
    }
}

#[cfg(test)]
pub(crate) fn analyze_shell_command(
    workspace_root: &Path,
    command: &str,
) -> Result<ShellCommandAnalysis> {
    let shell = ResolvedShell::resolve_explicit("sh")?;
    analyze_shell_command_with_shell(workspace_root, command, &shell)
}

/// Runtime-provided symbolic path roots understood by the structured analyzer.
///
/// These roots are derived from Sigil configuration, never from the inherited process
/// environment. Only the stable hash is copied into the permission plan.
#[derive(Debug, Clone, Default)]
pub(crate) struct ShellPathPolicyBinding {
    scratch_root: Option<PathBuf>,
    sandbox_tmpdir_root: Option<PathBuf>,
}

impl ShellPathPolicyBinding {
    pub(crate) fn for_runtime(
        workspace_root: &Path,
        scratch_root: &Path,
        bind_sandbox_tmpdir: bool,
    ) -> Result<Self> {
        let workspace_root = canonical_workspace_root(workspace_root)?;
        let scratch_root = absolute_path_from(&workspace_root, scratch_root);
        let scratch_root = resolve_existing_prefix(&lexically_normalize_path(&scratch_root)?)?;
        Ok(Self {
            scratch_root: Some(scratch_root.clone()),
            sandbox_tmpdir_root: bind_sandbox_tmpdir.then_some(scratch_root),
        })
    }

    pub(crate) fn stable_hash(&self) -> String {
        let payload = serde_json::json!({
            "version": 1,
            "scratch_root_sha256": self
                .scratch_root
                .as_ref()
                .map(|path| sha256_hex(path.to_string_lossy().as_bytes())),
            "sandbox_tmpdir_root_sha256": self
                .sandbox_tmpdir_root
                .as_ref()
                .map(|path| sha256_hex(path.to_string_lossy().as_bytes())),
        });
        sha256_hex(payload.to_string().as_bytes())
    }
}

#[derive(Debug, Clone)]
struct ShellBoundPathReference {
    symbol: &'static str,
    spelling: &'static str,
    suffix: String,
}

#[derive(Debug, Default)]
struct ShellBoundPathInspection {
    status: Option<ToolAnalysisStatus>,
    subjects: Vec<ToolSubject>,
    symbols: BTreeSet<&'static str>,
}

fn inspect_bound_shell_paths(
    workspace_root: &Path,
    command: &str,
    family: &CommandFamily,
    path_policy: &ShellPathPolicyBinding,
) -> Result<ShellBoundPathInspection> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let mut inspection = ShellBoundPathInspection::default();
    for reference in shell_bound_path_references(command) {
        inspection.symbols.insert(reference.spelling);
        let (root, runtime_owned) = match reference.symbol {
            "SIGIL_SCRATCH_DIR" => (path_policy.scratch_root.as_ref(), true),
            "TMPDIR" => (
                path_policy.sandbox_tmpdir_root.as_ref(),
                family.is_workspace_read_only() || family.is_workspace_check(),
            ),
            _ => unreachable!("bounded shell path scanner emits only modeled symbols"),
        };
        let requested = format!("{}{}", reference.spelling, reference.suffix);
        let valid_suffix = reference.suffix.is_empty() || reference.suffix.starts_with('/');
        let Some(root) = root.filter(|_| runtime_owned && valid_suffix) else {
            let reason_code = if root.is_none() {
                ToolAnalysisReasonCode::UnresolvedPath
            } else {
                ToolAnalysisReasonCode::UnprovenContainment
            };
            inspection
                .status
                .get_or_insert_with(|| ToolAnalysisStatus::Conservative {
                    reasons: vec![ToolAnalysisReason::new(
                        reason_code,
                        Some(
                            "a symbolic shell path is not bound to this execution profile"
                                .to_owned(),
                        ),
                    )],
                });
            inspection.subjects.push(ToolSubject::path_with_scope(
                requested.clone(),
                format!(
                    "unresolved_symbolic_path:sha256:{}",
                    sha256_hex(requested.as_bytes())
                ),
                None,
                ToolSubjectScope::Unknown,
            ));
            continue;
        };

        let relative = reference.suffix.strip_prefix('/').unwrap_or("");
        let lexical_target = lexically_normalize_path(&root.join(relative))?;
        let resolved_target = resolve_existing_prefix(&lexical_target)?;
        if !resolved_target.starts_with(root) {
            inspection
                .status
                .get_or_insert_with(|| ToolAnalysisStatus::Conservative {
                    reasons: vec![ToolAnalysisReason::new(
                        ToolAnalysisReasonCode::UnprovenContainment,
                        Some("a symbolic shell path escapes its runtime-owned root".to_owned()),
                    )],
                });
        }
        let mut resolved = resolve_tool_path_from_base(
            &workspace_root,
            root,
            if relative.is_empty() { "." } else { relative },
        )?;
        resolved.original = requested;
        if resolved_target.starts_with(root) {
            let relative_label = resolved_target
                .strip_prefix(root)
                .expect("containment checked above")
                .to_string_lossy()
                .replace('\\', "/");
            resolved.scope = ToolSubjectScope::RuntimeScratch;
            resolved.normalized = if relative_label.is_empty() {
                WORKSPACE_TEMP_ROOT.to_owned()
            } else {
                format!("{WORKSPACE_TEMP_ROOT}/{relative_label}")
            };
        }
        inspection
            .subjects
            .push(resolved_tool_path_subject(resolved));
    }
    Ok(inspection)
}

fn shell_bound_path_references(command: &str) -> Vec<ShellBoundPathReference> {
    const SYMBOLS: [(&str, &str, &str); 4] = [
        (
            "SIGIL_SCRATCH_DIR",
            "$SIGIL_SCRATCH_DIR",
            "$SIGIL_SCRATCH_DIR",
        ),
        (
            "SIGIL_SCRATCH_DIR",
            "${SIGIL_SCRATCH_DIR}",
            "${SIGIL_SCRATCH_DIR}",
        ),
        ("TMPDIR", "$TMPDIR", "$TMPDIR"),
        ("TMPDIR", "${TMPDIR}", "${TMPDIR}"),
    ];
    let bytes = command.as_bytes();
    let mut references = Vec::new();
    let mut index = 0usize;
    let mut single_quoted = false;
    let mut double_quoted = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if !single_quoted => {
                index = index.saturating_add(2);
                continue;
            }
            b'\'' if !double_quoted => {
                single_quoted = !single_quoted;
                index += 1;
                continue;
            }
            b'"' if !single_quoted => {
                double_quoted = !double_quoted;
                index += 1;
                continue;
            }
            b'$' if !single_quoted => {}
            _ => {
                index += 1;
                continue;
            }
        }

        let Some((symbol, pattern, spelling)) = SYMBOLS
            .iter()
            .find(|(_, pattern, _)| bytes[index..].starts_with(pattern.as_bytes()))
        else {
            index += 1;
            continue;
        };
        let suffix_start = index + pattern.len();
        let mut suffix_end = suffix_start;
        while suffix_end < bytes.len()
            && !matches!(
                bytes[suffix_end],
                b' ' | b'\t'
                    | b'\r'
                    | b'\n'
                    | b';'
                    | b'|'
                    | b'&'
                    | b'<'
                    | b'>'
                    | b'('
                    | b')'
                    | b'\''
                    | b'"'
            )
        {
            suffix_end += 1;
        }
        references.push(ShellBoundPathReference {
            symbol,
            spelling,
            suffix: String::from_utf8_lossy(&bytes[suffix_start..suffix_end]).into_owned(),
        });
        index = suffix_end.max(index + 1);
    }
    references
}

#[cfg(test)]
pub(crate) fn analyze_shell_command_with_shell(
    workspace_root: &Path,
    command: &str,
    shell: &ResolvedShell,
) -> Result<ShellCommandAnalysis> {
    analyze_shell_command_with_path_policy(
        workspace_root,
        command,
        shell,
        &ShellPathPolicyBinding::default(),
    )
}

pub(crate) fn analyze_shell_command_with_path_policy(
    workspace_root: &Path,
    command: &str,
    shell: &ResolvedShell,
    path_policy: &ShellPathPolicyBinding,
) -> Result<ShellCommandAnalysis> {
    analyze_shell_command_with_path_policy_and_environment(
        workspace_root,
        command,
        shell,
        path_policy,
        &controlled_shell_environment(),
    )
}

fn analyze_shell_command_with_path_policy_and_environment(
    workspace_root: &Path,
    command: &str,
    shell: &ResolvedShell,
    path_policy: &ShellPathPolicyBinding,
    controlled_environment: &BTreeMap<String, String>,
) -> Result<ShellCommandAnalysis> {
    if shell.dialect() != ShellDialect::Posix {
        let normalized_command = normalize_shell_command_for_permission(command);
        let subjects = vec![ToolSubject::command(
            normalized_command.clone(),
            command_permission_subject(command),
        )];
        return Ok(build_shell_command_analysis(
            ShellCommandAnalysisBase {
                command: command.to_owned(),
                normalized_command,
                command_family: CommandFamily::Unknown,
                classification_source: ShellClassificationSource::Unknown,
                access: ToolAccess::Execute,
                operation: ToolOperation::ExecuteUnknownCommand,
                subjects,
                grant_scope: None,
                explanation: ShellApprovalReason::UnknownCommand,
                shell_program: shell.program_string(),
                shell_dialect: shell.dialect(),
                ast_binding: format!("unsupported:{}", sha256_hex(command.as_bytes())),
                path_policy_binding: path_policy.stable_hash(),
            },
            ToolAnalysisStatus::Unsupported {
                reason: ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::UnsupportedSyntax,
                    Some(
                        "the first structured shell analyzer supports POSIX syntax only".to_owned(),
                    ),
                ),
            },
            false,
        ));
    }
    let ast = inspect_posix_shell_ast(command, path_policy);
    let mut family = classify_shell_command_family(workspace_root, command)?;
    let file_presence_uses_git = family == CommandFamily::FilePresenceCheck
        && bounded_file_presence_command_uses_git(command);
    if family == CommandFamily::FilePresenceCheck && !ast.verified_file_presence_loop {
        family = CommandFamily::Unknown;
    }
    let mut file_presence_execution_binding = None;
    let file_presence_execution_status =
        if file_presence_uses_git && family == CommandFamily::FilePresenceCheck {
            match file_presence_execution_profile_with_environment(
                workspace_root,
                shell,
                controlled_environment,
            ) {
                Ok(profile) => {
                    file_presence_execution_binding = Some(profile.binding);
                    None
                }
                Err(detail) => {
                    family = CommandFamily::Unknown;
                    Some(ToolAnalysisStatus::Conservative {
                        reasons: vec![ToolAnalysisReason::new(
                            ToolAnalysisReasonCode::UnprovenContainment,
                            Some(detail),
                        )],
                    })
                }
            }
        } else {
            None
        };
    let bound_paths = inspect_bound_shell_paths(workspace_root, command, &family, path_policy)?;
    let normalized_command = normalize_shell_command_for_permission(command);
    let destructive = shell_command_is_destructive(command);
    let has_file_write = shell_command_has_file_write(command);
    let semantic_limit_exceeded = shell_wrapper_limit_exceeded(command, 0);
    let semantic_dynamic_reason = shell_semantic_dynamic_reason(command);
    let incomplete_analysis = ast
        .status
        .clone()
        .or(file_presence_execution_status)
        .or_else(|| bound_paths.status.clone())
        .or_else(|| {
            if semantic_limit_exceeded {
                Some(ToolAnalysisStatus::Conservative {
                    reasons: vec![ToolAnalysisReason::new(
                        ToolAnalysisReasonCode::AnalysisLimitExceeded,
                        Some("shell wrapper recursion exceeds the analysis limit".to_owned()),
                    )],
                })
            } else {
                semantic_dynamic_reason.map(|detail| ToolAnalysisStatus::Conservative {
                    reasons: vec![ToolAnalysisReason::new(
                        ToolAnalysisReasonCode::DynamicCommand,
                        Some(detail.to_owned()),
                    )],
                })
            }
        });
    let workspace_safe_readonly =
        !destructive && bash_command_is_safe_readonly_in_workspace(workspace_root, command)?;
    let ast_known_readonly = !destructive
        && family == CommandFamily::Unknown
        && ast.saw_readonly_structure
        && workspace_safe_readonly;
    let mut subjects = Vec::new();
    let access;
    let operation;
    let grant_scope;
    let explanation;
    let classification_source;

    if incomplete_analysis.is_some() {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteUnknownCommand;
        grant_scope = None;
        explanation = ShellApprovalReason::UnknownCommand;
        classification_source = ShellClassificationSource::Unknown;
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            command_permission_subject(command),
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    } else if destructive {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteDestructiveCommand;
        grant_scope = None;
        explanation = ShellApprovalReason::DestructiveCommand;
        classification_source = ShellClassificationSource::DestructivePattern;
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            command_permission_subject(command),
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    } else if family.is_workspace_check() {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteWorkspaceCheckCommand;
        grant_scope = workspace_check_grant_scope(&family);
        explanation = ShellApprovalReason::WorkspaceCheck;
        classification_source = ShellClassificationSource::BuiltinFamily;
        let stable_subject = family.stable_subject();
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            stable_subject,
        ));
        subjects.extend(external_shell_path_subjects(workspace_root, command)?);
    } else if family.is_workspace_mutating() {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteMutatingCommand;
        grant_scope = None;
        explanation = ShellApprovalReason::WorkspaceMutation;
        classification_source = ShellClassificationSource::BuiltinFamily;
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            family.stable_subject(),
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    } else if family.is_workspace_read_only() || ast_known_readonly || workspace_safe_readonly {
        access = ToolAccess::Read;
        operation = ToolOperation::ExecuteReadOnlyCommand;
        grant_scope = if family == CommandFamily::Unknown {
            Some(CommandGrantScope::ExactCommand)
        } else {
            Some(CommandGrantScope::WorkspaceReadOnlyShell)
        };
        explanation = ShellApprovalReason::WorkspaceReadOnly;
        classification_source = if ast_known_readonly {
            ShellClassificationSource::AstKnownReadonly
        } else if family == CommandFamily::Unknown {
            ShellClassificationSource::KnownReadonlyFastPath
        } else {
            ShellClassificationSource::BuiltinFamily
        };
        let stable_subject = if family == CommandFamily::Unknown {
            command_permission_subject(command)
        } else {
            family.stable_subject()
        };
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            stable_subject,
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    } else {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteUnknownCommand;
        grant_scope = None;
        explanation = ShellApprovalReason::UnknownCommand;
        classification_source = ShellClassificationSource::Unknown;
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            command_permission_subject(command),
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    }

    if !bound_paths.symbols.is_empty() {
        subjects.retain(|subject| {
            subject.kind != sigil_kernel::ToolSubjectKind::Path
                || !bound_paths
                    .symbols
                    .iter()
                    .any(|symbol| subject.original.contains(symbol))
        });
        for subject in bound_paths.subjects {
            if !subjects.contains(&subject) {
                subjects.push(subject);
            }
        }
    }
    annotate_shell_subject_access(&mut subjects, command);

    let analysis_status = incomplete_analysis.unwrap_or_else(|| {
        if family.is_recognized() {
            ToolAnalysisStatus::Complete
        } else {
            ToolAnalysisStatus::Conservative {
                reasons: vec![ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::UnknownProgram,
                    Some(
                        "the command is outside the structured shell semantic registry".to_owned(),
                    ),
                )],
            }
        }
    });

    let mut analysis = build_shell_command_analysis(
        ShellCommandAnalysisBase {
            command: command.to_owned(),
            normalized_command,
            command_family: family,
            classification_source,
            access,
            operation,
            subjects,
            grant_scope,
            explanation,
            shell_program: shell.program_string(),
            shell_dialect: shell.dialect(),
            ast_binding: ast.normalized_ast_hash,
            path_policy_binding: path_policy.stable_hash(),
        },
        analysis_status,
        has_file_write,
    );
    if let Some(binding) = file_presence_execution_binding {
        analysis
            .analysis_bindings
            .insert(FILE_PRESENCE_EXECUTION_BINDING_KEY.to_owned(), binding);
    }
    Ok(analysis)
}

struct ShellCommandAnalysisBase {
    command: String,
    normalized_command: String,
    command_family: CommandFamily,
    classification_source: ShellClassificationSource,
    access: ToolAccess,
    operation: ToolOperation,
    subjects: Vec<ToolSubject>,
    grant_scope: Option<CommandGrantScope>,
    explanation: ShellApprovalReason,
    shell_program: String,
    shell_dialect: ShellDialect,
    ast_binding: String,
    path_policy_binding: String,
}

fn build_shell_command_analysis(
    base: ShellCommandAnalysisBase,
    analysis_status: ToolAnalysisStatus,
    has_file_write: bool,
) -> ShellCommandAnalysis {
    let permission_effects = shell_permission_effects(
        &base.command,
        &base.command_family,
        base.operation,
        &analysis_status,
        has_file_write,
    );
    let (access, operation, explanation) = effective_shell_permission_shape(
        base.access,
        base.operation,
        &permission_effects,
        base.explanation.clone(),
    );
    let containment = shell_containment_request(
        &base.command_family,
        operation,
        &analysis_status,
        &base.subjects,
    );
    let semantic_scope = shell_semantic_scope(
        &base.command_family,
        &analysis_status,
        &base.command,
        &base.ast_binding,
    );
    let safe_summary = shell_permission_summary(
        &base.command_family,
        operation,
        &analysis_status,
        has_file_write,
    );
    let analysis_bindings = BTreeMap::from([
        (
            "shell_dialect".to_owned(),
            base.shell_dialect.as_str().to_owned(),
        ),
        (
            "shell_program_sha256".to_owned(),
            sha256_hex(base.shell_program.as_bytes()),
        ),
        (
            "semantic_registry_version".to_owned(),
            SHELL_SEMANTIC_REGISTRY_VERSION.to_string(),
        ),
        ("normalized_ast".to_owned(), base.ast_binding),
        ("path_policy_binding".to_owned(), base.path_policy_binding),
    ]);
    ShellCommandAnalysis {
        command: base.command,
        normalized_command: base.normalized_command,
        command_family: base.command_family,
        classification_source: base.classification_source,
        access,
        operation,
        subjects: base.subjects,
        grant_scope: base.grant_scope,
        explanation,
        shell_program: base.shell_program,
        shell_dialect: base.shell_dialect,
        permission_effects,
        analysis_status,
        containment,
        semantic_scope,
        safe_summary,
        analysis_bindings,
    }
}

fn shell_permission_effects(
    command: &str,
    family: &CommandFamily,
    operation: ToolOperation,
    analysis_status: &ToolAnalysisStatus,
    has_file_write: bool,
) -> BTreeSet<ToolPermissionEffect> {
    let mut effects = BTreeSet::new();
    if analysis_status.is_complete() {
        if family.is_workspace_read_only() {
            effects.insert(ToolPermissionEffect::FileRead);
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        } else if family.is_workspace_check() {
            effects.insert(ToolPermissionEffect::FileRead);
            if family.workspace_code_steps() > 0 {
                effects.insert(ToolPermissionEffect::FileWrite);
                effects.insert(ToolPermissionEffect::ExecuteWorkspaceCode);
            } else {
                effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
            }
        } else if family.is_workspace_mutating() {
            effects.insert(ToolPermissionEffect::FileRead);
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
    } else if shell_semantic_dynamic_reason(command).is_some()
        || analysis_status_has_reason(analysis_status, ToolAnalysisReasonCode::DynamicCommand)
    {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
    } else {
        effects.insert(ToolPermissionEffect::Unknown);
    }
    if has_file_write || matches!(family, CommandFamily::FindWrite) {
        effects.insert(ToolPermissionEffect::FileWrite);
    }
    if shell_command_has_file_delete(command) || matches!(family, CommandFamily::FindDelete) {
        effects.insert(ToolPermissionEffect::FileDelete);
    }
    if effects.is_empty() {
        effects.insert(match operation {
            ToolOperation::ExecuteReadOnlyCommand => ToolPermissionEffect::ExecuteTrustedBinary,
            ToolOperation::ExecuteWorkspaceCheckCommand => {
                ToolPermissionEffect::ExecuteWorkspaceCode
            }
            _ => ToolPermissionEffect::Unknown,
        });
    }
    if !matches!(family, CommandFamily::FilePresenceCheck) {
        collect_shell_declared_effects(command, &mut effects, 0);
    }
    effects
}

fn analysis_status_has_reason(status: &ToolAnalysisStatus, code: ToolAnalysisReasonCode) -> bool {
    match status {
        ToolAnalysisStatus::Complete => false,
        ToolAnalysisStatus::Conservative { reasons } => {
            reasons.iter().any(|reason| reason.code == code)
        }
        ToolAnalysisStatus::Unsupported { reason } | ToolAnalysisStatus::Invalid { reason } => {
            reason.code == code
        }
    }
}

fn effective_shell_permission_shape(
    base_access: ToolAccess,
    base_operation: ToolOperation,
    effects: &BTreeSet<ToolPermissionEffect>,
    base_explanation: ShellApprovalReason,
) -> (ToolAccess, ToolOperation, ShellApprovalReason) {
    if effects.contains(&ToolPermissionEffect::FileDelete)
        || base_operation == ToolOperation::ExecuteDestructiveCommand
    {
        return (
            ToolAccess::Execute,
            ToolOperation::ExecuteDestructiveCommand,
            ShellApprovalReason::DestructiveCommand,
        );
    }
    if effects.contains(&ToolPermissionEffect::ExecuteDynamicCode)
        || effects.contains(&ToolPermissionEffect::Unknown)
    {
        return (
            ToolAccess::Execute,
            ToolOperation::ExecuteUnknownCommand,
            ShellApprovalReason::UnknownCommand,
        );
    }
    if effects.iter().any(|effect| {
        matches!(
            effect,
            ToolPermissionEffect::NetworkMutate
                | ToolPermissionEffect::ProcessControl
                | ToolPermissionEffect::PrivilegeEscalation
                | ToolPermissionEffect::PersistenceChange
                | ToolPermissionEffect::RemoteMutation
                | ToolPermissionEffect::ExternalApplicationControl
        )
    }) || effects.contains(&ToolPermissionEffect::FileWrite)
        && base_operation != ToolOperation::ExecuteWorkspaceCheckCommand
    {
        return (
            ToolAccess::Execute,
            ToolOperation::ExecuteMutatingCommand,
            ShellApprovalReason::WorkspaceMutation,
        );
    }
    if effects.iter().any(|effect| {
        matches!(
            effect,
            ToolPermissionEffect::NetworkRead
                | ToolPermissionEffect::NetworkUnknown
                | ToolPermissionEffect::CredentialAccess
        )
    }) {
        return (
            ToolAccess::Execute,
            ToolOperation::ExecuteUnknownCommand,
            ShellApprovalReason::UnknownCommand,
        );
    }
    (base_access, base_operation, base_explanation)
}

fn collect_shell_declared_effects(
    command: &str,
    effects: &mut BTreeSet<ToolPermissionEffect>,
    depth: usize,
) {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        return;
    }
    let tokens = tokenize_shell_subject_words(command);
    for segment in split_shell_command_segments(&tokens) {
        for pipeline_segment in split_shell_pipeline(segment) {
            collect_shell_segment_effects(pipeline_segment, effects, depth);
        }
    }
}

fn collect_shell_segment_effects(
    words: &[String],
    effects: &mut BTreeSet<ToolPermissionEffect>,
    depth: usize,
) {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        return;
    }
    if words
        .iter()
        .any(|word| input_redirection_target(word).is_some() || word == "<")
    {
        effects.insert(ToolPermissionEffect::FileRead);
    }
    if shell_segment_has_overwrite_redirection(words) {
        effects.insert(ToolPermissionEffect::FileWrite);
    }
    let Some((program, args)) = shell_segment_command_and_args(words) else {
        effects.insert(ToolPermissionEffect::Unknown);
        return;
    };
    if leading_assignments_are_dangerous(words) {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
    }
    if let Some(inner) = static_wrapper_inner(program, args) {
        if program == "nohup" {
            effects.insert(ToolPermissionEffect::PersistenceChange);
            effects.insert(ToolPermissionEffect::ProcessControl);
        }
        collect_shell_segment_effects(inner, effects, depth + 1);
        return;
    }
    if matches!(program, "sudo" | "doas") {
        effects.insert(ToolPermissionEffect::PrivilegeEscalation);
        if !args.is_empty() {
            collect_shell_segment_effects(args, effects, depth + 1);
        }
        return;
    }
    if matches!(program, "sh" | "bash" | "zsh") {
        if let Some(payload) = static_shell_payload(args) {
            collect_shell_declared_effects(payload, effects, depth + 1);
        } else {
            effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        }
        return;
    }
    if program == "eval" {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        return;
    }
    if program == "xargs" {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        if let Some(inner) = xargs_inner_command(args) {
            collect_shell_segment_effects(inner, effects, depth + 1);
        }
        return;
    }
    if matches!(program, "watch" | "setsid" | "flock") {
        effects.insert(ToolPermissionEffect::ProcessControl);
        effects.insert(ToolPermissionEffect::PersistenceChange);
        if let Some(inner) = lifecycle_wrapper_inner(program, args) {
            collect_shell_segment_effects(inner, effects, depth + 1);
        }
        return;
    }
    match program {
        "pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "du" | "file" | "readlink"
        | "realpath" | "basename" | "dirname" | "diff" | "cmp" | "grep" | "which" | "uname"
        | "date" | "whoami" | "id" | "sort" | "uniq" => {
            effects.insert(ToolPermissionEffect::FileRead);
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "echo" | "printf" | "true" | ":" | "set" => {
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "cd" => {
            effects.insert(ToolPermissionEffect::FileRead);
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "command" if matches!(args.first().map(String::as_str), Some("-v" | "-V")) => {
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "rg" => {
            effects.insert(ToolPermissionEffect::FileRead);
            if args.iter().any(|arg| rg_arg_may_execute_program(arg)) {
                effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
            } else {
                effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
            }
        }
        "find" => collect_find_effects(args, effects, depth),
        "git" => collect_git_effects(args, effects),
        "cargo" => collect_cargo_effects(args, effects),
        "npm" | "pnpm" | "yarn" | "bun" | "npx" | "pnpx" | "bunx" => {
            collect_package_manager_effects(args, effects);
        }
        "mkdir" | "touch" | "cp" | "mv" | "install" | "chmod" | "chown" | "truncate" | "dd" => {
            effects.insert(ToolPermissionEffect::FileWrite);
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "rm" | "rmdir" => {
            effects.insert(ToolPermissionEffect::FileDelete);
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "curl" | "wget" => collect_transfer_effects(args, effects),
        "ssh" | "scp" | "sftp" | "rsync" => {
            effects.insert(ToolPermissionEffect::NetworkUnknown);
            effects.insert(ToolPermissionEffect::RemoteMutation);
        }
        "gh" | "kubectl" | "terraform" | "tofu" | "ansible" | "ansible-playbook" | "helm"
        | "az" | "aws" | "gcloud" => collect_remote_cli_effects(program, args, effects),
        "docker" | "podman" | "nerdctl" => collect_container_effects(args, effects),
        "kill" | "pkill" | "killall" | "launchctl" | "systemctl" | "service" => {
            effects.insert(ToolPermissionEffect::ProcessControl);
        }
        "open" | "osascript" | "xdg-open" | "start" => {
            effects.insert(ToolPermissionEffect::ExternalApplicationControl);
        }
        _ if known_tool_help_or_version_query(words) => {
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        _ => {
            effects.insert(ToolPermissionEffect::Unknown);
        }
    }
}

fn leading_assignments_are_dangerous(words: &[String]) -> bool {
    words
        .iter()
        .take_while(|word| is_shell_assignment(word))
        .any(|word| shell_assignment_changes_execution_semantics(word))
}

fn shell_assignment_changes_execution_semantics(assignment: &str) -> bool {
    assignment
        .split_once('=')
        .is_none_or(|(name, _)| !matches!(name, "LANG" | "LC_ALL" | "LC_CTYPE" | "TZ" | "NO_COLOR"))
}

fn xargs_inner_command(args: &[String]) -> Option<&[String]> {
    let mut index = 0usize;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            index += 1;
            break;
        }
        if !arg.starts_with('-') {
            break;
        }
        if matches!(
            arg.as_str(),
            "-a" | "--arg-file" | "-d" | "--delimiter" | "-E" | "-I" | "-L" | "-n" | "-P" | "-s"
        ) {
            index = index.saturating_add(2);
        } else {
            index += 1;
        }
    }
    (index < args.len()).then_some(&args[index..])
}

fn lifecycle_wrapper_inner<'a>(program: &str, args: &'a [String]) -> Option<&'a [String]> {
    match program {
        "watch" => args
            .iter()
            .position(|arg| !arg.starts_with('-'))
            .map(|index| &args[index..]),
        "setsid" => args
            .iter()
            .position(|arg| !arg.starts_with('-'))
            .map(|index| &args[index..]),
        "flock" => {
            let lock_index = args.iter().position(|arg| !arg.starts_with('-'))?;
            (lock_index + 1 < args.len()).then_some(&args[lock_index + 1..])
        }
        _ => None,
    }
}

fn collect_find_effects(
    args: &[String],
    effects: &mut BTreeSet<ToolPermissionEffect>,
    depth: usize,
) {
    effects.insert(ToolPermissionEffect::FileRead);
    effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
    if find_segment_has_delete_action(args) {
        effects.insert(ToolPermissionEffect::FileDelete);
    }
    if find_segment_has_write_action(args) {
        effects.insert(ToolPermissionEffect::FileWrite);
    }
    let mut index = 0usize;
    while index < args.len() {
        if matches!(
            args[index].as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir"
        ) {
            let start = index + 1;
            if let Some(relative_end) = args[start..]
                .iter()
                .position(|word| matches!(word.as_str(), ";" | "\\;" | "+"))
            {
                collect_shell_segment_effects(
                    &args[start..start + relative_end],
                    effects,
                    depth + 1,
                );
                index = start + relative_end + 1;
                continue;
            }
            effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
            return;
        }
        index += 1;
    }
}

fn collect_cargo_effects(args: &[String], effects: &mut BTreeSet<ToolPermissionEffect>) {
    effects.insert(ToolPermissionEffect::FileRead);
    match cargo_command_family(args) {
        CommandFamily::CargoFmtCheck => {
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        CommandFamily::CargoCheck | CommandFamily::CargoTest | CommandFamily::CargoClippy => {
            effects.insert(ToolPermissionEffect::FileWrite);
            effects.insert(ToolPermissionEffect::ExecuteWorkspaceCode);
        }
        _ => {
            effects.insert(ToolPermissionEffect::Unknown);
        }
    }
}

fn collect_git_effects(args: &[String], effects: &mut BTreeSet<ToolPermissionEffect>) {
    effects.insert(ToolPermissionEffect::FileRead);
    if args.iter().any(|arg| git_arg_may_execute_program(arg)) {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        return;
    }
    let Some((subcommand, subcommand_args)) = git_subcommand_and_args(args) else {
        effects.insert(ToolPermissionEffect::Unknown);
        return;
    };
    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "rev-parse" | "ls-files" | "grep"
        | "branch" => {
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "stash" if matches!(subcommand_args, [operation] if operation == "list") => {
            effects.insert(ToolPermissionEffect::ExecuteTrustedBinary);
        }
        "fetch" | "pull" | "clone" | "ls-remote" => {
            effects.insert(ToolPermissionEffect::NetworkRead);
            effects.insert(ToolPermissionEffect::FileWrite);
        }
        "push" | "send-pack" => {
            effects.insert(ToolPermissionEffect::NetworkMutate);
            effects.insert(ToolPermissionEffect::RemoteMutation);
        }
        "commit" | "merge" | "rebase" | "checkout" | "restore" | "reset" | "clean" | "add"
        | "rm" | "mv" | "tag" => {
            effects.insert(ToolPermissionEffect::FileWrite);
            if matches!(subcommand, "clean" | "rm") {
                effects.insert(ToolPermissionEffect::FileDelete);
            }
        }
        "remote" | "submodule" => {
            effects.insert(ToolPermissionEffect::NetworkUnknown);
            effects.insert(ToolPermissionEffect::RemoteMutation);
        }
        _ => {
            effects.insert(ToolPermissionEffect::Unknown);
        }
    }
}

fn git_subcommand_and_args(args: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0usize;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            return args
                .get(index + 1)
                .map(|subcommand| (subcommand.as_str(), &args[index + 2..]));
        }
        if matches!(
            arg.as_str(),
            "-C" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            index = index.saturating_add(2);
            continue;
        }
        if arg.starts_with("--git-dir=")
            || arg.starts_with("--work-tree=")
            || arg.starts_with("--namespace=")
            || matches!(
                arg.as_str(),
                "--no-pager" | "--literal-pathspecs" | "--no-optional-locks"
            )
        {
            index += 1;
            continue;
        }
        return (!arg.starts_with('-')).then_some((arg.as_str(), &args[index + 1..]));
    }
    None
}

fn collect_package_manager_effects(args: &[String], effects: &mut BTreeSet<ToolPermissionEffect>) {
    let subcommand = args.first().map(String::as_str);
    effects.insert(ToolPermissionEffect::FileRead);
    match subcommand {
        Some("test" | "run" | "exec" | "dlx" | "x") => {
            effects.insert(ToolPermissionEffect::ExecuteWorkspaceCode);
            effects.insert(ToolPermissionEffect::FileWrite);
        }
        Some("install" | "add" | "update" | "upgrade" | "remove" | "uninstall") => {
            effects.insert(ToolPermissionEffect::NetworkRead);
            effects.insert(ToolPermissionEffect::FileWrite);
            effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        }
        Some("publish" | "login" | "logout" | "owner" | "deprecate") => {
            effects.insert(ToolPermissionEffect::NetworkMutate);
            effects.insert(ToolPermissionEffect::RemoteMutation);
        }
        _ => {
            effects.insert(ToolPermissionEffect::NetworkUnknown);
            effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
        }
    }
}

fn collect_transfer_effects(args: &[String], effects: &mut BTreeSet<ToolPermissionEffect>) {
    effects.insert(ToolPermissionEffect::NetworkRead);
    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-X" | "--request" | "-d" | "--data" | "--data-binary" | "-T" | "--upload-file"
        ) || arg.starts_with("--request=")
            || arg.starts_with("--data=")
            || arg.starts_with("--upload-file=")
    }) {
        effects.insert(ToolPermissionEffect::NetworkMutate);
    }
    if args.iter().any(|arg| {
        matches!(arg.as_str(), "-o" | "--output" | "-O" | "--remote-name")
            || arg.starts_with("--output=")
    }) {
        effects.insert(ToolPermissionEffect::FileWrite);
    }
    if args.iter().any(|arg| {
        matches!(arg.as_str(), "-x" | "--proxy" | "-L" | "--location")
            || arg.starts_with("--proxy=")
    }) {
        effects.insert(ToolPermissionEffect::NetworkUnknown);
    }
}

fn collect_remote_cli_effects(
    program: &str,
    args: &[String],
    effects: &mut BTreeSet<ToolPermissionEffect>,
) {
    let read_only = match program {
        "gh" => args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "status" | "view" | "list")),
        "kubectl" => args
            .iter()
            .any(|arg| matches!(arg.as_str(), "get" | "describe" | "logs" | "api-resources")),
        "terraform" | "tofu" => args
            .first()
            .is_some_and(|arg| matches!(arg.as_str(), "show" | "validate" | "fmt" | "providers")),
        _ => false,
    };
    if read_only {
        effects.insert(ToolPermissionEffect::NetworkRead);
    } else {
        effects.insert(ToolPermissionEffect::NetworkMutate);
        effects.insert(ToolPermissionEffect::RemoteMutation);
    }
    if program == "kubectl"
        && args
            .iter()
            .any(|arg| matches!(arg.as_str(), "exec" | "port-forward"))
    {
        effects.insert(ToolPermissionEffect::ProcessControl);
    }
}

fn collect_container_effects(args: &[String], effects: &mut BTreeSet<ToolPermissionEffect>) {
    effects.insert(ToolPermissionEffect::ProcessControl);
    if args.iter().any(|arg| {
        arg == "--privileged"
            || arg == "-v"
            || arg == "--volume"
            || arg.starts_with("--volume=")
            || arg == "-H"
            || arg == "--host"
            || arg.starts_with("--host=")
            || arg.starts_with("--context=")
    }) {
        effects.insert(ToolPermissionEffect::FileWrite);
        effects.insert(ToolPermissionEffect::NetworkUnknown);
    }
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "push" | "login" | "logout"))
    {
        effects.insert(ToolPermissionEffect::NetworkMutate);
        effects.insert(ToolPermissionEffect::RemoteMutation);
    } else {
        effects.insert(ToolPermissionEffect::ExecuteDynamicCode);
    }
}

fn shell_containment_request(
    family: &CommandFamily,
    operation: ToolOperation,
    analysis_status: &ToolAnalysisStatus,
    subjects: &[ToolSubject],
) -> ExecutionContainmentRequest {
    if !analysis_status.is_complete() {
        return ExecutionContainmentRequest {
            environment: EnvironmentContainment::UserInherited,
            ..ExecutionContainmentRequest::default()
        };
    }
    let subjects_are_workspace_bounded = subjects
        .iter()
        .all(|subject| subject.scope != ToolSubjectScope::External);
    let filesystem = if matches!(operation, ToolOperation::ExecuteReadOnlyCommand)
        && subjects_are_workspace_bounded
    {
        FilesystemContainment::WorkspaceReadOnly
    } else if family.is_workspace_check() && subjects_are_workspace_bounded {
        FilesystemContainment::WorkspaceAndScratch
    } else if matches!(
        operation,
        ToolOperation::ExecuteMutatingCommand | ToolOperation::ExecuteDestructiveCommand
    ) && subjects_are_workspace_bounded
    {
        FilesystemContainment::WorkspaceWrite
    } else {
        FilesystemContainment::Unspecified
    };
    let environment = if family.is_workspace_read_only() || family.is_workspace_check() {
        EnvironmentContainment::Restricted
    } else {
        EnvironmentContainment::UserInherited
    };
    ExecutionContainmentRequest {
        filesystem,
        network: if family.is_workspace_read_only() || family.is_workspace_check() {
            // This is a requirement, not a receipt. In particular, macOS Seatbelt does not prove
            // that the requested network denial is effective.
            NetworkContainment::Deny
        } else {
            NetworkContainment::Unspecified
        },
        process: if matches!(operation, ToolOperation::ExecuteUnknownCommand) {
            ProcessContainment::Unspecified
        } else {
            ProcessContainment::OwnedTree
        },
        environment,
        persistent_process: false,
    }
}

fn shell_semantic_scope(
    family: &CommandFamily,
    analysis_status: &ToolAnalysisStatus,
    command: &str,
    ast_binding: &str,
) -> Option<ToolSemanticScope> {
    if !analysis_status.is_complete() {
        return None;
    }
    let mut scope = if family.is_workspace_check() {
        ToolSemanticScope::new("workspace_validation", 2)
    } else if family.is_workspace_read_only() {
        ToolSemanticScope::new("workspace_read_only_shell", 1)
    } else if family.is_workspace_mutating() {
        ToolSemanticScope::new("workspace_shell_mutation", 1)
    } else {
        return None;
    };
    match family {
        CommandFamily::CargoValidationChain { steps } => {
            scope.qualifiers.insert(
                "commands".to_owned(),
                steps
                    .iter()
                    .map(|step| step.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            scope
                .qualifiers
                .insert("compound".to_owned(), "true".to_owned());
        }
        CommandFamily::CheckTouched { tier } => {
            scope
                .qualifiers
                .insert("command".to_owned(), "workspace_check_script".to_owned());
            if let Some(tier) = tier {
                scope
                    .qualifiers
                    .insert("tier_sha256".to_owned(), sha256_hex(tier.as_bytes()));
            }
        }
        _ => {
            scope
                .qualifiers
                .insert("command".to_owned(), family.as_str().to_owned());
        }
    }
    let normalized_arguments = if family.is_workspace_check() {
        // Session grants for a known validation command bind the executable validation core, not
        // presentation-only pipes such as `tail`, `head`, or `grep`. The full AST remains bound by
        // the per-execution permission plan hash, so this only widens durable grant reuse.
        workspace_validation_semantic_tokens(command)
    } else {
        scope
            .qualifiers
            .insert("ast_sha256".to_owned(), ast_binding.to_owned());
        tokenize_shell_subject_words(command)
    };
    let normalized_arguments = serde_json::to_vec(&normalized_arguments).ok()?;
    scope.qualifiers.insert(
        "arguments_sha256".to_owned(),
        sha256_hex(&normalized_arguments),
    );
    Some(scope)
}

fn workspace_validation_semantic_tokens(command: &str) -> Vec<String> {
    let tokens = tokenize_shell_subject_words(command);
    let tokens = tokens
        .iter()
        .position(|token| matches!(token.as_str(), "&&" | ";"))
        .filter(|separator| tokens.first().is_some_and(|token| token == "cd") && *separator >= 2)
        .map_or(tokens.as_slice(), |separator| &tokens[separator + 1..]);
    let mut normalized = Vec::new();
    for segment in split_shell_command_segments(tokens) {
        let Some(primary) = split_shell_pipeline(segment).first().copied() else {
            continue;
        };
        let mut skip_redirection_target = false;
        for token in primary {
            if skip_redirection_target {
                skip_redirection_target = false;
                continue;
            }
            if is_fd_duplication_token(token) || redirection_target(token).is_some() {
                continue;
            }
            if is_redirection_operator(token) {
                skip_redirection_target = true;
                continue;
            }
            normalized.push(token.clone());
        }
        normalized.push(";".to_owned());
    }
    if normalized.last().is_some_and(|token| token == ";") {
        normalized.pop();
    }
    normalized
}

fn shell_permission_summary(
    family: &CommandFamily,
    operation: ToolOperation,
    analysis_status: &ToolAnalysisStatus,
    has_file_write: bool,
) -> ToolPermissionSummary {
    let step_count = family.step_count();
    let workspace_code_steps = family.workspace_code_steps();
    let (title, detail) = if !analysis_status.is_complete() {
        (
            "Shell command requires review",
            "The command could not be fully classified by the structured shell analyzer".to_owned(),
        )
    } else if family.is_workspace_check() {
        (
            "Workspace validation",
            format!(
                "{step_count} validation step(s); {workspace_code_steps} execute workspace code"
            ),
        )
    } else if has_file_write {
        (
            "Shell command writes a file",
            "A shell redirection or command action writes a resolved path".to_owned(),
        )
    } else if family.is_workspace_mutating() {
        (
            "Shell command mutates files",
            "The recognized command contains a filesystem mutation".to_owned(),
        )
    } else {
        (
            "Read workspace information",
            format!("{step_count} bounded read-only shell step(s)"),
        )
    };
    ToolPermissionSummary {
        title: title.to_owned(),
        detail: if matches!(operation, ToolOperation::ExecuteDestructiveCommand) {
            format!("{detail}; destructive confirmation is required")
        } else {
            detail
        },
        step_count,
        workspace_code_steps,
    }
}

const MAX_SHELL_COMMAND_BYTES: usize = 64 * 1024;
const MAX_SHELL_AST_NODES: usize = 4_096;
const MAX_SHELL_AST_DEPTH: usize = 64;

struct ShellAstInspection {
    status: Option<ToolAnalysisStatus>,
    saw_readonly_structure: bool,
    verified_file_presence_loop: bool,
    normalized_ast_hash: String,
}

fn inspect_posix_shell_ast(
    command: &str,
    path_policy: &ShellPathPolicyBinding,
) -> ShellAstInspection {
    if command.len() > MAX_SHELL_COMMAND_BYTES {
        return ShellAstInspection {
            status: Some(ToolAnalysisStatus::Conservative {
                reasons: vec![ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::AnalysisLimitExceeded,
                    Some("shell command exceeds the 64 KiB analysis limit".to_owned()),
                )],
            }),
            saw_readonly_structure: false,
            verified_file_presence_loop: false,
            normalized_ast_hash: format!("limit:{}", sha256_hex(command.as_bytes())),
        };
    }
    let mut parser = Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return ShellAstInspection {
            status: Some(ToolAnalysisStatus::Unsupported {
                reason: ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::UnsupportedSyntax,
                    Some("the POSIX shell parser is unavailable".to_owned()),
                ),
            }),
            saw_readonly_structure: false,
            verified_file_presence_loop: false,
            normalized_ast_hash: format!("unavailable:{}", sha256_hex(command.as_bytes())),
        };
    }
    let Some(tree) = parser.parse(command, None) else {
        return ShellAstInspection {
            status: Some(ToolAnalysisStatus::Invalid {
                reason: ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::InvalidSyntax,
                    Some("the POSIX shell parser did not produce an AST".to_owned()),
                ),
            }),
            saw_readonly_structure: false,
            verified_file_presence_loop: false,
            normalized_ast_hash: format!("invalid:{}", sha256_hex(command.as_bytes())),
        };
    };
    let root = tree.root_node();
    if root.has_error() || root.is_missing() {
        return ShellAstInspection {
            status: Some(ToolAnalysisStatus::Invalid {
                reason: ToolAnalysisReason::new(
                    ToolAnalysisReasonCode::InvalidSyntax,
                    Some("the POSIX shell AST contains an error or missing node".to_owned()),
                ),
            }),
            saw_readonly_structure: false,
            verified_file_presence_loop: false,
            normalized_ast_hash: format!("invalid:{}", sha256_hex(command.as_bytes())),
        };
    }
    let shell_tokens = tokenize_shell_subject_words(command);
    let bounded_loop = parse_bounded_file_presence_command(&shell_tokens);
    let allow_file_presence_loop = bounded_loop.as_ref().is_some_and(|loop_spec| {
        posix_ast_matches_bounded_file_presence_loop(root, command.as_bytes(), loop_spec)
    });
    let allow_controlled_heredoc =
        shell_command_uses_controlled_scratch_heredoc(command, path_policy);
    let mut state = ShellAstWalkState {
        nodes: 0,
        saw_readonly_structure: false,
        allow_file_presence_loop,
        allow_controlled_heredoc,
        path_policy,
    };
    let status = bounded_loop
        .is_some_and(|_| !allow_file_presence_loop)
        .then(|| ToolAnalysisStatus::Unsupported {
            reason: ToolAnalysisReason::new(
                ToolAnalysisReasonCode::UnsupportedSyntax,
                Some(
                    "the bounded file-presence token shape does not match the POSIX shell AST"
                        .to_owned(),
                ),
            ),
        })
        .or_else(|| inspect_posix_shell_ast_node(root, command.as_bytes(), 0, &mut state))
        .or_else(|| {
            ast_glob_may_expand_to_option(root, command.as_bytes()).then(|| {
                ToolAnalysisStatus::Conservative {
                    reasons: vec![ToolAnalysisReason::new(
                        ToolAnalysisReasonCode::DynamicCommand,
                        Some("an unquoted glob may expand to a command-line option".to_owned()),
                    )],
                }
            })
        });
    ShellAstInspection {
        status,
        saw_readonly_structure: state.saw_readonly_structure,
        verified_file_presence_loop: allow_file_presence_loop,
        normalized_ast_hash: sha256_hex(root.to_sexp().as_bytes()),
    }
}

fn posix_ast_matches_bounded_file_presence_loop(
    root: Node<'_>,
    source: &[u8],
    loop_spec: &BoundedFilePresenceLoop<'_>,
) -> bool {
    let mut loop_node = None;
    if !find_unique_posix_ast_node(root, "for_statement", &mut loop_node) {
        return false;
    }
    let Some(loop_node) = loop_node else {
        return false;
    };
    if root.kind() != "program" {
        return false;
    }
    let root_children = posix_ast_named_children(root);
    let prefix_segments = split_shell_command_segments(loop_spec.prefix);
    if root_children.len() != prefix_segments.len().saturating_add(1)
        || root_children
            .last()
            .is_none_or(|last| !posix_ast_same_node(*last, loop_node))
        || root_children
            .iter()
            .zip(prefix_segments)
            .any(|(node, tokens)| !posix_ast_prefix_command_matches(*node, source, tokens))
    {
        return false;
    }
    if source
        .get(loop_node.end_byte()..)
        .is_none_or(|suffix| !suffix.iter().all(u8::is_ascii_whitespace))
        || loop_node
            .child(0)
            .is_none_or(|keyword| keyword.kind() != "for" || keyword.utf8_text(source) != Ok("for"))
    {
        return false;
    }

    let Some(variable) = loop_node.child_by_field_name("variable") else {
        return false;
    };
    if variable.kind() != "variable_name" || variable.utf8_text(source) != Ok(loop_spec.variable) {
        return false;
    }
    let mut value_cursor = loop_node.walk();
    let values = loop_node
        .children_by_field_name("value", &mut value_cursor)
        .collect::<Vec<_>>();
    if values.len() != loop_spec.items.len()
        || values
            .iter()
            .zip(loop_spec.items)
            .any(|(value, expected)| !posix_ast_static_literal_matches(*value, source, expected))
    {
        return false;
    }

    let Some(body) = loop_node.child_by_field_name("body") else {
        return false;
    };
    let body_children = posix_ast_named_children(body);
    let [if_statement] = body_children.as_slice() else {
        return false;
    };
    body.kind() == "do_group"
        && posix_ast_matches_bounded_presence_if(*if_statement, source, loop_spec)
}

fn posix_ast_prefix_command_matches(node: Node<'_>, source: &[u8], tokens: &[String]) -> bool {
    if node.kind() != "command" || tokens.is_empty() {
        return false;
    }
    let Some(name) = node.child_by_field_name("name") else {
        return false;
    };
    let name_children = posix_ast_named_children(name);
    let Ok(name_text) = name.utf8_text(source) else {
        return false;
    };
    let Ok(command_text) = node.utf8_text(source) else {
        return false;
    };
    name.kind() == "command_name"
        && matches!(name_children.as_slice(), [word] if word.kind() == "word")
        && name_text == tokens[0]
        && !name_text
            .chars()
            .any(|character| matches!(character, '/' | '\\'))
        && tokenize_shell_subject_words(command_text) == tokens
}

fn find_unique_posix_ast_node<'tree>(
    node: Node<'tree>,
    kind: &str,
    found: &mut Option<Node<'tree>>,
) -> bool {
    if node.kind() == kind && found.replace(node).is_some() {
        return false;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .all(|child| find_unique_posix_ast_node(child, kind, found))
}

fn posix_ast_named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn posix_ast_static_literal_matches(node: Node<'_>, source: &[u8], expected: &str) -> bool {
    let Ok(text) = node.utf8_text(source) else {
        return false;
    };
    let tokens = tokenize_shell_subject_words(text);
    matches!(tokens.as_slice(), [actual] if actual == expected)
        && !posix_ast_contains_expansion(node)
}

fn posix_ast_contains_expansion(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "expansion"
            | "simple_expansion"
            | "command_substitution"
            | "process_substitution"
            | "arithmetic_expansion"
    ) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(posix_ast_contains_expansion)
}

fn posix_ast_matches_bounded_presence_if(
    node: Node<'_>,
    source: &[u8],
    loop_spec: &BoundedFilePresenceLoop<'_>,
) -> bool {
    if node.kind() != "if_statement" {
        return false;
    }
    let children = posix_ast_named_children(node);
    let [condition, then_command, else_clause] = children.as_slice() else {
        return false;
    };
    let else_children = posix_ast_named_children(*else_clause);
    let [else_command] = else_children.as_slice() else {
        return false;
    };
    node.child_by_field_name("condition")
        .is_some_and(|field| posix_ast_same_node(field, *condition))
        && posix_ast_matches_bounded_presence_test(*condition, source, loop_spec)
        && posix_ast_is_unquoted_echo_command(*then_command, source, loop_spec.variable)
        && else_clause.kind() == "else_clause"
        && posix_ast_is_unquoted_echo_command(*else_command, source, loop_spec.variable)
}

fn posix_ast_matches_bounded_presence_test(
    node: Node<'_>,
    source: &[u8],
    loop_spec: &BoundedFilePresenceLoop<'_>,
) -> bool {
    if node.kind() != "test_command"
        || node.child(0).is_none_or(|open| open.kind() != "[")
        || node
            .child(node.child_count().saturating_sub(1) as u32)
            .is_none_or(|close| close.kind() != "]")
    {
        return false;
    }
    let children = posix_ast_named_children(node);
    let [unary] = children.as_slice() else {
        return false;
    };
    if unary.kind() != "unary_expression" {
        return false;
    }
    let Some(operator) = unary.child_by_field_name("operator") else {
        return false;
    };
    let expected_operator = match loop_spec.path_kind {
        FilePresenceLoopPathKind::ListedPath => "-f",
        FilePresenceLoopPathKind::WorkspaceGitMetadata => "-e",
    };
    if operator.kind() != "test_operator" || operator.utf8_text(source) != Ok(expected_operator) {
        return false;
    }
    let operands = posix_ast_named_children(*unary)
        .into_iter()
        .filter(|child| !posix_ast_same_node(*child, operator))
        .collect::<Vec<_>>();
    let [operand] = operands.as_slice() else {
        return false;
    };
    let expected_path = match loop_spec.path_kind {
        FilePresenceLoopPathKind::ListedPath => format!("${}", loop_spec.variable),
        FilePresenceLoopPathKind::WorkspaceGitMetadata => {
            format!(".git/${}", loop_spec.variable)
        }
    };
    let Ok(operand_text) = operand.utf8_text(source) else {
        return false;
    };
    let operand_tokens = tokenize_shell_subject_words(operand_text);
    matches!(operand_tokens.as_slice(), [actual] if actual == &expected_path)
        && posix_ast_expansions_match_loop_variable(*operand, source, loop_spec.variable, true)
}

fn posix_ast_is_unquoted_echo_command(node: Node<'_>, source: &[u8], variable: &str) -> bool {
    if node.kind() != "command" {
        return false;
    }
    let Some(name) = node.child_by_field_name("name") else {
        return false;
    };
    let name_children = posix_ast_named_children(name);
    if name.kind() != "command_name"
        || !matches!(name_children.as_slice(), [word] if word.kind() == "word")
        || name.utf8_text(source) != Ok("echo")
    {
        return false;
    }
    let mut argument_cursor = node.walk();
    let arguments = node
        .children_by_field_name("argument", &mut argument_cursor)
        .collect::<Vec<_>>();
    let mut redirect_cursor = node.walk();
    !arguments.is_empty()
        && node
            .children_by_field_name("redirect", &mut redirect_cursor)
            .next()
            .is_none()
        && arguments.iter().all(|argument| {
            posix_ast_expansions_match_loop_variable(*argument, source, variable, false)
        })
}

fn posix_ast_expansions_match_loop_variable(
    node: Node<'_>,
    source: &[u8],
    variable: &str,
    require_expansion: bool,
) -> bool {
    fn visit(node: Node<'_>, source: &[u8], expected: &str, count: &mut usize) -> bool {
        if matches!(node.kind(), "expansion" | "simple_expansion") {
            if node.utf8_text(source) != Ok(expected) {
                return false;
            }
            *count = count.saturating_add(1);
        } else if matches!(
            node.kind(),
            "command_substitution" | "process_substitution" | "arithmetic_expansion"
        ) {
            return false;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .all(|child| visit(child, source, expected, count))
    }

    let expected = format!("${variable}");
    let mut expansion_count = 0usize;
    visit(node, source, &expected, &mut expansion_count)
        && (!require_expansion || expansion_count == 1)
}

fn posix_ast_same_node(left: Node<'_>, right: Node<'_>) -> bool {
    left.kind() == right.kind()
        && left.start_byte() == right.start_byte()
        && left.end_byte() == right.end_byte()
}

fn ast_glob_may_expand_to_option(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() == "command" {
        let mut cursor = node.walk();
        let mut after_option_terminator = false;
        for child in node.named_children(&mut cursor) {
            if child.kind() == "command_name" {
                continue;
            }
            let Ok(word) = child.utf8_text(source) else {
                return true;
            };
            if word == "--" {
                after_option_terminator = true;
                continue;
            }
            if !after_option_terminator
                && child.kind() == "word"
                && word
                    .chars()
                    .next()
                    .is_some_and(|ch| matches!(ch, '*' | '?' | '['))
            {
                return true;
            }
        }
    }
    for index in 0..node.child_count() {
        if node
            .child(index as u32)
            .is_some_and(|child| ast_glob_may_expand_to_option(child, source))
        {
            return true;
        }
    }
    false
}

struct ShellAstWalkState<'a> {
    nodes: usize,
    saw_readonly_structure: bool,
    allow_file_presence_loop: bool,
    allow_controlled_heredoc: bool,
    path_policy: &'a ShellPathPolicyBinding,
}

fn inspect_posix_shell_ast_node(
    node: Node<'_>,
    source: &[u8],
    depth: usize,
    state: &mut ShellAstWalkState<'_>,
) -> Option<ToolAnalysisStatus> {
    state.nodes = state.nodes.saturating_add(1);
    if state.nodes > MAX_SHELL_AST_NODES || depth > MAX_SHELL_AST_DEPTH {
        return Some(ToolAnalysisStatus::Conservative {
            reasons: vec![ToolAnalysisReason::new(
                ToolAnalysisReasonCode::AnalysisLimitExceeded,
                Some("shell AST exceeds the node or depth analysis limit".to_owned()),
            )],
        });
    }
    if node.is_error() || node.is_missing() {
        return Some(ToolAnalysisStatus::Invalid {
            reason: ToolAnalysisReason::new(
                ToolAnalysisReasonCode::InvalidSyntax,
                Some("the POSIX shell AST contains an error or missing node".to_owned()),
            ),
        });
    }
    let kind = node.kind();
    if matches!(kind, "pipeline" | "list" | "redirected_statement") {
        state.saw_readonly_structure = true;
    }
    let allowed_loop_node = state.allow_file_presence_loop
        && matches!(
            kind,
            "for_statement" | "if_statement" | "expansion" | "simple_expansion"
        );
    let static_assignment = matches!(kind, "variable_assignment" | "variable_assignments")
        && node
            .utf8_text(source)
            .ok()
            .is_some_and(static_shell_assignment_is_analyzable);
    let bounded_symbolic_expansion = matches!(kind, "expansion" | "simple_expansion")
        && node.utf8_text(source).ok().is_some_and(|expansion| {
            shell_expansion_is_bounded_symbol(expansion, state.path_policy)
        });
    if !allowed_loop_node
        && !static_assignment
        && !bounded_symbolic_expansion
        && matches!(
            kind,
            "command_substitution"
                | "process_substitution"
                | "arithmetic_expansion"
                | "expansion"
                | "simple_expansion"
                | "variable_assignment"
                | "variable_assignments"
        )
    {
        return Some(ToolAnalysisStatus::Conservative {
            reasons: vec![ToolAnalysisReason::new(
                ToolAnalysisReasonCode::DynamicCommand,
                Some(format!("dynamic POSIX shell construct: {kind}")),
            )],
        });
    }
    let allowed_controlled_heredoc =
        state.allow_controlled_heredoc && matches!(kind, "heredoc_redirect" | "heredoc_body");
    if !allowed_loop_node
        && !allowed_controlled_heredoc
        && matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "while_statement"
                | "case_statement"
                | "function_definition"
                | "subshell"
                | "heredoc_redirect"
                | "heredoc_body"
                | "herestring_redirect"
                | "coproc"
                | "&"
        )
    {
        return Some(ToolAnalysisStatus::Unsupported {
            reason: ToolAnalysisReason::new(
                ToolAnalysisReasonCode::UnsupportedSyntax,
                Some(format!("unsupported POSIX shell construct: {kind}")),
            ),
        });
    }
    for index in 0..node.child_count() {
        if let Some(child) = node.child(index as u32)
            && let Some(status) = inspect_posix_shell_ast_node(child, source, depth + 1, state)
        {
            return Some(status);
        }
    }
    None
}

fn static_shell_assignment_is_analyzable(assignment: &str) -> bool {
    let Some((name, value)) = assignment.split_once('=') else {
        return false;
    };
    is_shell_identifier(name)
        && !matches!(name, "BASH_ENV" | "ENV" | "PROMPT_COMMAND" | "SHELLOPTS")
        && !name.starts_with("BASH_FUNC_")
        && !value.contains('$')
        && !value.contains('`')
        && !value.contains("$(")
        && !value.contains("<(")
        && !value.contains(">(")
}

fn shell_expansion_is_bounded_symbol(
    expansion: &str,
    path_policy: &ShellPathPolicyBinding,
) -> bool {
    matches!(expansion, "$PWD" | "${PWD}")
        || path_policy.scratch_root.is_some()
            && matches!(expansion, "$SIGIL_SCRATCH_DIR" | "${SIGIL_SCRATCH_DIR}")
        || path_policy.sandbox_tmpdir_root.is_some() && matches!(expansion, "$TMPDIR" | "${TMPDIR}")
}

fn shell_syntax_guidance(shell: &ResolvedShell) -> String {
    match shell.dialect() {
        ShellDialect::Posix => format!("POSIX shell ({})", shell.program().display()),
        ShellDialect::PowerShell => format!(
            "PowerShell ({}) with PowerShell syntax such as `$env:NAME` and `$null`",
            shell.program().display()
        ),
        ShellDialect::Cmd => format!("cmd.exe ({}) syntax", shell.program().display()),
    }
}

fn workspace_check_grant_scope(family: &CommandFamily) -> Option<CommandGrantScope> {
    match family {
        CommandFamily::CheckTouched { tier } => Some(CommandGrantScope::WorkspaceScript {
            path: "scripts/check-touched.sh".to_owned(),
            args_family: tier.clone(),
        }),
        _ => Some(CommandGrantScope::WorkspaceCheckFamily),
    }
}

#[cfg(test)]
pub(crate) fn bash_execution_request(
    command: &str,
    workspace_root: &Path,
    scratch_root: &Path,
    timeout_secs: u64,
) -> ExecutionRequest {
    let shell = ResolvedShell::resolve_explicit("sh").expect("sh is a supported shell");
    let analysis = analyze_shell_command_with_shell(workspace_root, command, &shell)
        .expect("test shell command analysis should succeed");
    bash_execution_request_with_shell(
        command,
        workspace_root,
        scratch_root,
        timeout_secs,
        &shell,
        &analysis,
    )
}

#[cfg(test)]
pub(crate) fn bash_execution_request_with_shell(
    command: &str,
    workspace_root: &Path,
    scratch_root: &Path,
    timeout_secs: u64,
    shell: &ResolvedShell,
    analysis: &ShellCommandAnalysis,
) -> ExecutionRequest {
    bash_execution_request_from_containment(
        command,
        workspace_root,
        scratch_root,
        timeout_secs,
        shell,
        analysis.containment.environment,
        None,
    )
}

fn bash_execution_request_from_containment(
    command: &str,
    workspace_root: &Path,
    scratch_root: &Path,
    timeout_secs: u64,
    shell: &ResolvedShell,
    environment_containment: EnvironmentContainment,
    file_presence_profile: Option<&FilePresenceExecutionProfile>,
) -> ExecutionRequest {
    let restricted_environment = environment_containment == EnvironmentContainment::Restricted;
    let mut env = if restricted_environment {
        controlled_shell_environment()
    } else {
        BTreeMap::new()
    };
    env.insert(
        SIGIL_SCRATCH_DIR_ENV.to_owned(),
        scratch_root.to_string_lossy().into_owned(),
    );
    if restricted_environment {
        env.insert(
            "TMPDIR".to_owned(),
            scratch_root.to_string_lossy().into_owned(),
        );
    }
    if let Some(profile) = file_presence_profile {
        profile.apply_to_environment(&mut env);
    }
    ExecutionRequest {
        program: file_presence_profile.map_or_else(
            || shell.program_string(),
            |profile| profile.shell_program.to_string_lossy().into_owned(),
        ),
        args: shell.one_shot_args(command),
        cwd: workspace_root.to_path_buf(),
        env,
        environment_policy: if restricted_environment {
            sigil_kernel::ProcessEnvironmentPolicy::IsolatedExtension
        } else {
            sigil_kernel::ProcessEnvironmentPolicy::InheritParent
        },
        timeout_ms: None,
        timeout_secs,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
        capture: None,
    }
}

fn controlled_shell_environment() -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    for name in ["PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ"] {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.to_owned(), value);
        }
    }
    environment.entry("PATH".to_owned()).or_insert_with(|| {
        "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned()
    });
    environment
}

fn file_presence_execution_profile_for_binding(
    workspace_root: &Path,
    shell: &ResolvedShell,
    expected_binding: Option<&String>,
) -> Result<Option<FilePresenceExecutionProfile>> {
    let Some(expected_binding) = expected_binding else {
        return Ok(None);
    };
    let profile = file_presence_execution_profile_with_environment(
        workspace_root,
        shell,
        &controlled_shell_environment(),
    )
    .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        profile.binding == *expected_binding,
        "bounded file-presence executable binding changed before execution"
    );
    Ok(Some(profile))
}

fn file_presence_execution_profile_with_environment(
    workspace_root: &Path,
    shell: &ResolvedShell,
    environment: &BTreeMap<String, String>,
) -> std::result::Result<FilePresenceExecutionProfile, String> {
    if shell.dialect() != ShellDialect::Posix {
        return Err("the bounded file-presence profile requires a POSIX shell".to_owned());
    }

    #[cfg(not(unix))]
    {
        let _ = (workspace_root, environment);
        return Err(
            "the bounded file-presence executable profile is unavailable on this platform"
                .to_owned(),
        );
    }

    #[cfg(unix)]
    {
        let shell_identity =
            trusted_executable_identity(workspace_root, Path::new("/bin/sh"), "POSIX shell", None)?;
        let path = environment
            .get("PATH")
            .ok_or_else(|| "the controlled shell PATH is unavailable".to_owned())?;
        let git_identity = resolve_trusted_git_from_path(workspace_root, path)?;
        let binding_payload = serde_json::to_vec(&json!({
            "version": FILE_PRESENCE_EXECUTION_PROFILE_VERSION,
            "shell": shell_identity.binding,
            "git": git_identity.binding,
            "git_environment_profile": "bounded-read-v1",
        }))
        .map_err(|error| format!("failed to encode executable binding: {error}"))?;
        let binding = format!(
            "file-presence-exec-v{FILE_PRESENCE_EXECUTION_PROFILE_VERSION}:{}",
            sha256_hex(&binding_payload)
        );
        Ok(FilePresenceExecutionProfile {
            binding,
            shell_program: shell_identity.program,
            git_program: git_identity.program,
        })
    }
}

#[cfg(unix)]
fn resolve_trusted_git_from_path(
    workspace_root: &Path,
    path: &str,
) -> std::result::Result<TrustedExecutableIdentity, String> {
    let canonical_workspace = fs::canonicalize(workspace_root).map_err(|error| {
        format!("failed to resolve workspace while binding trusted git: {error}")
    })?;
    let path = OsString::from(path);
    let mut rejected_candidates = Vec::new();
    for directory in std::env::split_paths(&path) {
        if directory.as_os_str().is_empty() || !directory.is_absolute() {
            return Err(format!(
                "the controlled PATH contains a relative entry before trusted git: {}",
                directory.display()
            ));
        }
        let candidate = directory.join("git");
        if fs::symlink_metadata(&candidate).is_err() {
            continue;
        }
        let canonical_candidate = fs::canonicalize(&candidate)
            .map_err(|error| format!("failed to resolve git executable: {error}"))?;
        if canonical_candidate.starts_with(&canonical_workspace) {
            return Err(format!(
                "the git executable resolves inside the workspace: {}",
                canonical_candidate.display()
            ));
        }
        match trusted_executable_identity(
            workspace_root,
            &candidate,
            "git",
            Some(OsStr::new("git")),
        ) {
            Ok(identity) => return Ok(identity),
            Err(error) => rejected_candidates.push(error),
        }
    }
    Err(rejected_candidates
        .pop()
        .unwrap_or_else(|| "git was not found on the controlled PATH".to_owned()))
}

#[cfg(unix)]
fn trusted_executable_identity(
    workspace_root: &Path,
    candidate: &Path,
    label: &str,
    expected_canonical_name: Option<&OsStr>,
) -> std::result::Result<TrustedExecutableIdentity, String> {
    let program = fs::canonicalize(candidate)
        .map_err(|error| format!("failed to resolve trusted {label} executable: {error}"))?;
    let canonical_workspace = fs::canonicalize(workspace_root).map_err(|error| {
        format!("failed to resolve workspace while binding trusted {label}: {error}")
    })?;
    if program.starts_with(&canonical_workspace) {
        return Err(format!(
            "the {label} executable resolves inside the workspace: {}",
            program.display()
        ));
    }
    if expected_canonical_name.is_some_and(|expected| program.file_name() != Some(expected)) {
        return Err(format!(
            "the {label} executable resolves to a different command identity: {}",
            program.display()
        ));
    }

    let metadata = fs::metadata(&program)
        .map_err(|error| format!("failed to inspect trusted {label} executable: {error}"))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(format!(
            "the {label} executable is not a root-owned, non-writable executable: {}",
            program.display()
        ));
    }
    let mut ancestor = program.parent();
    while let Some(directory) = ancestor {
        let directory_metadata = fs::metadata(directory).map_err(|error| {
            format!(
                "failed to inspect trusted {label} executable directory {}: {error}",
                directory.display()
            )
        })?;
        if directory_metadata.uid() != 0 || directory_metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "the {label} executable directory is not root-owned and non-writable: {}",
                directory.display()
            ));
        }
        ancestor = directory.parent();
    }
    let content = fs::read(&program)
        .map_err(|error| format!("failed to bind trusted {label} executable bytes: {error}"))?;
    let binding = json!({
        "canonical_path": program.to_string_lossy(),
        "device": metadata.dev(),
        "inode": metadata.ino(),
        "mode": metadata.permissions().mode(),
        "uid": metadata.uid(),
        "size": metadata.size(),
        "modified_secs": metadata.mtime(),
        "modified_nanos": metadata.mtime_nsec(),
        "sha256": sha256_hex(&content),
    });
    Ok(TrustedExecutableIdentity { program, binding })
}

#[cfg(test)]
pub(crate) fn analyze_shell_command_with_controlled_environment(
    workspace_root: &Path,
    command: &str,
    environment: &BTreeMap<String, String>,
) -> Result<ShellCommandAnalysis> {
    let shell = ResolvedShell::resolve_explicit("sh")?;
    analyze_shell_command_with_path_policy_and_environment(
        workspace_root,
        command,
        &shell,
        &ShellPathPolicyBinding::default(),
        environment,
    )
}

#[cfg(test)]
pub(crate) fn bounded_file_presence_execution_environment(
    workspace_root: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<(PathBuf, BTreeMap<String, String>)> {
    let shell = ResolvedShell::resolve_explicit("sh")?;
    let profile =
        file_presence_execution_profile_with_environment(workspace_root, &shell, environment)
            .map_err(anyhow::Error::msg)?;
    let mut execution_environment = environment.clone();
    profile.apply_to_environment(&mut execution_environment);
    Ok((profile.shell_program, execution_environment))
}

pub(crate) fn shell_environment_binding(
    ctx: &ToolContext,
    scratch_root: &Path,
    shell: &ResolvedShell,
    environment_containment: EnvironmentContainment,
) -> Result<String> {
    shell_environment_binding_with_profile(ctx, scratch_root, shell, environment_containment, None)
}

fn shell_environment_binding_with_profile(
    ctx: &ToolContext,
    scratch_root: &Path,
    shell: &ResolvedShell,
    environment_containment: EnvironmentContainment,
    file_presence_profile: Option<&FilePresenceExecutionProfile>,
) -> Result<String> {
    let scratch_root = absolute_path_from(&ctx.workspace_root, scratch_root);
    let restricted = environment_containment == EnvironmentContainment::Restricted;
    let mut environment = if restricted {
        controlled_shell_environment()
    } else {
        std::env::vars().collect::<BTreeMap<_, _>>()
    };
    environment.insert(
        SIGIL_SCRATCH_DIR_ENV.to_owned(),
        scratch_root.to_string_lossy().into_owned(),
    );
    if restricted {
        environment.insert(
            "TMPDIR".to_owned(),
            scratch_root.to_string_lossy().into_owned(),
        );
    }
    if let Some(profile) = file_presence_profile {
        profile.apply_to_environment(&mut environment);
    }
    let canonical = serde_json::to_vec(&json!({
        "policy_version": SHELL_ENVIRONMENT_POLICY_VERSION,
        "profile": if restricted { "restricted" } else { "user_inherited" },
        "shell_program": file_presence_profile.map_or_else(
            || shell.program_string(),
            |profile| profile.shell_program.to_string_lossy().into_owned(),
        ),
        "environment": environment,
    }))?;
    Ok(format!(
        "shell-env-v{SHELL_ENVIRONMENT_POLICY_VERSION}:{}",
        sha256_hex(&canonical)
    ))
}

#[cfg(test)]
pub(crate) fn bash_tool_result_from_execution_receipt(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
) -> Result<ToolResult> {
    bash_tool_result_from_execution_receipt_inner(call_id, tool_name, receipt, None)
}

pub(crate) fn bash_tool_result_from_execution_receipt_with_analysis(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
    analysis: &ShellCommandAnalysis,
) -> Result<ToolResult> {
    bash_tool_result_from_execution_receipt_inner(
        call_id,
        tool_name,
        receipt,
        Some(ShellReceiptContext::Analysis(analysis)),
    )
}

fn bash_tool_result_from_execution_receipt_with_plan(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
    command: &str,
    shell: &ResolvedShell,
    plan: &sigil_kernel::ToolPermissionPlanV2,
) -> Result<ToolResult> {
    bash_tool_result_from_execution_receipt_inner(
        call_id,
        tool_name,
        receipt,
        Some(ShellReceiptContext::Prepared {
            command,
            shell,
            plan,
        }),
    )
}

#[derive(Clone, Copy)]
enum ShellReceiptContext<'a> {
    Analysis(&'a ShellCommandAnalysis),
    Prepared {
        command: &'a str,
        shell: &'a ResolvedShell,
        plan: &'a sigil_kernel::ToolPermissionPlanV2,
    },
}

fn bash_tool_result_from_execution_receipt_inner(
    call_id: String,
    tool_name: String,
    mut receipt: ExecutionReceipt,
    shell_context: Option<ShellReceiptContext<'_>>,
) -> Result<ToolResult> {
    let capture_outcome = receipt.capture.take();
    let output = receipt.effective_output();
    let limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
    let limited_stdout = captured_stream_text(&receipt.stdout, &output.stdout, limit_bytes);
    let limited_stderr = captured_stream_text(&receipt.stderr, &output.stderr, limit_bytes);
    let mut content = String::new();
    if !limited_stdout.content.is_empty() {
        content.push_str(&limited_stdout.content);
    }
    if !limited_stderr.content.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&limited_stderr.content);
    }
    let output_truncated = output.stdout.truncated
        || output.stderr.truncated
        || limited_stdout.truncated
        || limited_stderr.truncated;
    let tail_available = output.stdout.retained_tail_bytes > 0
        || output.stderr.retained_tail_bytes > 0
        || output_truncated;
    let metadata = ToolResultMeta {
        exit_code: receipt.exit_code,
        stdout_bytes: Some(output.stdout.total_bytes),
        stderr_bytes: Some(output.stderr.total_bytes),
        truncated: output_truncated,
        omitted_bytes: Some(
            limited_stdout
                .omitted_bytes
                .saturating_add(limited_stderr.omitted_bytes),
        ),
        limit_bytes: Some(limit_bytes as u64),
        returned_bytes: Some(
            limited_stdout
                .returned_bytes
                .saturating_add(limited_stderr.returned_bytes),
        ),
        total_bytes: Some(output.combined_total_bytes),
        returned_lines: Some(limited_stdout.returned_lines + limited_stderr.returned_lines),
        total_lines: Some(
            output
                .stdout
                .total_lines
                .saturating_add(output.stderr.total_lines),
        ),
        details: execution_receipt_details_with_context(
            &receipt,
            shell_context,
            output_truncated,
            tail_available,
        ),
        ..ToolResultMeta::default()
    };
    if let Some((kind, message)) = execution_termination_error(&output.termination) {
        let details = metadata.details.clone();
        let mut result =
            ToolResult::error(call_id, tool_name, kind, message).with_error_details(false, details);
        if !content.is_empty() {
            result.content = content;
        }
        result.metadata = metadata;
        if let Some(outcome) = capture_outcome {
            result.attach_capture_outcome(outcome);
        }
        return Ok(result);
    }
    if receipt.exit_code == Some(0) {
        let mut result = ToolResult::ok(call_id, tool_name, content, metadata);
        if let Some(outcome) = capture_outcome {
            result.attach_capture_outcome(outcome);
        }
        Ok(result)
    } else {
        let summary_end = floor_char_boundary(
            &content,
            sigil_kernel::TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES.min(content.len()),
        );
        let summary = &content[..summary_end];
        let message = if summary_end < content.len() {
            format!(
                "bash command exited with non-zero status; full output is available in the tool artifact ({summary})"
            )
        } else if summary.is_empty() {
            "bash command exited with non-zero status".to_owned()
        } else {
            summary.to_owned()
        };
        let syntax_error = content.to_ascii_lowercase().contains("syntax error")
            || content.to_ascii_lowercase().contains("parse error");
        let kind = if syntax_error {
            ToolErrorKind::InvalidInput
        } else {
            ToolErrorKind::ExitStatus
        };
        let mut result = ToolResult::error(call_id, tool_name, kind, message);
        if syntax_error {
            result = result.with_error_details(
                false,
                json!({ "category": "shell_syntax", "retryable": false }),
            );
        }
        result.content = content;
        result.metadata = metadata;
        if let Some(outcome) = capture_outcome {
            result.attach_capture_outcome(outcome);
        }
        Ok(result)
    }
}

fn captured_stream_text(
    bytes: &[u8],
    capture: &ExecutionStreamCapture,
    fallback_limit_bytes: usize,
) -> TextLimitResult {
    if !capture.truncated {
        let text = String::from_utf8_lossy(bytes);
        let mut limited = limit_text_head_tail(&text, fallback_limit_bytes);
        if limited.content.len() > fallback_limit_bytes {
            limited.content = bounded_text_projection(
                &text,
                fallback_limit_bytes,
                capture
                    .total_bytes
                    .saturating_sub(capture.returned_bytes.min(fallback_limit_bytes as u64)),
            );
        }
        limited.total_bytes = capture.total_bytes;
        limited.total_lines = capture.total_lines;
        limited.returned_bytes = limited
            .returned_bytes
            .min(capture.returned_bytes)
            .min(fallback_limit_bytes as u64);
        limited.omitted_bytes = capture.total_bytes.saturating_sub(limited.returned_bytes);
        return limited;
    }

    let head_len = usize::try_from(capture.retained_head_bytes)
        .unwrap_or(bytes.len())
        .min(bytes.len());
    let tail_len = usize::try_from(capture.retained_tail_bytes)
        .unwrap_or(bytes.len().saturating_sub(head_len))
        .min(bytes.len().saturating_sub(head_len));
    let head = String::from_utf8_lossy(&bytes[..head_len]);
    let tail = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(tail_len)..]);
    let retained = format!("{head}{tail}");
    let content = bounded_text_projection(&retained, fallback_limit_bytes, capture.omitted_bytes);
    let returned_bytes = capture
        .returned_bytes
        .min(fallback_limit_bytes as u64)
        .min(capture.total_bytes);
    TextLimitResult {
        returned_bytes,
        returned_lines: content.lines().count() as u64,
        total_bytes: capture.total_bytes,
        total_lines: capture.total_lines,
        truncated: true,
        omitted_bytes: capture.total_bytes.saturating_sub(returned_bytes),
        content,
    }
}

fn bounded_text_projection(input: &str, max_bytes: usize, omitted_bytes: u64) -> String {
    let notice = format!("[sigil: output truncated, omitted {omitted_bytes} bytes]");
    if max_bytes <= notice.len() {
        let end = floor_char_boundary(&notice, max_bytes);
        return notice[..end].to_owned();
    }
    let separators = 2usize;
    let raw_budget = max_bytes.saturating_sub(notice.len() + separators);
    let head_budget = raw_budget / 2;
    let tail_budget = raw_budget.saturating_sub(head_budget);
    let head_end = floor_char_boundary(input, head_budget.min(input.len()));
    let tail_start =
        ceil_char_boundary(input, input.len().saturating_sub(tail_budget)).max(head_end);
    format!("{}\n{notice}\n{}", &input[..head_end], &input[tail_start..])
}

fn execution_termination_error(
    termination: &ExecutionTerminationCause,
) -> Option<(ToolErrorKind, &'static str)> {
    match termination {
        ExecutionTerminationCause::Exited => None,
        ExecutionTerminationCause::TimedOut => {
            Some((ToolErrorKind::Timeout, "bash command timed out"))
        }
        ExecutionTerminationCause::Cancelled => Some((
            ToolErrorKind::Interrupted,
            "bash command interrupted by run cancellation",
        )),
        ExecutionTerminationCause::OutputLimit { .. } => Some((
            ToolErrorKind::ResourceLimit,
            "bash command exceeded the output limit",
        )),
        ExecutionTerminationCause::ReaderFailed { .. } => {
            Some((ToolErrorKind::Io, "bash command output reader failed"))
        }
    }
}

fn execution_receipt_details_with_context(
    receipt: &ExecutionReceipt,
    shell_context: Option<ShellReceiptContext<'_>>,
    output_truncated: bool,
    tail_available: bool,
) -> Value {
    let output = receipt.effective_output();
    let mut details = json!({
        "execution": {
            "backend": receipt.backend,
            "capabilities": receipt.capabilities,
            "network": receipt.network,
            "resources": receipt.resources,
        }
    });
    if !matches!(output.termination, ExecutionTerminationCause::Exited)
        || output.stdout.truncated
        || output.stderr.truncated
    {
        details["execution"]["output"] = execution_output_details(&output);
    }
    if let Some(shell_context) = shell_context {
        details["shell"] = match shell_context {
            ShellReceiptContext::Analysis(analysis) => json!({
                "program": analysis.shell_program.as_str(),
                "dialect": analysis.shell_dialect.as_str(),
                "command": analysis.command.as_str(),
                "normalized_command": analysis.normalized_command.as_str(),
                "command_family": analysis.command_family.as_str(),
                "classification_source": analysis.classification_source.as_str(),
                "call": {"summary": format!("command={}", analysis.command.as_str())},
                "grant_scope": analysis.grant_scope.as_ref().map(CommandGrantScope::as_str),
                "grant_scope_detail": shell_grant_scope_detail(analysis.grant_scope.as_ref()),
                "approval_reason": analysis.explanation.as_str(),
                "exit_code": receipt.exit_code,
                "verdict": shell_verdict(receipt),
                "output_truncated": output_truncated,
                "tail_available": tail_available,
                "rerun_not_needed": shell_rerun_not_needed(analysis, receipt),
            }),
            ShellReceiptContext::Prepared {
                command,
                shell,
                plan,
            } => json!({
                "program": shell.program_string(),
                "dialect": shell.dialect().as_str(),
                "command": command,
                "normalized_command": normalize_shell_command_for_permission(command),
                "call": {"summary": format!("command={command}")},
                "command_family": plan.semantic_scope.as_ref().map(|scope| scope.family.as_str()).unwrap_or("reviewed_shell"),
                "classification_source": "prepared_permission_plan_v2",
                "permission_plan_hash": plan.plan_hash.as_str(),
                "approval_reason": plan.operation.as_str(),
                "exit_code": receipt.exit_code,
                "verdict": shell_verdict(receipt),
                "output_truncated": output_truncated,
                "tail_available": tail_available,
                "rerun_not_needed": plan.operation == ToolOperation::ExecuteWorkspaceCheckCommand
                    && receipt.exit_code == Some(0)
                    && matches!(receipt.effective_output().termination, ExecutionTerminationCause::Exited),
            }),
        };
    }
    details
}

fn execution_output_details(output: &ExecutionOutputReceipt) -> Value {
    let mut details = json!({
        "termination": output.termination.as_str(),
        "stdout": &output.stdout,
        "stderr": &output.stderr,
        "combined_total_bytes": output.combined_total_bytes,
        "combined_hard_limit_bytes": output.combined_hard_limit_bytes,
    });
    match &output.termination {
        ExecutionTerminationCause::OutputLimit {
            stream,
            limit_bytes,
            observed_bytes,
        } => {
            details["code"] = json!("output_limit_exceeded");
            details["stream"] = json!(stream.as_str());
            details["limit_bytes"] = json!(limit_bytes);
            details["observed_bytes"] = json!(observed_bytes);
        }
        ExecutionTerminationCause::ReaderFailed { stream, reason } => {
            details["code"] = json!("output_reader_failed");
            details["stream"] = json!(stream.as_str());
            details["reason"] = json!(reason);
        }
        ExecutionTerminationCause::TimedOut => {
            details["code"] = json!("execution_timeout");
        }
        ExecutionTerminationCause::Cancelled => {
            details["code"] = json!("execution_cancelled");
        }
        ExecutionTerminationCause::Exited => {}
    }
    details
}

pub(crate) fn shell_grant_scope_detail(scope: Option<&CommandGrantScope>) -> Value {
    match scope {
        Some(CommandGrantScope::WorkspaceScript { path, args_family }) => json!({
            "path": path,
            "args_family": args_family,
        }),
        _ => Value::Null,
    }
}

fn shell_verdict(receipt: &ExecutionReceipt) -> &'static str {
    match receipt.effective_output().termination {
        ExecutionTerminationCause::TimedOut => "timed_out",
        ExecutionTerminationCause::Cancelled => "interrupted",
        ExecutionTerminationCause::OutputLimit { .. } => "resource_limited",
        ExecutionTerminationCause::ReaderFailed { .. } => "output_reader_failed",
        ExecutionTerminationCause::Exited => match receipt.exit_code {
            Some(0) => "passed",
            Some(_) => "failed",
            None => "unknown",
        },
    }
}

fn shell_rerun_not_needed(analysis: &ShellCommandAnalysis, receipt: &ExecutionReceipt) -> bool {
    analysis.command_family.is_workspace_check()
        && receipt.exit_code == Some(0)
        && matches!(
            receipt.effective_output().termination,
            ExecutionTerminationCause::Exited
        )
}

pub(crate) fn command_permission_subject(command: &str) -> String {
    const MAX_CHARS: usize = 120;
    let normalized = normalize_shell_command_for_permission(command);
    let char_count = normalized.chars().count();
    if char_count <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn normalize_shell_command_for_permission(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify_shell_command_family(workspace_root: &Path, command: &str) -> Result<CommandFamily> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let raw_tokens = tokenize_shell_subject_words(command);
    if bounded_file_presence_command_is_safe_readonly(&raw_tokens) {
        return Ok(CommandFamily::FilePresenceCheck);
    }
    let tokens = strip_workspace_cd_prefix(&workspace_root, raw_tokens)?;
    if tokens.is_empty() {
        return Ok(CommandFamily::Unknown);
    }
    let command_segments = split_shell_command_segments(&tokens);
    if command_segments.is_empty() {
        return Ok(CommandFamily::Unknown);
    }
    let segment_families = command_segments
        .iter()
        .map(|segment| command_family_for_pipeline(segment))
        .collect::<Vec<_>>();
    let cargo_steps = segment_families
        .iter()
        .filter_map(cargo_validation_step)
        .collect::<Vec<_>>();
    if !cargo_steps.is_empty()
        && segment_families
            .iter()
            .all(|family| cargo_validation_step(family).is_some() || family.is_shell_noop())
    {
        return Ok(if cargo_steps.len() == 1 && segment_families.len() == 1 {
            segment_families[0].clone()
        } else {
            CommandFamily::CargoValidationChain { steps: cargo_steps }
        });
    }
    if segment_families
        .iter()
        .all(|family| family.is_workspace_read_only())
    {
        return Ok(if segment_families.len() == 1 {
            segment_families[0].clone()
        } else {
            CommandFamily::ReadOnlyChain {
                step_count: segment_families.len(),
            }
        });
    }
    if segment_families.len() == 1 {
        return Ok(segment_families[0].clone());
    }
    Ok(CommandFamily::Unknown)
}

fn cargo_validation_step(family: &CommandFamily) -> Option<CargoValidationStep> {
    match family {
        CommandFamily::CargoFmtCheck => Some(CargoValidationStep::FmtCheck),
        CommandFamily::CargoCheck => Some(CargoValidationStep::Check),
        CommandFamily::CargoTest => Some(CargoValidationStep::Test),
        CommandFamily::CargoClippy => Some(CargoValidationStep::Clippy),
        _ => None,
    }
}

impl CommandFamily {
    fn is_shell_noop(&self) -> bool {
        matches!(self, Self::ShellNoop)
    }
}

fn strip_workspace_cd_prefix(workspace_root: &Path, tokens: Vec<String>) -> Result<Vec<String>> {
    let Some(separator_index) = tokens
        .iter()
        .position(|token| matches!(token.as_str(), "&&" | ";"))
    else {
        return Ok(tokens);
    };
    let prefix = &tokens[..separator_index];
    if !matches!(prefix.first().map(String::as_str), Some("cd")) {
        return Ok(tokens);
    }
    let Some(target) = prefix.get(1).filter(|target| !target.starts_with('-')) else {
        return Ok(tokens);
    };
    let resolved = resolve_tool_path_from_base(workspace_root, workspace_root, target)?;
    if resolved.scope != ToolSubjectScope::Workspace {
        return Ok(tokens);
    }
    Ok(tokens[separator_index + 1..].to_vec())
}

fn split_shell_command_segments(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if start < index {
                segments.push(&tokens[start..index]);
            }
            start = index.saturating_add(1);
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn command_family_for_pipeline(tokens: &[String]) -> CommandFamily {
    let pipeline = split_shell_pipeline(tokens);
    let Some(primary) = pipeline.first().copied() else {
        return CommandFamily::Unknown;
    };
    if pipeline
        .iter()
        .skip(1)
        .any(|filter| !shell_segment_is_read_filter(filter))
    {
        return CommandFamily::Unknown;
    }
    command_family_for_simple_segment(primary)
}

fn split_shell_pipeline(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if token == "|" {
            if start < index {
                segments.push(&tokens[start..index]);
            }
            start = index.saturating_add(1);
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn command_family_for_simple_segment(tokens: &[String]) -> CommandFamily {
    command_family_for_simple_segment_with_depth(tokens, 0)
}

const MAX_SHELL_WRAPPER_DEPTH: usize = 8;

fn command_family_for_simple_segment_with_depth(tokens: &[String], depth: usize) -> CommandFamily {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return CommandFamily::Unknown;
    }
    let words = tokens
        .iter()
        .filter(|token| !is_fd_duplication_token(token))
        .cloned()
        .collect::<Vec<_>>();
    let Some((command, args)) = shell_segment_command_and_args(&words) else {
        return CommandFamily::Unknown;
    };
    if let Some(inner) = static_wrapper_inner(command, args) {
        return command_family_for_simple_segment_with_depth(inner, depth + 1);
    }
    if matches!(command, "sh" | "bash" | "zsh") {
        return static_shell_payload(args)
            .map(|payload| classify_nested_shell_family(payload, depth + 1))
            .unwrap_or(CommandFamily::Unknown);
    }
    match command {
        "cargo" => cargo_command_family(args),
        "git" if git_segment_is_safe_readonly(&words) => CommandFamily::GitReadOnly,
        "grep" | "rg" if search_segment_is_read_only(command, args) => CommandFamily::Search,
        "find" if find_segment_is_safe_readonly(&words) => CommandFamily::Search,
        "find" if find_segment_has_delete_action(args) => CommandFamily::FindDelete,
        "find" if find_segment_has_write_action(args) => CommandFamily::FindWrite,
        "cat" if shell_segment_has_overwrite_redirection(&words) => CommandFamily::StaticFileWrite,
        "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "du" | "file" | "readlink"
        | "realpath" | "basename" | "dirname" | "diff" | "cmp" | "pwd" => CommandFamily::ListRead,
        "echo" | "printf" if shell_segment_has_overwrite_redirection(&words) => {
            CommandFamily::StaticFileWrite
        }
        "echo" | "printf" | "true" | ":" if shell_segment_redirections_are_readonly(&words) => {
            CommandFamily::ShellNoop
        }
        "set" if matches!(args, [option, value] if option == "-o" && value == "pipefail") => {
            CommandFamily::ShellNoop
        }
        "command" if matches!(args.first().map(String::as_str), Some("-v" | "-V")) => {
            CommandFamily::ListRead
        }
        _ if known_tool_help_or_version_query(&words) => CommandFamily::ListRead,
        command if command.ends_with("check-touched.sh") => CommandFamily::CheckTouched {
            tier: check_touched_tier(args),
        },
        _ => CommandFamily::Unknown,
    }
}

fn classify_nested_shell_family(payload: &str, depth: usize) -> CommandFamily {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return CommandFamily::Unknown;
    }
    let tokens = tokenize_shell_subject_words(payload);
    let segments = split_shell_command_segments(&tokens);
    if segments.len() != 1 {
        let families = segments
            .iter()
            .map(|segment| command_family_for_pipeline(segment))
            .collect::<Vec<_>>();
        if families.iter().all(CommandFamily::is_workspace_read_only) {
            return CommandFamily::ReadOnlyChain {
                step_count: families.len(),
            };
        }
        let steps = families
            .iter()
            .filter_map(cargo_validation_step)
            .collect::<Vec<_>>();
        if !steps.is_empty()
            && families
                .iter()
                .all(|family| cargo_validation_step(family).is_some() || family.is_shell_noop())
        {
            return CommandFamily::CargoValidationChain { steps };
        }
        return CommandFamily::Unknown;
    }
    segments
        .first()
        .map(|segment| command_family_for_pipeline(segment))
        .unwrap_or(CommandFamily::Unknown)
}

fn static_wrapper_inner<'a>(command: &str, args: &'a [String]) -> Option<&'a [String]> {
    let mut index = 0usize;
    match command {
        "command" => {
            if args
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "-v" | "-V"))
            {
                return None;
            }
            while args
                .get(index)
                .is_some_and(|arg| matches!(arg.as_str(), "-p" | "--"))
            {
                index += 1;
            }
            if args.get(index).is_some_and(|arg| arg.starts_with('-')) {
                return None;
            }
        }
        "builtin" => {
            if args.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            }
        }
        "env" => {
            while let Some(arg) = args.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                }
                if matches!(arg.as_str(), "-i" | "--ignore-environment") || is_shell_assignment(arg)
                {
                    index += 1;
                    continue;
                }
                if matches!(arg.as_str(), "-u" | "--unset") {
                    index = index.saturating_add(2);
                    continue;
                }
                if arg.starts_with("--unset=") {
                    index += 1;
                    continue;
                }
                if arg.starts_with('-') {
                    return None;
                }
                break;
            }
        }
        "timeout" => {
            while let Some(arg) = args.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                }
                if matches!(
                    arg.as_str(),
                    "--foreground" | "--preserve-status" | "--verbose"
                ) {
                    index += 1;
                    continue;
                }
                if matches!(arg.as_str(), "-k" | "--kill-after" | "-s" | "--signal") {
                    index = index.saturating_add(2);
                    continue;
                }
                if arg.starts_with("--kill-after=") || arg.starts_with("--signal=") {
                    index += 1;
                    continue;
                }
                if arg.starts_with('-') {
                    return None;
                }
                // timeout duration
                index += 1;
                break;
            }
        }
        "time" => {
            while args.get(index).is_some_and(|arg| arg.starts_with('-')) {
                if matches!(args[index].as_str(), "-o" | "--output" | "-f" | "--format") {
                    index = index.saturating_add(2);
                } else {
                    index += 1;
                }
            }
        }
        "nice" => {
            if args
                .get(index)
                .is_some_and(|arg| matches!(arg.as_str(), "-n" | "--adjustment"))
            {
                index = index.saturating_add(2);
            } else if args
                .get(index)
                .is_some_and(|arg| arg.starts_with("--adjustment=") || is_nice_adjustment(arg))
            {
                index += 1;
            }
        }
        "nohup" => {}
        "stdbuf" => {
            while let Some(arg) = args.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                }
                if matches!(arg.as_str(), "-i" | "-o" | "-e") {
                    index = index.saturating_add(2);
                } else if arg.starts_with("-i") || arg.starts_with("-o") || arg.starts_with("-e") {
                    index += 1;
                } else {
                    break;
                }
            }
        }
        _ => return None,
    }
    (index < args.len()).then_some(&args[index..])
}

fn is_nice_adjustment(value: &str) -> bool {
    value
        .strip_prefix('-')
        .unwrap_or(value)
        .chars()
        .all(|ch| ch.is_ascii_digit())
}

fn static_shell_payload(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find_map(|pair| matches!(pair[0].as_str(), "-c" | "-lc").then_some(pair[1].as_str()))
}

fn cargo_command_family(args: &[String]) -> CommandFamily {
    match args.first().map(String::as_str) {
        Some("check") => CommandFamily::CargoCheck,
        Some("test") => CommandFamily::CargoTest,
        Some("clippy") => CommandFamily::CargoClippy,
        Some("fmt") if args.iter().skip(1).any(|arg| arg == "--check") => {
            CommandFamily::CargoFmtCheck
        }
        _ => CommandFamily::Unknown,
    }
}

fn check_touched_tier(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--tier=").map(str::to_owned).or_else(|| {
            (arg == "--tier")
                .then(|| args.get(index + 1).cloned())
                .flatten()
        })
    })
}

fn shell_segment_is_read_filter(tokens: &[String]) -> bool {
    let words = tokens
        .iter()
        .filter(|token| !is_fd_duplication_token(token))
        .cloned()
        .collect::<Vec<_>>();
    let Some((command, _args)) = shell_segment_command_and_args(&words) else {
        return false;
    };
    if !shell_segment_redirections_are_readonly(&words) {
        return false;
    }
    match command {
        "head" | "tail" | "wc" | "cat" | "grep" | "rg" => true,
        "sort" => words.iter().skip(1).all(|word| {
            word.starts_with('-')
                && !matches!(word.as_str(), "-o" | "--output")
                && !word.starts_with("-o")
                && !word.starts_with("--output=")
        }),
        "uniq" => words.iter().skip(1).all(|word| word.starts_with('-')),
        _ => false,
    }
}

fn search_segment_is_read_only(command: &str, args: &[String]) -> bool {
    command != "rg" || !args.iter().any(|arg| rg_arg_may_execute_program(arg))
}

fn rg_arg_may_execute_program(arg: &str) -> bool {
    matches!(arg, "--pre" | "--search-zip" | "-z")
        || arg.starts_with("--pre=")
        || arg.starts_with("--pre-glob")
        || arg
            .strip_prefix('-')
            .filter(|short| !short.starts_with('-'))
            .is_some_and(|short| short.contains('z'))
}

fn is_fd_duplication_token(token: &str) -> bool {
    matches!(token, "2>&1" | "1>&2" | ">&2" | ">&1")
}

fn external_shell_path_subjects(workspace_root: &Path, command: &str) -> Result<Vec<ToolSubject>> {
    Ok(bash_path_subjects(workspace_root, command)?
        .into_iter()
        .filter(|subject| subject.scope == ToolSubjectScope::External)
        .collect())
}

#[cfg(test)]
pub(crate) fn shell_command_permission_operation(command: &str) -> ToolOperation {
    if shell_command_is_destructive(command) {
        ToolOperation::ExecuteDestructiveCommand
    } else if bash_command_is_safe_readonly(command) {
        ToolOperation::ExecuteReadOnlyCommand
    } else {
        ToolOperation::ExecuteUnknownCommand
    }
}

#[cfg(test)]
pub(crate) fn terminal_input_permission_operation(input: &str) -> ToolOperation {
    if shell_command_is_destructive(input) {
        ToolOperation::ExecuteDestructiveCommand
    } else {
        ToolOperation::SendTerminalInput
    }
}

pub(crate) fn shell_command_is_destructive(command: &str) -> bool {
    let tokens = tokenize_shell_subject_words(command);
    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if shell_segment_is_destructive(&segment) {
                return true;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    shell_segment_is_destructive(&segment)
}

fn shell_command_has_file_write(command: &str) -> bool {
    tokenize_shell_subject_words(command)
        .split(|token| matches!(token.as_str(), "&&" | "||" | ";" | "|"))
        .any(|segment| {
            shell_segment_has_overwrite_redirection(segment)
                || shell_segment_command_and_args(segment).is_some_and(|(program, args)| {
                    program == "find" && find_segment_has_write_action(args)
                })
        })
}

fn shell_command_has_file_delete(command: &str) -> bool {
    tokenize_shell_subject_words(command)
        .split(|token| matches!(token.as_str(), "&&" | "||" | ";" | "|"))
        .any(|segment| {
            shell_segment_command_and_args(segment).is_some_and(|(program, args)| {
                matches!(program, "rm" | "rmdir")
                    || program == "find" && find_segment_has_delete_action(args)
            })
        })
}

fn shell_semantic_dynamic_reason(command: &str) -> Option<&'static str> {
    if command.chars().any(|ch| {
        ch.is_whitespace() && !ch.is_ascii()
            || matches!(
                ch,
                '\u{ff1b}' | '\u{ff5c}' | '\u{ff06}' | '\u{2212}' | '\u{2010}' | '\u{2011}'
            )
    }) {
        return Some("the command contains ambiguous Unicode whitespace or operator characters");
    }
    for segment in tokenize_shell_subject_words(command)
        .split(|token| matches!(token.as_str(), "&&" | "||" | ";" | "|"))
    {
        let Some((program, args)) = shell_segment_command_and_args(segment) else {
            continue;
        };
        if leading_assignments_are_dangerous(segment)
            || program == "env"
                && args.iter().any(|arg| {
                    is_shell_assignment(arg) && shell_assignment_changes_execution_semantics(arg)
                })
        {
            return Some("the command injects a shell startup or function environment variable");
        }
        if program == "rg" && args.iter().any(|arg| rg_arg_may_execute_program(arg)) {
            return Some("ripgrep preprocessing or archive search may execute another program");
        }
        if program == "git" && args.iter().any(|arg| git_arg_may_execute_program(arg)) {
            return Some("Git pager, diff, textconv, or inline config may execute configured code");
        }
        if program == "find"
            && args
                .iter()
                .any(|arg| matches!(arg.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
            && !find_segment_is_safe_readonly(segment)
            && !find_segment_has_delete_action(args)
        {
            return Some("find contains an inner command that is not fully classified");
        }
        if matches!(program, "eval" | "xargs") {
            return Some("a shell wrapper or argument-driven executor contains dynamic code");
        }
        if program == "fish" {
            return Some("a nested shell uses an unsupported shell dialect");
        }
        if matches!(program, "sh" | "bash" | "zsh") {
            let Some(payload) = static_shell_payload(args) else {
                return Some("a nested shell invocation has no statically analyzable payload");
            };
            if payload.contains("$(")
                || payload.contains('`')
                || payload.contains("<(")
                || payload.contains(">(")
            {
                return Some("a nested shell payload contains dynamic expansion");
            }
            if let Some(reason) = shell_semantic_dynamic_reason(payload) {
                return Some(reason);
            }
        }
    }
    None
}

fn shell_wrapper_limit_exceeded(command: &str, depth: usize) -> bool {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return true;
    }
    let tokens = tokenize_shell_subject_words(command);
    for segment in split_shell_command_segments(&tokens) {
        for pipeline_segment in split_shell_pipeline(segment) {
            let Some((program, args)) = shell_segment_command_and_args(pipeline_segment) else {
                continue;
            };
            let nested_limit_exceeded = if let Some(inner) = static_wrapper_inner(program, args) {
                shell_wrapper_tokens_exceed_limit(inner, depth + 1)
            } else if matches!(program, "sudo" | "doas") {
                shell_wrapper_tokens_exceed_limit(args, depth + 1)
            } else if matches!(program, "sh" | "bash" | "zsh") {
                static_shell_payload(args)
                    .is_some_and(|payload| shell_wrapper_limit_exceeded(payload, depth + 1))
            } else {
                false
            };
            if nested_limit_exceeded {
                return true;
            }
        }
    }
    false
}

fn shell_wrapper_tokens_exceed_limit(tokens: &[String], depth: usize) -> bool {
    if depth > MAX_SHELL_WRAPPER_DEPTH {
        return true;
    }
    let Some((program, args)) = shell_segment_command_and_args(tokens) else {
        return false;
    };
    static_wrapper_inner(program, args)
        .is_some_and(|inner| shell_wrapper_tokens_exceed_limit(inner, depth + 1))
        || matches!(program, "sudo" | "doas") && shell_wrapper_tokens_exceed_limit(args, depth + 1)
}

pub(crate) fn shell_segment_is_destructive(words: &[String]) -> bool {
    let Some((command, args)) = shell_segment_command_and_args(words) else {
        return false;
    };

    if matches!(command, "sudo" | "doas" | "env" | "command") && !args.is_empty() {
        return shell_segment_is_destructive(args);
    }

    if shell_segment_has_overwrite_redirection(words) {
        return true;
    }

    match command {
        "rm" => true,
        "rmdir" => true,
        "truncate" => true,
        "dd" => args.iter().any(|word| word.starts_with("of=")),
        "find" => find_segment_is_destructive(args),
        "git" => git_segment_is_destructive(args),
        "sh" | "bash" | "zsh" | "fish" => shell_invocation_is_destructive(args),
        _ => false,
    }
}

pub(crate) fn shell_segment_command_and_args(words: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0usize;
    while let Some(word) = words.get(index) {
        if is_shell_assignment(word) {
            index += 1;
            continue;
        }
        return Some((shell_command_basename(word), &words[index + 1..]));
    }
    None
}

pub(crate) fn is_shell_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
}

pub(crate) fn shell_command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

pub(crate) fn shell_segment_has_overwrite_redirection(words: &[String]) -> bool {
    let mut index = 0usize;
    while index < words.len() {
        let word = &words[index];
        if overwrite_redirection_target(word) {
            return true;
        }
        if is_overwrite_redirection_operator(word) {
            if overwrite_redirection_operator_target_is_destructive(
                words.get(index + 1).map(String::as_str),
            ) {
                return true;
            }
            index += 1;
        }
        index += 1;
    }
    false
}

pub(crate) fn is_overwrite_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | ">|" | "1>" | "1>>" | "1>|" | "2>" | "2>>" | "2>|" | "&>" | "&>>"
    )
}

pub(crate) fn overwrite_redirection_target(word: &str) -> bool {
    [
        "1>>", "1>|", "1>", "2>>", "2>|", "2>", "&>>", "&>", ">>", ">|", ">",
    ]
    .iter()
    .any(|prefix| {
        word.strip_prefix(prefix).is_some_and(|target| {
            !target.is_empty()
                && !target.starts_with('&')
                && !shell_requested_path_is_safe_device(target)
        })
    })
}

fn overwrite_redirection_operator_target_is_destructive(target: Option<&str>) -> bool {
    target.is_none_or(|target| {
        !target.starts_with('&') && !shell_requested_path_is_safe_device(target)
    })
}

pub(crate) fn find_segment_is_destructive(words: &[String]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        word == "-delete"
            || matches!(word.as_str(), "-exec" | "-execdir")
                && words
                    .get(index + 1)
                    .map(|command| shell_command_basename(command) == "rm")
                    .unwrap_or(false)
    })
}

fn find_segment_has_delete_action(words: &[String]) -> bool {
    find_segment_is_destructive(words)
}

fn find_segment_has_write_action(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| matches!(word.as_str(), "-fprint" | "-fprintf" | "-fls"))
}

pub(crate) fn git_segment_is_destructive(words: &[String]) -> bool {
    let Some((subcommand, subcommand_args)) = git_subcommand_and_args(words) else {
        return false;
    };
    match subcommand {
        "clean" => true,
        "reset" => subcommand_args.iter().any(|word| word == "--hard"),
        "checkout" | "restore" => subcommand_args
            .iter()
            .any(|word| word == "-f" || word == "--force"),
        _ => false,
    }
}

pub(crate) fn shell_invocation_is_destructive(words: &[String]) -> bool {
    words.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-c" | "-lc") && shell_command_is_destructive(&pair[1])
    })
}

#[cfg(test)]
pub(crate) fn bash_command_is_ast_known_readonly(command: &str) -> bool {
    let trimmed = command.trim();
    !trimmed.is_empty()
        && bash_ast_has_supported_readonly_structure(trimmed)
        && bash_command_is_safe_readonly(trimmed)
}

#[cfg(test)]
fn bash_ast_has_supported_readonly_structure(command: &str) -> bool {
    let mut parser = Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(command, None) else {
        return false;
    };
    let root = tree.root_node();
    if root.has_error() {
        return false;
    }
    let mut saw_readonly_structure = false;
    bash_ast_node_is_supported_readonly_candidate(root, &mut saw_readonly_structure)
        && saw_readonly_structure
}

#[cfg(test)]
fn bash_ast_node_is_supported_readonly_candidate(
    node: Node<'_>,
    saw_readonly_structure: &mut bool,
) -> bool {
    let kind = node.kind();
    if bash_ast_node_kind_is_unsupported_for_readonly(kind) {
        return false;
    }
    if matches!(
        kind,
        "pipeline" | "list" | "redirected_statement" | "binary_expression"
    ) {
        *saw_readonly_structure = true;
    }

    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .all(|child| bash_ast_node_is_supported_readonly_candidate(child, saw_readonly_structure))
}

#[cfg(test)]
fn bash_ast_node_kind_is_unsupported_for_readonly(kind: &str) -> bool {
    matches!(
        kind,
        "if_statement"
            | "for_statement"
            | "while_statement"
            | "case_statement"
            | "function_definition"
            | "subshell"
            | "command_substitution"
            | "process_substitution"
            | "heredoc_redirect"
            | "heredoc_body"
            | "variable_assignment"
            | "expansion"
            | "arithmetic_expansion"
    )
}

pub(crate) fn bash_command_is_safe_readonly(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    let tokens = tokenize_shell_subject_words(trimmed);
    if tokens.is_empty() {
        return false;
    }

    if bounded_file_presence_command_is_safe_readonly(&tokens) {
        return true;
    }

    if tokens_contain_unsupported_readonly_expansion(&tokens) {
        return false;
    }

    let mut segment_count = 0usize;
    let mut segment = Vec::new();
    for token in &tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if !segment.is_empty() {
                segment_count = segment_count.saturating_add(1);
            }
            segment.clear();
        } else {
            segment.push(token.clone());
        }
    }
    if !segment.is_empty() {
        segment_count = segment_count.saturating_add(1);
    }
    let allow_noop_segments = segment_count > 1;

    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if !bash_segment_is_safe_readonly_with_context(&segment, allow_noop_segments) {
                return false;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    bash_segment_is_safe_readonly_with_context(&segment, allow_noop_segments)
}

fn bash_command_is_safe_readonly_in_workspace(
    workspace_root: &Path,
    command: &str,
) -> Result<bool> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let tokens = strip_workspace_cd_prefix(
        &workspace_root,
        tokenize_shell_subject_words(command.trim()),
    )?;
    Ok(bash_command_is_safe_readonly(&tokens.join(" ")))
}

#[cfg(test)]
pub(crate) fn contains_unsupported_safe_shell_syntax(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '>' | '<' | '$' | '`' | '(' | ')' | '*' | '?' | '[' | ']'
        )
    })
}

pub(crate) fn bash_segment_is_safe_readonly(words: &[String]) -> bool {
    bash_segment_is_safe_readonly_with_depth(words, 0)
}

fn bash_segment_is_safe_readonly_with_depth(words: &[String], depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let pipeline = split_shell_pipeline(words);
    if pipeline.len() > 1 {
        let Some((primary, filters)) = pipeline.split_first() else {
            return false;
        };
        return bash_simple_segment_is_safe_readonly_with_depth(primary, depth)
            && filters
                .iter()
                .all(|filter| shell_segment_is_read_filter(filter));
    }
    bash_simple_segment_is_safe_readonly_with_depth(words, depth)
}

fn bash_segment_is_safe_readonly_with_context(words: &[String], allow_noop: bool) -> bool {
    bash_segment_is_safe_readonly(words) || allow_noop && shell_segment_is_safe_readonly_noop(words)
}

fn bash_simple_segment_is_safe_readonly_with_depth(words: &[String], depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let Some((command, args)) = shell_segment_command_and_args(words) else {
        return false;
    };

    if !shell_segment_redirections_are_readonly(words) {
        return false;
    }

    if known_tool_help_or_version_query(words) {
        return true;
    }

    if let Some(inner) = static_wrapper_inner(command, args) {
        return bash_simple_segment_is_safe_readonly_with_depth(inner, depth + 1);
    }
    if matches!(command, "sh" | "bash" | "zsh") {
        return static_shell_payload(args).is_some_and(bash_command_is_safe_readonly);
    }

    match command {
        "pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "du" | "file" | "readlink"
        | "realpath" | "basename" | "dirname" | "diff" | "cmp" | "grep" | "which" | "uname"
        | "date" | "whoami" | "id" => true,
        "rg" => search_segment_is_read_only(command, args),
        "command" => matches!(words.get(1).map(String::as_str), Some("-v")) && words.len() >= 3,
        "find" => find_segment_is_safe_readonly_with_depth(words, depth + 1),
        "git" => git_segment_is_safe_readonly(words),
        _ => false,
    }
}

fn shell_segment_is_safe_readonly_noop(words: &[String]) -> bool {
    let Some((command, _args)) = shell_segment_command_and_args(words) else {
        return false;
    };
    if !shell_segment_redirections_are_readonly(words) {
        return false;
    }
    matches!(command, "echo" | "printf" | "true" | ":")
}

fn shell_segment_redirections_are_readonly(words: &[String]) -> bool {
    let mut index = 0usize;
    while index < words.len() {
        let word = &words[index];
        if is_fd_duplication_token(word) {
            index += 1;
            continue;
        }
        if let Some(target) = output_redirection_target(word) {
            if !shell_requested_path_is_safe_device(target) {
                return false;
            }
            index += 1;
            continue;
        }
        if is_output_redirection_operator(word) {
            let Some(target) = words.get(index + 1).map(String::as_str) else {
                return false;
            };
            if !target.starts_with('&') && !shell_requested_path_is_safe_device(target) {
                return false;
            }
            index += 2;
            continue;
        }
        if matches!(word.as_str(), "<<" | "<<-") {
            return false;
        }
        if let Some(target) = input_redirection_target(word)
            && target.starts_with('(')
        {
            return false;
        }
        index += 1;
    }
    true
}

fn output_redirection_target(word: &str) -> Option<&str> {
    [
        ">>", ">|", ">", "1>>", "1>|", "1>", "2>>", "2>|", "2>", "&>>", "&>",
    ]
    .iter()
    .find_map(|prefix| {
        word.strip_prefix(prefix)
            .filter(|target| !target.is_empty() && !target.starts_with('&'))
    })
}

fn input_redirection_target(word: &str) -> Option<&str> {
    word.strip_prefix('<')
        .filter(|target| !target.is_empty() && !target.starts_with('<'))
}

fn is_output_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | ">|" | "1>" | "1>>" | "1>|" | "2>" | "2>>" | "2>|" | "&>" | "&>>"
    )
}

fn tokens_contain_unsupported_readonly_expansion(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        token.contains('$')
            || token.contains('`')
            || token.contains('*')
            || token.contains('?')
            || token.contains('(')
            || token.contains(')')
            || token.contains('[')
            || token.contains(']')
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilePresenceLoopPathKind {
    ListedPath,
    WorkspaceGitMetadata,
}

#[derive(Debug)]
struct BoundedFilePresenceLoop<'a> {
    loop_start: usize,
    prefix: &'a [String],
    variable: &'a str,
    items: &'a [String],
    path_kind: FilePresenceLoopPathKind,
}

#[derive(Debug)]
struct ParsedFilePresenceLoop<'a> {
    variable: &'a str,
    items: &'a [String],
    path_kind: FilePresenceLoopPathKind,
}

fn bounded_file_presence_command_is_safe_readonly(tokens: &[String]) -> bool {
    parse_bounded_file_presence_command(tokens).is_some()
}

fn bounded_file_presence_command_uses_git(command: &str) -> bool {
    let tokens = tokenize_shell_subject_words(command);
    parse_bounded_file_presence_command(&tokens).is_some_and(|loop_spec| {
        split_shell_command_segments(loop_spec.prefix)
            .iter()
            .any(|segment| segment.first().is_some_and(|program| program == "git"))
    })
}

fn parse_bounded_file_presence_command(tokens: &[String]) -> Option<BoundedFilePresenceLoop<'_>> {
    if tokens.iter().any(|token| {
        token
            .chars()
            .any(|character| matches!(character, '&' | '<' | '>'))
    }) {
        return None;
    }
    let mut parsed = None;
    for (loop_start, token) in tokens.iter().enumerate() {
        if token != "for"
            || loop_start > 0 && tokens.get(loop_start - 1).map(String::as_str) != Some(";")
        {
            continue;
        }
        let Some(loop_spec) = parse_for_in_file_test_echo_loop(&tokens[loop_start..]) else {
            continue;
        };
        if loop_start > 0 {
            let prefix = &tokens[..loop_start - 1];
            if prefix.is_empty() || !readonly_shell_prefix_is_safe(prefix) {
                continue;
            }
        }
        if parsed.is_some() {
            return None;
        }
        parsed = Some(BoundedFilePresenceLoop {
            loop_start,
            prefix: if loop_start == 0 {
                &tokens[..0]
            } else {
                &tokens[..loop_start - 1]
            },
            variable: loop_spec.variable,
            items: loop_spec.items,
            path_kind: loop_spec.path_kind,
        });
    }
    parsed
}

fn readonly_shell_prefix_is_safe(tokens: &[String]) -> bool {
    if tokens_contain_unsupported_readonly_expansion(tokens) {
        return false;
    }
    let segments = split_shell_command_segments(tokens);
    !segments.is_empty()
        && segments
            .iter()
            .all(|segment| bounded_file_presence_prefix_segment_is_safe(segment))
}

fn bounded_file_presence_prefix_segment_is_safe(tokens: &[String]) -> bool {
    match tokens {
        [command, arguments @ ..] if command == "echo" => !arguments.is_empty(),
        [git, log, oneline, count] if git == "git" && log == "log" && oneline == "--oneline" => {
            count.strip_prefix('-').is_some_and(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            })
        }
        [git, log, oneline, count_flag, count]
            if git == "git" && log == "log" && oneline == "--oneline" && count_flag == "-n" =>
        {
            !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit())
        }
        [git, branch, show_current]
            if git == "git" && branch == "branch" && show_current == "--show-current" =>
        {
            true
        }
        [git, rev_parse, revision]
            if git == "git" && rev_parse == "rev-parse" && revision == "HEAD" =>
        {
            true
        }
        [git, stash, list] if git == "git" && stash == "stash" && list == "list" => true,
        _ => false,
    }
}

fn parse_for_in_file_test_echo_loop(tokens: &[String]) -> Option<ParsedFilePresenceLoop<'_>> {
    if tokens.len() < 16
        || tokens.first().map(String::as_str) != Some("for")
        || tokens.get(2).map(String::as_str) != Some("in")
    {
        return None;
    }
    let variable = tokens.get(1)?.as_str();
    if !is_shell_identifier(variable) {
        return None;
    }
    let mut cursor = 3usize;
    while tokens.get(cursor).is_some_and(|token| token != ";") {
        cursor += 1;
    }
    let items = tokens.get(3..cursor)?;
    if items.is_empty()
        || !items
            .iter()
            .all(|item| file_presence_loop_item_is_static(item))
    {
        return None;
    }
    for expected in [";", "do", "if", "["] {
        if tokens.get(cursor).map(String::as_str) != Some(expected) {
            return None;
        }
        cursor += 1;
    }
    let operator = tokens.get(cursor)?.as_str();
    cursor += 1;
    let tested_path = tokens.get(cursor)?.as_str();
    cursor += 1;
    for expected in ["]", ";", "then"] {
        if tokens.get(cursor).map(String::as_str) != Some(expected) {
            return None;
        }
        cursor += 1;
    }
    cursor = parse_bounded_echo_clause(tokens, cursor, variable)?;
    if tokens.get(cursor).map(String::as_str) != Some(";")
        || tokens.get(cursor + 1).map(String::as_str) != Some("else")
    {
        return None;
    }
    cursor = parse_bounded_echo_clause(tokens, cursor + 2, variable)?;
    if tokens.get(cursor).map(String::as_str) != Some(";")
        || tokens.get(cursor + 1).map(String::as_str) != Some("fi")
        || tokens.get(cursor + 2).map(String::as_str) != Some(";")
        || tokens.get(cursor + 3).map(String::as_str) != Some("done")
        || cursor + 4 != tokens.len()
    {
        return None;
    }
    if !tokens.iter().all(|token| {
        token_has_no_dynamic_shell_syntax(token)
            && token_only_references_loop_variable(token, variable)
    }) {
        return None;
    }

    let variable_ref = format!("${variable}");
    let git_metadata_ref = format!(".git/${variable}");
    let path_kind = match (operator, tested_path) {
        ("-f", path) if path == variable_ref && items.iter().any(|item| item.contains('/')) => {
            FilePresenceLoopPathKind::ListedPath
        }
        ("-e", path)
            if path == git_metadata_ref
                && items.iter().all(|item| bounded_git_metadata_name(item)) =>
        {
            FilePresenceLoopPathKind::WorkspaceGitMetadata
        }
        _ => return None,
    };
    Some(ParsedFilePresenceLoop {
        variable,
        items,
        path_kind,
    })
}

fn parse_static_git_metadata_presence_loop_header(tokens: &[String]) -> Option<&[String]> {
    if tokens.len() < 10
        || tokens.first().map(String::as_str) != Some("for")
        || tokens.get(2).map(String::as_str) != Some("in")
    {
        return None;
    }
    let variable = tokens.get(1)?.as_str();
    if !is_shell_identifier(variable) {
        return None;
    }
    let mut cursor = 3usize;
    while tokens.get(cursor).is_some_and(|token| token != ";") {
        cursor += 1;
    }
    let items = tokens.get(3..cursor)?;
    if items.is_empty()
        || !items
            .iter()
            .all(|item| file_presence_loop_item_is_static(item) && bounded_git_metadata_name(item))
    {
        return None;
    }
    let git_metadata_ref = format!(".git/${variable}");
    for expected in [";", "do", "if", "[", "-e", git_metadata_ref.as_str(), "]"] {
        if tokens.get(cursor).map(String::as_str) != Some(expected) {
            return None;
        }
        cursor += 1;
    }
    Some(items)
}

fn parse_bounded_echo_clause(
    tokens: &[String],
    mut cursor: usize,
    variable: &str,
) -> Option<usize> {
    if tokens.get(cursor).map(String::as_str) != Some("echo") {
        return None;
    }
    cursor += 1;
    let argument_start = cursor;
    while tokens.get(cursor).is_some_and(|token| token != ";") {
        let token = &tokens[cursor];
        if matches!(token.as_str(), "&&" | "||" | "|" | "&")
            || is_redirection_operator(token)
            || redirection_target(token).is_some()
            || !token_has_no_dynamic_shell_syntax(token)
            || !token_only_references_loop_variable(token, variable)
        {
            return None;
        }
        cursor += 1;
    }
    (cursor > argument_start).then_some(cursor)
}

fn file_presence_loop_item_is_static(item: &str) -> bool {
    !item.is_empty()
        && !item.starts_with('-')
        && !item.starts_with('~')
        && !item.contains('$')
        && token_has_no_dynamic_shell_syntax(item)
        && !matches!(item, "." | "..")
}

fn bounded_git_metadata_name(item: &str) -> bool {
    !item.is_empty()
        && !matches!(item, "." | "..")
        && item
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn token_has_no_dynamic_shell_syntax(token: &str) -> bool {
    !token.contains('`')
        && !token.contains('*')
        && !token.contains('?')
        && !token.contains('(')
        && !token.contains(')')
        && !token.contains('{')
        && !token.contains('}')
        && (matches!(token, "[" | "]") || !token.contains('[') && !token.contains(']'))
}

fn token_only_references_loop_variable(token: &str, variable: &str) -> bool {
    let needle = format!("${variable}");
    let mut rest = token;
    while let Some(index) = rest.find('$') {
        if !rest[index..].starts_with(&needle) {
            return false;
        }
        rest = &rest[index + needle.len()..];
        if rest
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        {
            return false;
        }
    }
    true
}

fn is_shell_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && value
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(crate) fn is_help_or_version_query(words: &[String]) -> bool {
    words.len() == 2
        && matches!(
            words[1].as_str(),
            "--version" | "-V" | "--help" | "-h" | "help"
        )
}

fn known_tool_help_or_version_query(words: &[String]) -> bool {
    let Some(program) = words.first().map(|word| shell_command_basename(word)) else {
        return false;
    };
    matches!(
        program,
        "cargo"
            | "rustc"
            | "rustfmt"
            | "clippy-driver"
            | "git"
            | "rg"
            | "grep"
            | "find"
            | "node"
            | "npm"
            | "pnpm"
            | "yarn"
            | "bun"
            | "python"
            | "python3"
            | "go"
            | "java"
            | "javac"
    ) && is_help_or_version_query(words)
}

pub(crate) fn find_segment_is_safe_readonly(words: &[String]) -> bool {
    find_segment_is_safe_readonly_with_depth(words, 0)
}

fn find_segment_is_safe_readonly_with_depth(words: &[String], depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let mut index = 1usize;
    while index < words.len() {
        match words[index].as_str() {
            "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprintf" | "-fls" => return false,
            "-exec" | "-execdir" => {
                let start = index + 1;
                let Some(relative_end) = words[start..]
                    .iter()
                    .position(|word| matches!(word.as_str(), ";" | "\\;" | "+"))
                else {
                    return false;
                };
                let end = start + relative_end;
                if !bash_simple_segment_is_safe_readonly_with_depth(&words[start..end], depth + 1) {
                    return false;
                }
                index = end + 1;
            }
            _ => index += 1,
        }
    }
    true
}

pub(crate) fn git_segment_is_safe_readonly(words: &[String]) -> bool {
    if words
        .iter()
        .skip(1)
        .any(|word| git_arg_may_execute_program(word))
    {
        return false;
    }
    let Some((_, args)) = shell_segment_command_and_args(words) else {
        return false;
    };
    let Some((subcommand, subcommand_args)) = git_subcommand_and_args(args) else {
        return false;
    };
    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "rev-parse" | "ls-files" | "grep" => true,
        "stash" => matches!(subcommand_args, [operation] if operation == "list"),
        "branch" => args
            .iter()
            .skip_while(|arg| arg.as_str() != "branch")
            .skip(1)
            .all(|word| matches!(word.as_str(), "--show-current" | "--list")),
        _ => false,
    }
}

fn git_arg_may_execute_program(arg: &str) -> bool {
    matches!(
        arg,
        "--ext-diff" | "--textconv" | "--paginate" | "-p" | "-c" | "--config-env"
    ) || arg.starts_with("-c") && arg.len() > 2
        || arg.starts_with("--config-env=")
        || arg.starts_with("--pager=")
        || arg.starts_with("--exec-path=")
        || arg.starts_with("--upload-pack=")
        || arg.starts_with("--receive-pack=")
}

pub(crate) fn bash_path_subjects(workspace_root: &Path, command: &str) -> Result<Vec<ToolSubject>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    bash_path_subjects_from_cwd(&workspace_root, &workspace_root, command)
}

pub(crate) fn bash_path_subjects_from_cwd(
    workspace_root: &Path,
    initial_cwd: &Path,
    command: &str,
) -> Result<Vec<ToolSubject>> {
    let tokens = tokenize_shell_subject_words(command);
    let mut subjects = Vec::new();
    let mut cwd = initial_cwd.to_path_buf();
    if let Some(loop_spec) = parse_bounded_file_presence_command(&tokens) {
        collect_bash_path_subjects_from_tokens(
            workspace_root,
            &mut cwd,
            &tokens[..loop_spec.loop_start],
            &mut subjects,
        )?;
        for item in loop_spec.items {
            let requested = match loop_spec.path_kind {
                FilePresenceLoopPathKind::ListedPath => item.clone(),
                FilePresenceLoopPathKind::WorkspaceGitMetadata => format!(".git/{item}"),
            };
            push_shell_path_subject(&mut subjects, workspace_root, &cwd, &requested)?;
        }
        return Ok(subjects);
    }
    collect_bash_path_subjects_from_tokens(workspace_root, &mut cwd, &tokens, &mut subjects)?;
    for (loop_start, token) in tokens.iter().enumerate() {
        if token != "for"
            || loop_start > 0 && tokens.get(loop_start - 1).map(String::as_str) != Some(";")
        {
            continue;
        }
        let Some(items) = parse_static_git_metadata_presence_loop_header(&tokens[loop_start..])
        else {
            continue;
        };
        let mut loop_cwd = initial_cwd.to_path_buf();
        let mut ignored_subjects = Vec::new();
        collect_bash_path_subjects_from_tokens(
            workspace_root,
            &mut loop_cwd,
            &tokens[..loop_start],
            &mut ignored_subjects,
        )?;
        for item in items {
            push_shell_path_subject(
                &mut subjects,
                workspace_root,
                &loop_cwd,
                &format!(".git/{item}"),
            )?;
        }
    }
    Ok(subjects)
}

fn collect_bash_path_subjects_from_tokens(
    workspace_root: &Path,
    cwd: &mut PathBuf,
    tokens: &[String],
    subjects: &mut Vec<ToolSubject>,
) -> Result<()> {
    let mut segment_words = Vec::new();
    for token in tokens {
        if token == "&&" || token == "||" || token == ";" {
            collect_bash_segment_subjects(workspace_root, cwd, &segment_words, subjects)?;
            segment_words.clear();
        } else {
            segment_words.push(token.clone());
        }
    }
    collect_bash_segment_subjects(workspace_root, cwd, &segment_words, subjects)
}

pub(crate) fn collect_bash_segment_subjects(
    workspace_root: &Path,
    cwd: &mut PathBuf,
    words: &[String],
    subjects: &mut Vec<ToolSubject>,
) -> Result<()> {
    if words.is_empty() {
        return Ok(());
    }
    if words.iter().any(|word| word == "|") {
        for pipeline_segment in split_shell_pipeline(words) {
            collect_bash_segment_subjects(workspace_root, cwd, pipeline_segment, subjects)?;
        }
        return Ok(());
    }

    let command = words[0].as_str();
    let mut index = 1usize;
    if command == "cd" {
        if let Some(target) = words.get(1).filter(|word| !word.starts_with('-')) {
            let resolved = resolve_tool_path_from_base(workspace_root, cwd, target)?;
            subjects.push(resolved_tool_path_subject(resolved.clone()));
            *cwd = resolved.canonical;
        }
        return Ok(());
    }

    while index < words.len() {
        let word = &words[index];
        if let Some(target) = redirection_target(word) {
            push_shell_path_subject(subjects, workspace_root, cwd, target)?;
        } else if command == "dd" && word.starts_with("of=") && word.len() > 3 {
            push_shell_path_subject(subjects, workspace_root, cwd, &word[3..])?;
        } else if command == "git"
            && let Some(target) = word
                .strip_prefix("--git-dir=")
                .or_else(|| word.strip_prefix("--work-tree="))
        {
            push_shell_path_subject(subjects, workspace_root, cwd, target)?;
        } else if (command == "git" && matches!(word.as_str(), "-C" | "--git-dir" | "--work-tree"))
            || is_redirection_operator(word)
        {
            if let Some(target) = words.get(index + 1) {
                push_shell_path_subject(subjects, workspace_root, cwd, target)?;
                index += 1;
            }
        } else if is_path_argument(command, word) && !find_pattern_argument(words, command, index) {
            push_shell_path_subject(subjects, workspace_root, cwd, word)?;
        }
        index += 1;
    }
    Ok(())
}

fn find_pattern_argument(words: &[String], command: &str, index: usize) -> bool {
    command == "find"
        && index.checked_sub(1).is_some_and(|previous| {
            matches!(
                words[previous].as_str(),
                "-name" | "-iname" | "-path" | "-ipath" | "-regex" | "-iregex"
            )
        })
}

fn push_shell_path_subject(
    subjects: &mut Vec<ToolSubject>,
    workspace_root: &Path,
    cwd: &Path,
    requested: &str,
) -> Result<()> {
    if shell_requested_path_is_safe_device(requested) {
        return Ok(());
    }
    if let Some(relative) = requested
        .strip_prefix("$PWD/")
        .or_else(|| requested.strip_prefix("${PWD}/"))
    {
        push_resolved_or_unknown_shell_path_subject(
            subjects,
            workspace_root,
            cwd,
            cwd.join(relative).to_string_lossy().as_ref(),
            requested,
        );
        return Ok(());
    }
    if matches!(requested, "$PWD" | "${PWD}") {
        push_resolved_or_unknown_shell_path_subject(
            subjects,
            workspace_root,
            cwd,
            cwd.to_string_lossy().as_ref(),
            requested,
        );
        return Ok(());
    }
    push_resolved_or_unknown_shell_path_subject(
        subjects,
        workspace_root,
        cwd,
        requested,
        requested,
    );
    Ok(())
}

fn push_resolved_or_unknown_shell_path_subject(
    subjects: &mut Vec<ToolSubject>,
    workspace_root: &Path,
    cwd: &Path,
    resolution_target: &str,
    original: &str,
) {
    match shell_path_subject(workspace_root, cwd, resolution_target) {
        Ok(mut subject) => {
            subject.original = original.to_owned();
            subjects.push(subject);
        }
        Err(_) => subjects.push(ToolSubject::path_with_scope(
            original.to_owned(),
            format!(
                "unresolved_shell_path:sha256:{}",
                sha256_hex(original.as_bytes())
            ),
            None,
            ToolSubjectScope::Unknown,
        )),
    }
}

/// A shell invocation can read one path and overwrite another path in the same command. Keep the
/// enclosing `ToolAccess::Execute` for the command subject, but annotate concrete path subjects so
/// policy can evaluate each resource independently.
fn annotate_shell_subject_access(subjects: &mut [ToolSubject], command: &str) {
    let tokens = tokenize_shell_subject_words(command);
    let mut mark = |target: &str| {
        if let Some(subject) = subjects.iter_mut().find(|subject| {
            subject.kind == sigil_kernel::ToolSubjectKind::Path && subject.original == target
        }) {
            subject.access = ToolAccess::Write;
        }
    };
    for (index, token) in tokens.iter().enumerate() {
        if overwrite_redirection_target(token) {
            let target = [
                "1>>", "1>|", "1>", "2>>", "2>|", "2>", "&>>", "&>", ">>", ">|", ">",
            ]
            .iter()
            .find_map(|prefix| token.strip_prefix(prefix))
            .unwrap_or_default();
            mark(target);
        } else if is_overwrite_redirection_operator(token) {
            if let Some(target) = tokens.get(index + 1) {
                mark(target);
            }
        } else if token.starts_with("of=") && token.len() > 3 {
            mark(&token[3..]);
        }
    }
}

fn shell_requested_path_is_safe_device(requested: &str) -> bool {
    matches!(requested, "/dev/null" | "/dev/stdout" | "/dev/stderr")
}

pub(crate) fn shell_path_subject(
    workspace_root: &Path,
    cwd: &Path,
    requested: &str,
) -> Result<ToolSubject> {
    resolve_tool_path_from_base(workspace_root, cwd, requested).map(resolved_tool_path_subject)
}

pub(crate) fn resolved_tool_path_subject(resolved: ResolvedToolPath) -> ToolSubject {
    ToolSubject::path_with_scope(
        resolved.original,
        resolved.normalized,
        Some(resolved.canonical),
        resolved.scope,
    )
}

pub(crate) fn tokenize_shell_subject_words(command: &str) -> Vec<String> {
    let command = strip_shell_heredoc_bodies(command);
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None::<char>;
    while let Some(ch) = chars.next() {
        if let Some(active_quote) = quote {
            match active_quote {
                '\'' => {
                    if ch == '\'' {
                        quote = None;
                    } else {
                        current.push(ch);
                    }
                }
                '"' => {
                    if ch == '"' {
                        quote = None;
                    } else if ch == '\\' {
                        match chars.peek().copied() {
                            Some('$' | '`' | '"' | '\\') => {
                                if let Some(escaped) = chars.next() {
                                    current.push(escaped);
                                }
                            }
                            Some('\n') => {
                                chars.next();
                            }
                            _ => current.push('\\'),
                        }
                    } else {
                        current.push(ch);
                    }
                }
                _ => unreachable!("POSIX tokenizer only records single or double quotes"),
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    if next == '\n' {
                        continue;
                    } else if next == ';' {
                        current.push_str("\\;");
                    } else {
                        current.push(next);
                    }
                } else {
                    current.push('\\');
                }
            }
            ' ' | '\t' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            '\n' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                if !words
                    .last()
                    .is_some_and(|word| matches!(word.as_str(), ";" | "&&" | "||" | "|"))
                {
                    words.push(";".to_owned());
                }
            }
            ';' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push(";".to_owned());
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("&&".to_owned());
            }
            '&' if chars.peek() == Some(&'>') => current.push(ch),
            '&' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("&".to_owned());
            }
            '|' if chars.peek() == Some(&'|') => {
                chars.next();
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("||".to_owned());
            }
            '|' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("|".to_owned());
            }
            '<' | '>' => {
                let mut operator = String::new();
                if !current.is_empty() {
                    if current == "&" || current.chars().all(|part| part.is_ascii_digit()) {
                        operator.push_str(&std::mem::take(&mut current));
                    } else {
                        words.push(std::mem::take(&mut current));
                    }
                }
                operator.push(ch);
                if chars.peek() == Some(&ch) {
                    chars.next();
                    operator.push(ch);
                }
                if ch == '>' && chars.peek() == Some(&'|') {
                    chars.next();
                    operator.push('|');
                }
                if chars.peek() == Some(&'&') {
                    chars.next();
                    operator.push('&');
                    while chars.peek().is_some_and(|next| next.is_ascii_digit()) {
                        if let Some(next) = chars.next() {
                            operator.push(next);
                        }
                    }
                }
                words.push(operator);
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Removes heredoc bodies before shell subject extraction. A heredoc body is data, not an
/// executable path subject; retaining it made an absolute path written inside an AWK/Python
/// program look like a root mutation. The redirection and destination remain analyzed normally.
fn strip_shell_heredoc_bodies(command: &str) -> String {
    let mut output = String::with_capacity(command.len());
    let mut lines = command.split_inclusive('\n');
    while let Some(line) = lines.next() {
        output.push_str(line);
        let Some(delimiter) = shell_heredoc_delimiter(line) else {
            continue;
        };
        for body_line in lines.by_ref() {
            if body_line.trim_end_matches(['\n', '\r']) == delimiter {
                break;
            }
        }
    }
    output
}

fn shell_heredoc_delimiter(line: &str) -> Option<String> {
    let marker = line.find("<<")?;
    if line.as_bytes().get(marker + 2) == Some(&b'<') {
        return None;
    }
    let mut rest = &line[marker + 2..];
    if rest.starts_with('-') {
        rest = &rest[1..];
    }
    let rest = rest.trim_start();
    let token = rest.split_whitespace().next()?.trim_end_matches(';');
    let token = token
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            token
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(token);
    (!token.is_empty()).then(|| token.to_owned())
}

fn shell_command_uses_controlled_scratch_heredoc(
    command: &str,
    path_policy: &ShellPathPolicyBinding,
) -> bool {
    path_policy.scratch_root.is_some()
        && command.contains("<<")
        && (command.contains("$SIGIL_SCRATCH_DIR") || command.contains("${SIGIL_SCRATCH_DIR}"))
}

pub(crate) fn is_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | ">|" | "<" | "<<" | "2>" | "2>>" | "2>|" | "&>" | "&>>" | "1>" | "1>>" | "1>|"
    )
}

pub(crate) fn redirection_target(word: &str) -> Option<&str> {
    for prefix in [
        ">>", ">|", ">", "<", "2>>", "2>|", "2>", "&>>", "&>", "1>>", "1>|", "1>",
    ] {
        if let Some(target) = word
            .strip_prefix(prefix)
            .filter(|target| !target.is_empty() && !target.starts_with('&'))
        {
            return Some(target);
        }
    }
    None
}

#[cfg(test)]
#[path = "tests/shell_tests.rs"]
mod tests;

pub(crate) fn is_path_argument(command: &str, word: &str) -> bool {
    if word.starts_with('-') || word.contains("://") {
        return false;
    }
    if word.starts_with('/')
        || word.starts_with("./")
        || word.starts_with("../")
        || word == "."
        || word == ".."
        || word.contains('/')
    {
        return true;
    }
    matches!(
        command,
        "cat"
            | "head"
            | "tail"
            | "wc"
            | "stat"
            | "du"
            | "file"
            | "readlink"
            | "realpath"
            | "basename"
            | "dirname"
            | "diff"
            | "cmp"
            | "ls"
            | "find"
            | "rm"
            | "rmdir"
            | "truncate"
            | "dd"
    )
}
