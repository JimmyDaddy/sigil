use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    DeclaredToolPermissionFacts, EnvironmentContainment, ExecutionCleanupStatus,
    ProcessContainment, TerminalTaskEntry, TerminalTaskId, TerminalTaskStatus, Tool, ToolAccess,
    ToolArtifactEncoding, ToolArtifactSensitivity, ToolCategory, ToolContext, ToolErrorKind,
    ToolExecutionId, ToolOperation, ToolPermissionEffect, ToolPermissionPlanDraft,
    ToolPermissionPlanV2, ToolPermissionSummary, ToolPreviewCapability, ToolProgressEvent,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind,
    declared_tool_permission_plan, safe_persistence_text,
};

use crate::{
    constants::{
        DEFAULT_TERMINAL_READ_LIMIT_BYTES, HARD_TERMINAL_READ_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV,
    },
    path::{
        absolute_path_from, canonical_workspace_root, resolve_tool_path_from_base,
        tool_path_subject,
    },
    scratch_namespace::{
        ScratchNamespaceControl, ScratchQuota, scratch_provision_error_result, session_scratch_key,
    },
    shell::{
        CommandFamily, ShellCommandAnalysis, ShellPathPolicyBinding,
        analyze_shell_command_with_path_policy, bash_path_subjects_from_cwd,
        command_permission_subject, shell_execution_environment, shell_grant_scope_detail,
    },
    shell_runtime::{ResolvedShell, ShellDialect},
    support::{optional_string, optional_usize, required_string},
    terminal_process::{
        MAX_TERMINAL_INPUT_BYTES, TerminalExecutionConfig, TerminalInputResult,
        TerminalProcessManager, TerminalPtySize, TerminalReadResult, TerminalReadinessCondition,
        TerminalResizeResult, TerminalStartRequest, TerminalTaskPermissionContext,
        TerminalTaskSnapshot, TerminalWaitCondition, TerminalWaitOutcome, TerminalWaitResult,
    },
};

mod capture;

pub(crate) const MAX_TERMINAL_READ_GUARDS: usize = 1_024;

pub(crate) struct TerminalStartTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
    pub(crate) scratch_label: String,
    pub(crate) scratch_quota: ScratchQuota,
    pub(crate) scratch: ScratchNamespaceControl,
}
pub(crate) struct TerminalReadTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
    pub(crate) scratch: ScratchNamespaceControl,
}
pub(crate) struct TerminalWaitTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
    pub(crate) scratch: ScratchNamespaceControl,
}
pub(crate) struct TerminalInputTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}
pub(crate) struct TerminalResizeTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}
pub(crate) struct TerminalCancelTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
    pub(crate) scratch: ScratchNamespaceControl,
}

/// Process-local typed control retained by product adapters for persistent terminal tasks.
///
/// The handle owns the exact manager set used by the registered terminal tools. It never exposes
/// physical artifact paths to renderer clients; adapters must bind cancellation to an already
/// authenticated workspace and terminal task identity.
const EXECUTION_CLASS_BINDING: &str = "managed";

#[derive(Clone)]
pub struct TerminalTaskControlHandle {
    managers: Arc<TerminalProcessManagers>,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
}

impl std::fmt::Debug for TerminalTaskControlHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalTaskControlHandle")
            .field("artifact_root", &"[bound]")
            .finish_non_exhaustive()
    }
}

impl TerminalTaskControlHandle {
    /// Stops admission and notifies all actual execution owners, including commands whose
    /// start call already yielded. This operation never waits for process or thread cleanup.
    pub fn request_stop_all(&self) {
        self.managers.stopping.store(true, Ordering::SeqCst);
        let managers = self
            .managers
            .managers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for manager in managers.values() {
            manager.request_stop_all();
        }
    }

    /// Drains the exact managers retained by this control owner and joins their execution work.
    ///
    /// # Errors
    /// Returns an error if any real execution owner could not confirm final cleanup.
    pub async fn shutdown_owned(&self) -> Result<()> {
        self.request_stop_all();
        let managers = self
            .managers
            .managers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut failures = 0usize;
        for manager in managers {
            if manager.shutdown_owned().await.is_err() {
                failures += 1;
            }
        }
        if failures != 0 {
            bail!("execution shutdown failed for {failures} managed owners");
        }
        Ok(())
    }

    pub(crate) fn new(
        managers: Arc<TerminalProcessManagers>,
        artifact_root: PathBuf,
        artifact_label_root: PathBuf,
    ) -> Self {
        Self {
            managers,
            artifact_root,
            artifact_label_root,
        }
    }

    #[cfg(test)]
    pub(crate) fn managed_execution_is_bound(&self) -> bool {
        matches!(
            self.managers.execution_owner,
            TerminalManagerExecutionOwnerV1::Managed(_)
        )
    }

    /// Cancels one task through the exact process manager that admitted it.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace/task identity is invalid, the task is unknown to this
    /// process owner, or cleanup cannot be confirmed.
    pub async fn cancel(
        &self,
        workspace_root: &Path,
        task_id: &TerminalTaskId,
    ) -> Result<TerminalTaskEntry> {
        self.managers
            .manager_for(
                workspace_root,
                &self.artifact_root,
                &self.artifact_label_root,
            )?
            .cancel(task_id)
            .await
    }

    /// Reads the latest process-owner state for one exact task.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace/task identity is invalid or the task is not owned by
    /// this process.
    pub async fn status(
        &self,
        workspace_root: &Path,
        task_id: &TerminalTaskId,
    ) -> Result<TerminalTaskEntry> {
        self.managers
            .manager_for(
                workspace_root,
                &self.artifact_root,
                &self.artifact_label_root,
            )?
            .status(task_id)
            .await
    }
}

pub(crate) struct TerminalProcessManagers {
    stopping: AtomicBool,
    terminal_execution_config: TerminalExecutionConfig,
    execution_owner: TerminalManagerExecutionOwnerV1,
    lifecycle_route: Option<TerminalLifecycleRoute>,
    scratch_leases: Option<Arc<crate::scratch_namespace::ScratchTaskLeaseRegistry>>,
    managers: StdMutex<BTreeMap<(PathBuf, PathBuf), Arc<TerminalProcessManager>>>,
    terminal_read_guards: StdMutex<TerminalReadGuardState>,
}

pub(crate) enum TerminalManagerExecutionOwnerV1 {
    Managed(Arc<dyn crate::ManagedTerminalExecutionPortV1>),
    #[cfg(any(test, feature = "test-support"))]
    LegacyDirect,
}

#[cfg(any(test, feature = "test-support"))]
impl Default for TerminalProcessManagers {
    fn default() -> Self {
        Self::new_legacy(TerminalExecutionConfig::default())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TerminalReadGuardKey {
    session_scope_id: String,
    logical_run_id: String,
    task_id: String,
    offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalReadGuardObservation {
    generation: u64,
    total_bytes: u64,
    sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalReadGuardDecision {
    Bypass,
    Proceed,
    UseTerminalWait,
}

#[derive(Debug, Default)]
pub(crate) struct TerminalReadGuardState {
    next_sequence: u64,
    observations: BTreeMap<TerminalReadGuardKey, TerminalReadGuardObservation>,
}

impl TerminalReadGuardState {
    pub(crate) fn observe(
        &mut self,
        key: TerminalReadGuardKey,
        generation: u64,
        total_bytes: u64,
        no_change: bool,
    ) -> TerminalReadGuardDecision {
        if !no_change {
            self.observations.remove(&key);
            return TerminalReadGuardDecision::Proceed;
        }
        if self.observations.get(&key).is_some_and(|observation| {
            observation.generation == generation && observation.total_bytes == total_bytes
        }) {
            return TerminalReadGuardDecision::UseTerminalWait;
        }
        if !self.observations.contains_key(&key)
            && self.observations.len() >= MAX_TERMINAL_READ_GUARDS
            && let Some(oldest) = self
                .observations
                .iter()
                .min_by_key(|(_, observation)| observation.sequence)
                .map(|(key, _)| key.clone())
        {
            self.observations.remove(&oldest);
        }
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.observations.insert(
            key,
            TerminalReadGuardObservation {
                generation,
                total_bytes,
                sequence: self.next_sequence,
            },
        );
        TerminalReadGuardDecision::Proceed
    }

    pub(crate) fn key(
        session_scope_id: &str,
        logical_run_id: &str,
        task_id: &str,
        offset: u64,
    ) -> TerminalReadGuardKey {
        TerminalReadGuardKey {
            session_scope_id: session_scope_id.to_owned(),
            logical_run_id: logical_run_id.to_owned(),
            task_id: task_id.to_owned(),
            offset,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.observations.len()
    }

    pub(crate) fn clear_task(
        &mut self,
        session_scope_id: &str,
        logical_run_id: &str,
        task_id: &str,
    ) {
        self.observations.retain(|key, _| {
            key.session_scope_id != session_scope_id
                || key.logical_run_id != logical_run_id
                || key.task_id != task_id
        });
    }
}

#[derive(Clone)]
pub enum TerminalLifecycleRoute {
    Bound(Arc<dyn sigil_kernel::TerminalLifecycleSink>),
    Factory(Arc<dyn sigil_kernel::TerminalLifecycleSinkFactory>),
}

impl TerminalProcessManagers {
    pub(crate) fn ensure_execution_available(&self) -> Result<()> {
        match &self.execution_owner {
            TerminalManagerExecutionOwnerV1::Managed(port) if !port.is_available() => {
                Err(anyhow::Error::new(
                    sigil_kernel::managed_execution::ManagedExecutionErrorV1::ProviderUnavailable,
                )
                .context("managed terminal launch failed"))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn new_managed(
        terminal_execution_config: TerminalExecutionConfig,
        managed_execution: Arc<dyn crate::ManagedTerminalExecutionPortV1>,
    ) -> Self {
        Self {
            stopping: AtomicBool::new(false),
            terminal_execution_config,
            execution_owner: TerminalManagerExecutionOwnerV1::Managed(managed_execution),
            lifecycle_route: None,
            scratch_leases: None,
            terminal_read_guards: StdMutex::new(TerminalReadGuardState::default()),
            managers: StdMutex::new(BTreeMap::new()),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn new_legacy(terminal_execution_config: TerminalExecutionConfig) -> Self {
        Self {
            stopping: AtomicBool::new(false),
            terminal_execution_config,
            execution_owner: TerminalManagerExecutionOwnerV1::LegacyDirect,
            lifecycle_route: None,
            scratch_leases: None,
            terminal_read_guards: StdMutex::new(TerminalReadGuardState::default()),
            managers: StdMutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn with_lifecycle_route(
        mut self,
        lifecycle_route: Option<TerminalLifecycleRoute>,
    ) -> Self {
        self.lifecycle_route = lifecycle_route;
        self
    }

    /// RFC-0062 14.1: shares the task-scoped scratch lease registry with every spawned task.
    pub(crate) fn with_scratch_task_leases(
        mut self,
        scratch_leases: Option<Arc<crate::scratch_namespace::ScratchTaskLeaseRegistry>>,
    ) -> Self {
        self.scratch_leases = scratch_leases;
        self
    }

    fn lifecycle_sink(
        &self,
        ctx: &ToolContext,
    ) -> Result<Option<Arc<dyn sigil_kernel::TerminalLifecycleSink>>> {
        match &self.lifecycle_route {
            None => Ok(None),
            Some(TerminalLifecycleRoute::Bound(sink)) => Ok(Some(Arc::clone(sink))),
            Some(TerminalLifecycleRoute::Factory(factory)) => {
                let session_scope_id = ctx.session_scope_id().ok_or_else(|| {
                    anyhow!("terminal lifecycle route requires an exact session scope")
                })?;
                let logical_run_id = ctx.logical_run_id().ok_or_else(|| {
                    anyhow!("terminal lifecycle route requires an exact logical run id")
                })?;
                let recorder = ctx.mutation_recorder.clone().ok_or_else(|| {
                    anyhow!("terminal lifecycle route requires a durable mutation recorder")
                })?;
                factory
                    .sink_for_run(session_scope_id, logical_run_id, recorder)
                    .map(Some)
            }
        }
    }

    fn resolve_shell(&self, explicit: Option<&str>) -> Result<ResolvedShell> {
        self.terminal_execution_config.resolve_shell(explicit)
    }

    fn permission_backend_binding(&self) -> String {
        self.terminal_execution_config.permission_backend_binding()
    }

    fn default_shell_summary(&self) -> String {
        self.resolve_shell(None)
            .map(|shell| {
                format!(
                    "{} ({})",
                    shell.program().display(),
                    shell.dialect().as_str()
                )
            })
            .unwrap_or_else(|_| "unavailable".to_owned())
    }

    pub(crate) fn manager_for(
        &self,
        workspace_root: &Path,
        artifact_root: &Path,
        artifact_label_root: &Path,
    ) -> Result<Arc<TerminalProcessManager>> {
        let workspace_root = canonical_workspace_root(workspace_root)?;
        let artifact_root = absolute_path_from(&workspace_root, artifact_root);
        let key = (workspace_root.clone(), artifact_root.clone());
        let mut managers = self
            .managers
            .lock()
            .map_err(|_| anyhow!("terminal process manager registry lock poisoned"))?;
        if let Some(manager) = managers.get(&key) {
            return Ok(Arc::clone(manager));
        }
        if self.stopping.load(Ordering::SeqCst) {
            bail!("execution owner is shutting down");
        }

        let manager = TerminalProcessManager::new_with_artifact_root_and_terminal_execution(
            &workspace_root,
            artifact_root,
            artifact_label_root.to_path_buf(),
            self.terminal_execution_config.clone(),
        )?
        .with_scratch_task_leases(self.scratch_leases.clone());
        let manager = match &self.execution_owner {
            TerminalManagerExecutionOwnerV1::Managed(managed_execution) => {
                manager.with_managed_execution(Arc::clone(managed_execution))
            }
            #[cfg(any(test, feature = "test-support"))]
            TerminalManagerExecutionOwnerV1::LegacyDirect => {
                manager.with_legacy_execution_for_test_support()
            }
        };
        let manager = Arc::new(manager);
        managers.insert(key, Arc::clone(&manager));
        Ok(manager)
    }

    fn observe_terminal_read(
        &self,
        ctx: &ToolContext,
        task_id: &TerminalTaskId,
        offset: u64,
        read: &TerminalReadResult,
    ) -> Result<TerminalReadGuardDecision> {
        let (Some(session_scope_id), Some(logical_run_id)) =
            (ctx.session_scope_id(), ctx.logical_run_id())
        else {
            return Ok(TerminalReadGuardDecision::Bypass);
        };
        let key =
            TerminalReadGuardState::key(session_scope_id, logical_run_id, task_id.as_str(), offset);
        self.terminal_read_guards
            .lock()
            .map_err(|_| anyhow!("terminal read guard lock poisoned"))
            .map(|mut state| state.observe(key, read.generation, read.total_bytes, read.no_change))
    }

    fn clear_terminal_read_guards(
        &self,
        ctx: &ToolContext,
        task_id: &TerminalTaskId,
    ) -> Result<()> {
        let (Some(session_scope_id), Some(logical_run_id)) =
            (ctx.session_scope_id(), ctx.logical_run_id())
        else {
            return Ok(());
        };
        let mut state = self
            .terminal_read_guards
            .lock()
            .map_err(|_| anyhow!("terminal read guard lock poisoned"))?;
        state.clear_task(session_scope_id, logical_run_id, task_id.as_str());
        Ok(())
    }
}

impl TerminalStartTool {
    fn session_scratch_dir(&self, ctx: &ToolContext) -> PathBuf {
        self.scratch.session_scratch_dir(ctx.session_scope_id())
    }

    fn analyze_command(
        &self,
        ctx: &ToolContext,
        command: &str,
        shell: &ResolvedShell,
    ) -> Result<ShellCommandAnalysis> {
        let path_policy = ShellPathPolicyBinding::for_runtime(
            &ctx.workspace_root,
            &self.session_scratch_dir(ctx),
            false,
        )?;
        analyze_shell_command_with_path_policy(&ctx.workspace_root, command, shell, &path_policy)
    }
}

#[async_trait]
impl Tool for TerminalStartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec_command".to_owned(),
            description: format!(
                "Execute a shell command from the workspace. The default shell is {}; explicit shell accepts modeled POSIX, PowerShell, or cmd executables. Finite commands, builds, tests, services, and interactive work use this one entry. pty selects terminal I/O independently of lifetime. Wait up to yield_time_ms (default 1000, maximum 60000); if still running, retain the returned execution_id and use exec_wait, exec_read, exec_input, exec_resize, or exec_cancel. max_runtime_secs optionally stops the process at a separate runtime deadline. A running response is not command success. Use ${SIGIL_SCRATCH_DIR_ENV} for temporary shell files that must survive across tool calls in this session (shown as {}). The scratch directory is scoped to the current session and private to this user; usage is measured during maintenance and expired contents are reclaimed. Do not rely on it for long-term storage. OS temp directories are outside the workspace and require permission.external_directory.",
                self.managers.default_shell_summary(),
                self.scratch_label
            ),
            input_schema: json!({
            "type": "object",
            "properties": {
                "command": { "type": "string" },
                "cwd": { "type": "string" },
                "shell": { "type": "string" },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 60000 },
                "max_runtime_secs": { "type": "integer", "minimum": 1, "maximum": 86400 },
                "pty": { "type": "boolean" },
                "rows": { "type": "integer" },
                "cols": { "type": "integer" }
            },
            "required": ["command"],
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let args = parse_terminal_start_args(args)?;
        let resolved_shell = self.managers.resolve_shell(args.shell.as_deref())?;
        let analysis = self.analyze_command(ctx, &args.command, &resolved_shell)?;
        let mut plan = analysis.permission_plan();
        let cwd = args.cwd.as_deref().and_then(Path::to_str);

        let mut terminal_subjects = Vec::new();
        if let Some(shell) = args.shell.as_deref() {
            terminal_subjects.push(ToolSubject::command(
                shell.to_owned(),
                command_permission_subject(shell),
            ));
        }
        terminal_subjects.push(tool_path_subject(&ctx.workspace_root, cwd.unwrap_or("."))?);
        if resolved_shell.dialect() == ShellDialect::Posix {
            terminal_subjects.extend(terminal_command_path_subjects(
                &ctx.workspace_root,
                cwd,
                &args.command,
            )?);
        }
        for subject in terminal_subjects {
            if !plan.subjects.contains(&subject) {
                plan.subjects.push(subject);
            }
        }

        plan.containment.process = ProcessContainment::OwnedTree;
        plan.containment.environment = EnvironmentContainment::Restricted;
        plan.containment.persistent_process = true;
        if let Some(scope) = plan.semantic_scope.as_mut() {
            scope
                .qualifiers
                .insert("io_mode".to_owned(), terminal_io_mode(args.pty).to_owned());
            scope
                .qualifiers
                .insert("terminal_pty".to_owned(), args.pty.to_string());
            scope
                .qualifiers
                .insert("terminal_readiness".to_owned(), "none".to_owned());
        }
        plan.analysis_bindings.insert(
            "terminal_execution_class".to_owned(),
            EXECUTION_CLASS_BINDING.to_owned(),
        );
        plan.analysis_bindings
            .insert("containment_proven".to_owned(), "false".to_owned());
        plan.analysis_bindings.insert(
            "execution_backend".to_owned(),
            self.managers.permission_backend_binding(),
        );
        plan.analysis_bindings.insert(
            "execution_profile".to_owned(),
            serde_json::to_string(&plan.containment)?,
        );
        plan.analysis_bindings.insert(
            "environment_binding".to_owned(),
            shell_execution_environment(
                ctx,
                &self.session_scratch_dir(ctx),
                &resolved_shell,
                &analysis.analysis_bindings,
            )?
            .binding,
        );
        plan.analysis_bindings.insert(
            "max_runtime_secs".to_owned(),
            serde_json::to_string(&args.max_runtime_secs)?,
        );
        plan.analysis_bindings
            .insert("io_mode".to_owned(), terminal_io_mode(args.pty).to_owned());
        plan.analysis_bindings
            .insert("terminal_pty".to_owned(), args.pty.to_string());
        plan.analysis_bindings
            .insert("terminal_readiness".to_owned(), "none".to_owned());
        plan.safe_summary = ToolPermissionSummary {
            title: "Execute a shell command".to_owned(),
            detail: plan.safe_summary.detail.clone(),
            step_count: plan.safe_summary.step_count.max(1),
            workspace_code_steps: plan.safe_summary.workspace_code_steps,
        };
        Ok(plan)
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let args = parse_terminal_start_args(&args)?;
        self.managers.ensure_execution_available()?;
        let shell = self.managers.resolve_shell(args.shell.as_deref())?;
        let execution_analysis = self.analyze_command(&ctx, &args.command, &shell)?;
        let workspace_resources = if execution_analysis.command_family.is_workspace_check() {
            let workspace = ctx.workspace_root.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    crate::shell::workspace_check_resource_observation(&workspace)
                })
                .await
                .context("workspace resource observation task panicked")?,
            )
        } else {
            None
        };
        let execution_environment = shell_execution_environment(
            &ctx,
            &self.session_scratch_dir(&ctx),
            &shell,
            ctx.prepared_permission_plan()
                .map(|plan| &plan.analysis_bindings)
                .unwrap_or(&execution_analysis.analysis_bindings),
        )?;
        let receipt_context = if let Some(plan) = ctx.prepared_permission_plan() {
            validate_terminal_start_prepared_plan(
                &ctx,
                plan,
                &args,
                &self.session_scratch_dir(&ctx),
                &self.managers,
                &execution_environment.binding,
            )?;
            TerminalShellReceiptContext::Prepared(plan)
        } else {
            TerminalShellReceiptContext::Analysis(&execution_analysis)
        };
        // Provision the authority-owned session scratch namespace before a forward effect.
        // Maintenance observations do not gate admission; actual allocation errors remain
        // recoverable tool errors and never select the system temp directory as a fallback.
        let scratch_control = self.scratch.clone();
        let provision_scope = ctx.session_scope_id().map(str::to_owned);
        let provision_quota = self.scratch_quota;
        let scratch_lease = match self
            .scratch
            .namespaces
            .acquire(&session_scratch_key(provision_scope.as_deref()))
        {
            Ok(lease) => lease,
            Err(error) => {
                return Ok(scratch_provision_error_result(
                    call_id,
                    self.spec().name,
                    &self.scratch_label,
                    error,
                ));
            }
        };
        let provision = tokio::task::spawn_blocking(move || {
            scratch_control.ensure_session_scratch(provision_scope.as_deref(), &provision_quota)
        })
        .await
        .context("scratch provisioning task panicked")?;
        match provision {
            Ok(_provision) => {}
            Err(error) => {
                drop(scratch_lease);
                return Ok(scratch_provision_error_result(
                    call_id,
                    self.spec().name,
                    &self.scratch_label,
                    error,
                ));
            }
        };
        let _process_effect = ctx.begin_forward_effect(sigil_kernel::RunEffectKind::Process)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        let env = execution_environment.environment;
        let display_command = safe_persistence_text(&args.command);
        let request = TerminalStartRequest {
            task_id: None,
            max_runtime_secs: args.max_runtime_secs,
            command: args.command,
            cwd: args.cwd,
            shell: Some(execution_environment.shell_program),
            env,
        };
        let lifecycle_sink = self.managers.lifecycle_sink(&ctx)?;
        let entry = if args.pty {
            manager
                .start_pty_with_readiness_and_sink(
                    request,
                    args.pty_size,
                    TerminalReadinessCondition::None,
                    lifecycle_sink,
                    ctx.cancellation_handle(),
                )
                .await?
        } else {
            manager
                .start_with_readiness_and_sink(
                    request,
                    TerminalReadinessCondition::None,
                    lifecycle_sink,
                    ctx.cancellation_handle(),
                )
                .await?
        };
        let task_id = entry.handle.task_id.clone();
        // Move the admission lease into the execution owner before any fallible projection or
        // wait. A failed cancellation must not drop scratch protection under a live process.
        self.scratch
            .tasks
            .register_owned(task_id.as_str(), scratch_lease);
        let mut snapshot = manager.snapshot(&task_id).await?;
        // A fast process may have terminalized just before the lease transfer. Re-observing
        // after registration closes that race; otherwise its later lifecycle wake releases it.
        if snapshot.entry.status.is_terminal() {
            self.scratch.tasks.release(task_id.as_str());
        }
        // Progress is a best-effort projection. Losing a display sink must not lose the already
        // admitted execution identity, scratch lease, or eventual process/cleanup receipt.
        let _ = ctx.emit_progress(ToolProgressEvent {
            execution_id: ToolExecutionId::new(task_id.as_str().to_owned())?,
            call_id: call_id.clone(), tool_name: self.spec().name, sequence: 1,
            status: "running".to_owned(), message: Some("command is running".to_owned()),
            output_preview: None, output_log_ref: None,
            total_bytes: Some(0), updated_at_ms: Some(snapshot.entry.updated_at_ms.max(snapshot.started_at_ms)),
            details: json!({ "execution_id": task_id.as_str(), "started_at_ms": snapshot.started_at_ms, "updated_at_ms": snapshot.entry.updated_at_ms, "call": { "summary": format!("command={display_command}") } }),
        });
        if !snapshot.entry.status.is_terminal() && !args.yield_time.is_zero() {
            let wait = wait_with_cancellation(
                &ctx,
                &manager,
                &task_id,
                snapshot.generation,
                TerminalWaitCondition::Exit,
                args.yield_time,
            )
            .await?;
            snapshot = wait.snapshot;
        }
        if snapshot.entry.status.is_terminal() {
            self.scratch.tasks.release(task_id.as_str());
        }
        let capture_entry = snapshot.entry.clone();
        let mut result = terminal_start_result(
            call_id,
            self.spec().name,
            snapshot,
            receipt_context,
            args.pty,
        );
        result.metadata.details["call"] = json!({"summary": format!("command={display_command}")});
        if let Some(resources) = workspace_resources {
            result.metadata.details["workspace_check_resources"] = resources;
        }
        capture::capture_terminal_snapshot(
            &ctx,
            result,
            capture_entry,
            manager.artifacts_for(&task_id)?,
        )
        .await
    }
}

#[async_trait]
impl Tool for TerminalReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec_read".to_owned(),
            description: "Inspect one explicit bounded page of a terminal task output log. This is not a polling tool; use exec_wait to wait for lifecycle or output changes.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execution_id": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit_bytes": { "type": "integer" },
                    "include_content": {
                        "type": "boolean",
                        "description": "Return the raw output slice in the tool result content. Defaults to false."
                    }
                },
                "required": ["execution_id", "offset"],
                }),
            category: ToolCategory::Shell,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let task_id = required_terminal_task_id(args)?;
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Read,
                network_effect: None,
                subjects: vec![terminal_task_subject(&task_id)],
                tool_default_mode: None,
                managed_file_access: None,
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let offset = required_u64_arg(&args, "offset")?;
        let limit_bytes = terminal_read_limit(&args)?;
        let include_content = args
            .get("include_content")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        let read = manager.read(&task_id, offset, limit_bytes).await?;
        if read
            .latest_entry
            .as_ref()
            .is_some_and(|entry| entry.status.is_terminal())
        {
            // RFC-0062 14.1: the terminal task has settled, so its scratch lease can be
            // released and the namespace becomes TTL-eligible.
            self.scratch.tasks.release(task_id.as_str());
        }
        if self
            .managers
            .observe_terminal_read(&ctx, &task_id, offset, &read)?
            == TerminalReadGuardDecision::UseTerminalWait
        {
            return Ok(ToolResult::error(
                call_id,
                self.spec().name,
                ToolErrorKind::InvalidInput,
                "terminal output has not changed since the previous read; use exec_wait instead of polling exec_read",
            )
            .with_error_details(
                false,
                json!({
                    "execution_id": task_id.as_str(),
                    "offset": offset,
                    "generation": read.generation,
                    "total_bytes": read.total_bytes,
                    "next_action": "exec_wait",
                    "after_generation": read.generation,
                }),
            ));
        }
        let result = ToolResult::ok(
            call_id,
            self.spec().name,
            terminal_read_content(&read, include_content),
            ToolResultMeta {
                bytes: Some(read.total_bytes),
                truncated: read.truncated,
                limit_bytes: Some(limit_bytes as u64),
                returned_bytes: Some(read.returned_bytes),
                omitted_bytes: (!include_content).then_some(read.returned_bytes),
                total_bytes: Some(read.total_bytes),
                returned_lines: Some(if include_content {
                    read.content.lines().count() as u64
                } else {
                    0
                }),
                details: terminal_read_details(&read, limit_bytes, include_content),
                ..ToolResultMeta::default()
            },
        );
        let mut result = result;
        attach_execution_snapshot(&mut result, &manager.snapshot(&task_id).await?);
        Ok(attach_terminal_read_artifact(&ctx, result, &read))
    }
}

#[async_trait]
impl Tool for TerminalWaitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec_wait".to_owned(),
            description: "Wait once for a terminal lifecycle or output condition. This subscribes to the task owner generation and does not poll terminal_read.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execution_id": { "type": "string" },
                    "after_generation": { "type": "integer" },
                    "until": {
                        "type": "string",
                        "enum": ["status_change", "exit", "output_contains", "output_regex"]
                    },
                    "value": { "type": "string" },
                    "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 60000 }
                },
                "required": ["execution_id"],
                }),
            category: ToolCategory::Shell,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let task_id = required_terminal_task_id(args)?;
        if args.get("after_generation").is_some() {
            required_u64_arg(args, "after_generation")?;
        }
        execution_yield_time(args)?;
        parse_terminal_wait_condition(args)?;
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Read,
                network_effect: None,
                subjects: vec![terminal_task_subject(&task_id)],
                tool_default_mode: None,
                managed_file_access: None,
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let max_wait = execution_yield_time(&args)?;
        let condition = parse_terminal_wait_condition(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        self.managers.clear_terminal_read_guards(&ctx, &task_id)?;
        let after_generation = args
            .get("after_generation")
            .map(|_| required_u64_arg(&args, "after_generation"))
            .transpose()?
            .unwrap_or(manager.snapshot(&task_id).await?.generation);
        let result = wait_with_cancellation(
            &ctx,
            &manager,
            &task_id,
            after_generation,
            condition,
            max_wait,
        )
        .await?;
        if result.snapshot.entry.status.is_terminal() {
            // RFC-0062 14.1: the terminal task has settled, so its scratch lease can be
            // released and the namespace becomes TTL-eligible.
            self.scratch.tasks.release(task_id.as_str());
        }
        let capture_entry = result.snapshot.entry.clone();
        let result = terminal_wait_result(call_id, self.spec().name, result);
        capture::capture_terminal_snapshot(
            &ctx,
            result,
            capture_entry,
            manager.artifacts_for(&task_id)?,
        )
        .await
    }
}

pub(crate) fn attach_terminal_read_artifact(
    ctx: &ToolContext,
    result: ToolResult,
    read: &TerminalReadResult,
) -> ToolResult {
    if read.content.is_empty() {
        return result;
    }
    let Some(mut sink) = ctx.create_policy_safe_tool_output_sink(
        &result.call_id,
        &result.tool_name,
        "text/plain; charset=utf-8",
        ToolArtifactEncoding::Utf8,
        ToolArtifactSensitivity::SensitiveLocal,
    ) else {
        return result;
    };
    let safe_content = safe_persistence_text(&read.content);
    let redaction_count =
        u32::from(safe_content != read.content || safe_content.len() as u64 != read.returned_bytes);
    let publication = sink
        .write_all(safe_content.as_bytes())
        .map_err(anyhow::Error::from)
        .and_then(|()| sink.finish_with_source_evidence(read.returned_bytes, redaction_count));
    match publication {
        Ok(descriptor) => result.with_captured_artifact(descriptor),
        Err(_) => result.with_unavailable_artifact_capture(read.returned_bytes),
    }
}

#[async_trait]
impl Tool for TerminalInputTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec_input".to_owned(),
            description:
                "Send input to an interactive terminal task when the backend supports stdin."
                    .to_owned(),
            input_schema: json!({
            "type": "object",
            "properties": {
                "execution_id": { "type": "string" },
                "input": {
                    "type": "string",
                    "maxLength": MAX_TERMINAL_INPUT_BYTES
                }
            },
            "required": ["execution_id", "input"],
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let (subjects, operation, analysis) = self.permission_facts(ctx, args)?;
        let mut plan = analysis.permission_plan();
        plan.access = ToolAccess::Execute;
        plan.operation = operation;
        plan.subjects = subjects;
        plan.effects.insert(ToolPermissionEffect::ProcessControl);
        plan.analysis_bindings
            .insert("terminal_execution_class".to_owned(), "input".to_owned());
        plan.safe_summary = ToolPermissionSummary {
            title: "Send terminal input".to_owned(),
            detail: "Send one bounded input payload to an existing interactive task".to_owned(),
            step_count: 1,
            workspace_code_steps: plan.safe_summary.workspace_code_steps,
        };
        Ok(plan)
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let input = required_string(&args, "input")?;
        if let Err(error) = validate_terminal_input_len(input) {
            let details = json!({
                "execution_id": task_id.as_str(),
                "input_bytes": input.len(),
                "limit_bytes": MAX_TERMINAL_INPUT_BYTES
            });
            let mut result = ToolResult::error(
                call_id,
                self.spec().name,
                ToolErrorKind::InvalidInput,
                error.to_string(),
            )
            .with_error_details(false, details.clone());
            result.metadata = ToolResultMeta {
                bytes: Some(input.len() as u64),
                limit_bytes: Some(MAX_TERMINAL_INPUT_BYTES as u64),
                details,
                ..ToolResultMeta::default()
            };
            return Ok(result);
        }
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        match manager.input(&task_id, input.to_owned()).await {
            Ok(result) => {
                let mut result = terminal_input_result(call_id, self.spec().name, result);
                attach_execution_snapshot(&mut result, &manager.snapshot(&task_id).await?);
                Ok(result)
            }
            Err(error) if is_terminal_backend_unsupported(&error) => {
                let details = json!({
                    "execution_id": task_id.as_str(),
                    "input_bytes": input.len(),
                    "supported": false,
                    "backend": "process"
                });
                let mut result = ToolResult::error(
                    call_id,
                    self.spec().name,
                    ToolErrorKind::Unsupported,
                    "exec_input is not supported by this terminal task backend",
                )
                .with_error_details(false, details.clone());
                result.metadata = ToolResultMeta {
                    bytes: Some(input.len() as u64),
                    details,
                    ..ToolResultMeta::default()
                };
                attach_execution_snapshot(&mut result, &manager.snapshot(&task_id).await?);
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }
}

impl TerminalInputTool {
    fn permission_facts(
        &self,
        ctx: &ToolContext,
        args: &Value,
    ) -> Result<(Vec<ToolSubject>, ToolOperation, ShellCommandAnalysis)> {
        let task_id = required_terminal_task_id(args)?;
        let input = required_string(args, "input")?;
        validate_terminal_input_len(input)?;
        let context = self.terminal_input_permission_context(ctx, &task_id)?;
        let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
        let shell = ResolvedShell::resolve_explicit(&context.shell)?;
        let analysis = self.analyze_input(ctx, &context, input, &shell)?;
        let operation = match analysis.operation {
            ToolOperation::ExecuteDestructiveCommand => ToolOperation::ExecuteDestructiveCommand,
            ToolOperation::ExecuteReadOnlyCommand
                if !matches!(analysis.command_family, CommandFamily::ShellNoop) =>
            {
                ToolOperation::ExecuteReadOnlyCommand
            }
            ToolOperation::ExecuteWorkspaceCheckCommand => {
                ToolOperation::ExecuteWorkspaceCheckCommand
            }
            _ => ToolOperation::SendTerminalInput,
        };
        let mut subjects = vec![
            terminal_task_subject(&task_id),
            terminal_input_subject(input.len()),
        ];
        subjects.extend(
            analysis
                .subjects
                .iter()
                .filter(|subject| subject.kind == ToolSubjectKind::Command)
                .cloned(),
        );
        if shell.dialect() == ShellDialect::Posix {
            subjects.extend(bash_path_subjects_from_cwd(
                &workspace_root,
                &context.cwd,
                input,
            )?);
        }
        Ok((subjects, operation, analysis))
    }

    fn analyze_input(
        &self,
        ctx: &ToolContext,
        context: &TerminalTaskPermissionContext,
        input: &str,
        shell: &ResolvedShell,
    ) -> Result<ShellCommandAnalysis> {
        let path_policy = context
            .scratch_root
            .as_deref()
            .map(|scratch_root| {
                ShellPathPolicyBinding::for_runtime(&ctx.workspace_root, scratch_root, false)
            })
            .transpose()?
            .unwrap_or_default();
        analyze_shell_command_with_path_policy(&ctx.workspace_root, input, shell, &path_policy)
    }

    fn terminal_input_permission_context(
        &self,
        ctx: &ToolContext,
        task_id: &TerminalTaskId,
    ) -> Result<TerminalTaskPermissionContext> {
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        manager.permission_context(task_id)
    }
}

#[async_trait]
impl Tool for TerminalResizeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec_resize".to_owned(),
            description: "Resize a PTY-backed terminal task.".to_owned(),
            input_schema: json!({
            "type": "object",
            "properties": {
                "execution_id": { "type": "string" },
                "rows": { "type": "integer" },
                "cols": { "type": "integer" }
            },
            "required": ["execution_id", "rows", "cols"],
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let task_id = required_terminal_task_id(args)?;
        required_terminal_pty_size(args)?;
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Execute,
                operation: ToolOperation::ResizeTerminalTask,
                network_effect: None,
                subjects: vec![terminal_task_subject(&task_id)],
                tool_default_mode: None,
                managed_file_access: None,
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let size = required_terminal_pty_size(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        match manager.resize(&task_id, size).await {
            Ok(result) => {
                let mut result = terminal_resize_result(call_id, self.spec().name, result);
                attach_execution_snapshot(&mut result, &manager.snapshot(&task_id).await?);
                Ok(result)
            }
            Err(error) if is_terminal_backend_unsupported(&error) => {
                let details = json!({
                    "execution_id": task_id.as_str(),
                    "rows": size.rows,
                    "cols": size.cols,
                    "supported": false,
                    "backend": "process"
                });
                let mut result = ToolResult::error(
                    call_id,
                    self.spec().name,
                    ToolErrorKind::Unsupported,
                    "exec_resize is not supported by this terminal task backend",
                )
                .with_error_details(false, details.clone());
                result.metadata = ToolResultMeta {
                    details,
                    ..ToolResultMeta::default()
                };
                attach_execution_snapshot(&mut result, &manager.snapshot(&task_id).await?);
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl Tool for TerminalCancelTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec_cancel".to_owned(),
            description: "Cancel a running terminal task with terminate and kill fallback."
                .to_owned(),
            input_schema: json!({
            "type": "object",
            "properties": {
                "execution_id": { "type": "string" }
            },
            "required": ["execution_id"],
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let task_id = required_terminal_task_id(args)?;
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Execute,
                operation: ToolOperation::CancelTerminalTask,
                network_effect: None,
                subjects: vec![terminal_task_subject(&task_id)],
                tool_default_mode: None,
                managed_file_access: None,
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        let entry = manager.cancel(&task_id).await?;
        // RFC-0062 14.1: a cancelled or interrupted terminal task no longer holds a scratch
        // lease, so TTL GC can reclaim its session namespace.
        self.scratch.tasks.release(task_id.as_str());
        let action = match entry.status {
            TerminalTaskStatus::Cancelled => "cancelled",
            TerminalTaskStatus::Interrupted => "interrupted",
            _ => "terminal",
        };
        let mut result = terminal_entry_result(call_id, self.spec().name, action, entry);
        attach_execution_snapshot(&mut result, &manager.snapshot(&task_id).await?);
        Ok(result)
    }
}

fn terminal_io_mode(pty: bool) -> &'static str {
    if pty { "pty" } else { "pipe" }
}

#[derive(Debug, Clone)]
pub(crate) struct TerminalStartArgs {
    command: String,
    cwd: Option<PathBuf>,
    shell: Option<String>,
    pty: bool,
    pty_size: Option<TerminalPtySize>,
    yield_time: Duration,
    max_runtime_secs: Option<u64>,
}

pub(crate) fn parse_terminal_start_args(args: &Value) -> Result<TerminalStartArgs> {
    for removed in [
        "mode",
        "task_id",
        "execution_id",
        "timeout_secs",
        "readiness",
    ] {
        anyhow::ensure!(
            args.get(removed).is_none(),
            "exec_command does not accept {removed}; use pty, yield_time_ms, and max_runtime_secs"
        );
    }
    let command = required_string(args, "command")?.to_owned();
    let cwd = optional_string(args, "cwd").map(PathBuf::from);
    let shell = optional_string(args, "shell").map(str::to_owned);
    let pty = match args.get("pty") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => bail!("pty must be a boolean"),
    };
    let yield_time = execution_yield_time(args)?;
    let max_runtime_secs = optional_positive_u64(args, "max_runtime_secs")?;
    anyhow::ensure!(
        max_runtime_secs.is_none_or(|seconds| seconds <= 86_400),
        "max_runtime_secs must be between 1 and 86400"
    );
    let pty_size = if args.get("rows").is_some() || args.get("cols").is_some() {
        Some(required_terminal_pty_size(args)?)
    } else {
        None
    };
    anyhow::ensure!(pty || pty_size.is_none(), "rows and cols require pty=true");
    Ok(TerminalStartArgs {
        command,
        cwd,
        shell,
        pty,
        pty_size,
        yield_time,
        max_runtime_secs,
    })
}

fn execution_yield_time(args: &Value) -> Result<Duration> {
    let millis = match args.get("yield_time_ms") {
        None => 1_000,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| anyhow!("yield_time_ms must be an unsigned integer"))?,
    };
    anyhow::ensure!(
        millis <= 60_000,
        "yield_time_ms must be between 0 and 60000"
    );
    Ok(Duration::from_millis(millis))
}

fn optional_positive_u64(args: &Value, key: &str) -> Result<Option<u64>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(value) = value.as_u64() else {
        bail!("{key} must be a positive integer");
    };
    if value == 0 {
        bail!("{key} must be greater than 0");
    }
    Ok(Some(value))
}

fn validate_terminal_start_prepared_plan(
    ctx: &ToolContext,
    plan: &ToolPermissionPlanV2,
    args: &TerminalStartArgs,
    scratch_root: &Path,
    managers: &TerminalProcessManagers,
    environment_binding: &str,
) -> Result<()> {
    anyhow::ensure!(
        plan.tool_name == "exec_command",
        "prepared permission plan belongs to a different tool"
    );
    anyhow::ensure!(
        plan.containment.process == ProcessContainment::OwnedTree
            && plan.containment.environment == EnvironmentContainment::Restricted
            && plan.containment.persistent_process,
        "prepared terminal containment changed before execution"
    );
    anyhow::ensure!(
        plan.analysis_bindings.get("max_runtime_secs")
            == Some(&serde_json::to_string(&args.max_runtime_secs)?),
        "prepared execution runtime deadline changed before execution"
    );
    let expected_backend = managers.permission_backend_binding();
    anyhow::ensure!(
        plan.analysis_bindings.get("execution_backend") == Some(&expected_backend),
        "prepared terminal execution backend changed before execution"
    );
    let expected_profile = serde_json::to_string(&plan.containment)?;
    anyhow::ensure!(
        plan.analysis_bindings.get("execution_profile") == Some(&expected_profile),
        "prepared terminal execution profile changed before execution"
    );
    anyhow::ensure!(
        plan.analysis_bindings
            .get("environment_binding")
            .map(String::as_str)
            == Some(environment_binding),
        "prepared terminal environment binding changed before execution"
    );
    let expected_path_policy =
        ShellPathPolicyBinding::for_runtime(&ctx.workspace_root, scratch_root, false)?
            .stable_hash();
    anyhow::ensure!(
        plan.analysis_bindings.get("path_policy_binding") == Some(&expected_path_policy),
        "prepared terminal symbolic path binding changed before execution"
    );
    for (key, expected) in [
        ("terminal_execution_class", EXECUTION_CLASS_BINDING),
        ("io_mode", terminal_io_mode(args.pty)),
        ("terminal_pty", if args.pty { "true" } else { "false" }),
        ("terminal_readiness", "none"),
    ] {
        anyhow::ensure!(
            plan.analysis_bindings.get(key).map(String::as_str) == Some(expected),
            "prepared terminal permission binding {key} changed before execution"
        );
    }
    Ok(())
}

pub(crate) fn required_terminal_task_id(args: &Value) -> Result<TerminalTaskId> {
    TerminalTaskId::new(required_string(args, "execution_id")?.to_owned())
}

pub(crate) fn required_terminal_pty_size(args: &Value) -> Result<TerminalPtySize> {
    TerminalPtySize::new(required_u16(args, "rows")?, required_u16(args, "cols")?)
}

pub(crate) fn required_u16(args: &Value, key: &str) -> Result<u16> {
    let value = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing positive integer field {key}"))?;
    u16::try_from(value).map_err(|_| anyhow!("{key} is too large for a terminal dimension"))
}

fn required_u64_arg(args: &Value, key: &str) -> Result<u64> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing unsigned integer field {key}"))
}

fn parse_terminal_wait_condition(args: &Value) -> Result<TerminalWaitCondition> {
    let until = optional_string(args, "until").unwrap_or("exit");
    let value = || {
        optional_string(args, "value")
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("exec_wait value is required for {until}"))
    };
    match until {
        "status_change" => Ok(TerminalWaitCondition::StatusChange),
        "exit" => Ok(TerminalWaitCondition::Exit),
        "output_contains" => Ok(TerminalWaitCondition::OutputContains(value()?)),
        "output_regex" => Ok(TerminalWaitCondition::OutputRegex(value()?)),
        _ => bail!("exec_wait until must be status_change, exit, output_contains, or output_regex"),
    }
}

pub(crate) fn terminal_read_limit(args: &Value) -> Result<usize> {
    Ok(optional_usize(args, "limit_bytes")?
        .unwrap_or(DEFAULT_TERMINAL_READ_LIMIT_BYTES)
        .clamp(1, HARD_TERMINAL_READ_LIMIT_BYTES))
}

pub(crate) fn terminal_task_subject(task_id: &TerminalTaskId) -> ToolSubject {
    let value = format!("terminal_task:{}", task_id.as_str());
    ToolSubject::command(value.clone(), value)
}

pub(crate) fn terminal_input_subject(input_bytes: usize) -> ToolSubject {
    ToolSubject::command(
        format!("exec_input bytes={input_bytes}"),
        format!("terminal_input_bytes:{input_bytes}"),
    )
}

pub(crate) fn validate_terminal_input_len(input: &str) -> Result<()> {
    if input.len() > MAX_TERMINAL_INPUT_BYTES {
        bail!(
            "exec_input input exceeds maximum of {} bytes",
            MAX_TERMINAL_INPUT_BYTES
        );
    }
    Ok(())
}

pub(crate) fn terminal_command_path_subjects(
    workspace_root: &Path,
    cwd: Option<&str>,
    command: &str,
) -> Result<Vec<ToolSubject>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let cwd = cwd
        .map(|cwd| resolve_tool_path_from_base(&workspace_root, &workspace_root, cwd))
        .transpose()?
        .map(|resolved| resolved.canonical)
        .unwrap_or_else(|| workspace_root.clone());
    bash_path_subjects_from_cwd(&workspace_root, &cwd, command)
}

pub(crate) fn terminal_entry_result(
    call_id: String,
    tool_name: String,
    action: &'static str,
    entry: TerminalTaskEntry,
) -> ToolResult {
    terminal_entry_result_with_shell_analysis(call_id, tool_name, action, entry, None)
}

pub(crate) fn terminal_entry_result_with_shell_analysis(
    call_id: String,
    tool_name: String,
    action: &'static str,
    entry: TerminalTaskEntry,
    analysis: Option<&ShellCommandAnalysis>,
) -> ToolResult {
    terminal_entry_result_with_shell_context(
        call_id,
        tool_name,
        action,
        entry,
        analysis.map(TerminalShellReceiptContext::Analysis),
    )
}

#[derive(Debug, Clone, Copy)]
enum TerminalShellReceiptContext<'a> {
    Analysis(&'a ShellCommandAnalysis),
    Prepared(&'a ToolPermissionPlanV2),
}

fn terminal_entry_result_with_shell_context(
    call_id: String,
    tool_name: String,
    action: &'static str,
    entry: TerminalTaskEntry,
    shell_context: Option<TerminalShellReceiptContext<'_>>,
) -> ToolResult {
    let (verdict, failure) = match &entry.status {
        TerminalTaskStatus::Starting | TerminalTaskStatus::Running => ("running", None),
        TerminalTaskStatus::Exited { exit_code: Some(0) } => ("success", None),
        TerminalTaskStatus::Exited { .. } => ("failed", Some(ToolErrorKind::ExitStatus)),
        TerminalTaskStatus::Cancelled | TerminalTaskStatus::Interrupted => {
            ("interrupted", Some(ToolErrorKind::Interrupted))
        }
        TerminalTaskStatus::Failed { reason } if reason == "managed terminal timed out" => {
            ("timed_out", Some(ToolErrorKind::Timeout))
        }
        TerminalTaskStatus::Failed { .. } => ("failed", Some(ToolErrorKind::ExitStatus)),
    };
    let cleanup_complete = entry.cleanup.as_ref().is_some_and(|receipt| {
        matches!(
            receipt.status,
            ExecutionCleanupStatus::Completed | ExecutionCleanupStatus::NotNeeded
        )
    });
    let invalid_syntax = matches!(
        entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(2) }
    ) && match shell_context {
        Some(TerminalShellReceiptContext::Analysis(analysis)) => matches!(
            analysis.analysis_status,
            sigil_kernel::ToolAnalysisStatus::Invalid { .. }
        ),
        Some(TerminalShellReceiptContext::Prepared(plan)) => matches!(
            plan.analysis,
            sigil_kernel::ToolAnalysisStatus::Invalid { .. }
        ),
        None => false,
    };
    let failure = if invalid_syntax {
        Some(ToolErrorKind::InvalidInput)
    } else {
        failure
    };
    let (verdict, failure) = if entry.status.is_terminal() && !cleanup_complete {
        ("cleanup_incomplete", Some(ToolErrorKind::Interrupted))
    } else {
        (verdict, failure)
    };
    let mut details = terminal_entry_details_with_shell_context(&entry, shell_context);
    details["verdict"] = json!(verdict);
    details["cleanup_complete"] = json!(cleanup_complete);
    if invalid_syntax {
        details["category"] = json!("shell_syntax");
    }
    details["exit_code"] = match entry.status {
        TerminalTaskStatus::Exited { exit_code } => json!(exit_code),
        _ => Value::Null,
    };
    if let Some(analysis) = details.get_mut("shell_analysis") {
        analysis["verdict"] = json!(verdict);
        analysis["exit_code"] = match entry.status {
            TerminalTaskStatus::Exited { exit_code } => json!(exit_code),
            _ => Value::Null,
        };
        let workspace_check = match shell_context {
            Some(TerminalShellReceiptContext::Analysis(analysis)) => {
                analysis.command_family.is_workspace_check()
            }
            Some(TerminalShellReceiptContext::Prepared(plan)) => {
                plan.safe_summary.workspace_code_steps > 0
            }
            None => false,
        };
        analysis["rerun_not_needed"] = json!(workspace_check && verdict == "success");
    }
    let content = format!(
        "{action} execution {}\nstatus: {}\nverdict: {verdict}{}",
        entry.handle.task_id.as_str(),
        entry.status.as_str(),
        entry
            .output_preview
            .as_deref()
            .map(|preview| format!("\n{preview}"))
            .unwrap_or_default()
    );
    let failure = if action == "cancelled" && cleanup_complete {
        None
    } else {
        failure
    };
    let mut result = if let Some(kind) = failure {
        ToolResult::error(call_id, tool_name, kind, content)
            .with_error_details(false, details.clone())
    } else {
        ToolResult::ok(call_id, tool_name, content, ToolResultMeta::default())
    };
    result.metadata = ToolResultMeta {
        exit_code: match entry.status {
            TerminalTaskStatus::Exited { exit_code } => exit_code,
            _ => None,
        },
        truncated: entry.output_truncated,
        total_bytes: Some(entry.output_total_bytes),
        limit_bytes: entry.output_limit_bytes,
        details,
        ..ToolResultMeta::default()
    };
    result
}

fn terminal_start_result(
    call_id: String,
    tool_name: String,
    snapshot: TerminalTaskSnapshot,
    shell_context: TerminalShellReceiptContext<'_>,
    pty: bool,
) -> ToolResult {
    let mut result = terminal_entry_result_with_shell_context(
        call_id,
        tool_name,
        "started",
        snapshot.entry.clone(),
        Some(shell_context),
    );
    attach_lifecycle_details(&mut result.metadata.details, &snapshot, pty);
    result.metadata.stdout_bytes = Some(snapshot.stdout_bytes);
    result.metadata.stderr_bytes = Some(snapshot.stderr_bytes);
    result
}

/// Control acknowledgements describe process state separately from the action outcome.
fn attach_execution_snapshot(result: &mut ToolResult, snapshot: &TerminalTaskSnapshot) {
    let mut state = terminal_entry_result(
        result.call_id.clone(),
        result.tool_name.clone(),
        "observed",
        snapshot.entry.clone(),
    );
    state.metadata.details["started_at_ms"] = json!(snapshot.started_at_ms);
    state.metadata.details["updated_at_ms"] =
        json!(snapshot.entry.updated_at_ms.max(snapshot.started_at_ms));
    // The kernel persists a complete owner receipt, not the display-state subset. Keep
    // paging/action fields on the result, but source every lifecycle field from this snapshot.
    let lifecycle = state
        .metadata
        .details
        .as_object()
        .expect("terminal entry details must be an object");
    let details = result
        .metadata
        .details
        .as_object_mut()
        .expect("execution result details must be an object");
    if details.contains_key("terminal_task") {
        // Reads may have observed an earlier generation before taking this snapshot.
        details.insert("terminal_task".to_owned(), Value::Object(lifecycle.clone()));
    }
    details.extend(lifecycle.clone());
    result.metadata.exit_code = state.metadata.exit_code;
    result.metadata.stdout_bytes = Some(snapshot.stdout_bytes);
    result.metadata.stderr_bytes = Some(snapshot.stderr_bytes);
}

fn attach_lifecycle_details(details: &mut Value, snapshot: &TerminalTaskSnapshot, pty: bool) {
    if let Some(object) = details.as_object_mut() {
        object.insert("started_at_ms".to_owned(), json!(snapshot.started_at_ms));
        object.insert(
            "updated_at_ms".to_owned(),
            json!(snapshot.entry.updated_at_ms.max(snapshot.started_at_ms)),
        );
        object.insert("generation".to_owned(), json!(snapshot.generation));
        object.insert("readiness".to_owned(), json!(&snapshot.readiness));
        object.insert("io_mode".to_owned(), json!(terminal_io_mode(pty)));
    }
}

async fn wait_with_cancellation(
    ctx: &ToolContext,
    manager: &TerminalProcessManager,
    task_id: &TerminalTaskId,
    after_generation: u64,
    condition: TerminalWaitCondition,
    max_wait: Duration,
) -> Result<TerminalWaitResult> {
    let wait = manager.wait(task_id, after_generation, condition, max_wait);
    if let Some(cancellation) = ctx.cancellation_handle() {
        tokio::select! {
            result = wait => result,
            () = cancellation.cancelled() => {
                if let Err(error) = manager.cancel(task_id).await {
                    cancellation.mark_cleanup_incomplete();
                    return Err(error);
                }
                let snapshot = manager.snapshot(task_id).await?;
                if !snapshot.entry.cleanup.as_ref().is_some_and(|receipt| matches!(receipt.status, ExecutionCleanupStatus::Completed | ExecutionCleanupStatus::NotNeeded)) {
                    cancellation.mark_cleanup_incomplete();
                }
                Ok(TerminalWaitResult { outcome: TerminalWaitOutcome::Cancelled, snapshot })
            }
        }
    } else {
        wait.await
    }
}

fn terminal_wait_result(
    call_id: String,
    tool_name: String,
    result: TerminalWaitResult,
) -> ToolResult {
    let outcome = match result.outcome {
        TerminalWaitOutcome::ConditionMet => "condition_met",
        TerminalWaitOutcome::Timeout => "timeout",
        TerminalWaitOutcome::OwnerShutdown => "owner_shutdown",
        TerminalWaitOutcome::Cancelled => "cancelled",
    };
    let mut tool_result = terminal_entry_result_with_shell_context(
        call_id,
        tool_name,
        "observed",
        result.snapshot.entry.clone(),
        None,
    );
    attach_execution_snapshot(&mut tool_result, &result.snapshot);
    tool_result.metadata.details["outcome"] = json!(outcome);
    tool_result.metadata.details["generation"] = json!(result.snapshot.generation);
    tool_result.metadata.details["readiness"] = json!(&result.snapshot.readiness);
    tool_result
}

pub(crate) fn terminal_entry_details(
    entry: &TerminalTaskEntry,
    analysis: Option<&ShellCommandAnalysis>,
) -> Value {
    terminal_entry_details_with_shell_context(
        entry,
        analysis.map(TerminalShellReceiptContext::Analysis),
    )
}

fn terminal_entry_details_with_shell_context(
    entry: &TerminalTaskEntry,
    shell_context: Option<TerminalShellReceiptContext<'_>>,
) -> Value {
    let mut details = json!({
        "schema_version": entry.schema_version,
        "execution_id": entry.handle.task_id.as_str(),
        "generation": entry.generation,
        "status": entry.status.as_str(),
        "status_detail": &entry.status,
        "readiness": &entry.readiness,
        "command_sha256": &entry.handle.command_sha256,
        "cwd_label": &entry.handle.cwd_label,
        "shell_label": &entry.handle.shell_label,
        "shell_sha256": &entry.handle.shell_sha256,
        "log_ref": &entry.handle.log_ref,
        "created_at_ms": entry.handle.created_at_ms,
        "updated_at_ms": entry.updated_at_ms,
        "output_preview": &entry.output_preview,
        "output_hash": &entry.output_hash,
        "output_truncated": entry.output_truncated,
        "output_total_bytes": entry.output_total_bytes,
        "output_limit_bytes": entry.output_limit_bytes,
        "output_termination_reason": entry.output_termination_reason
    });
    let details_object = details
        .as_object_mut()
        .expect("terminal task details should be a JSON object");
    details_object.insert(
        "execution_backend".to_owned(),
        json!(entry.handle.execution_backend),
    );
    details_object.insert(
        "execution_backend_capabilities".to_owned(),
        json!(entry.handle.execution_backend_capabilities),
    );
    details_object.insert(
        "enforcement_backend".to_owned(),
        json!(entry.handle.enforcement_backend),
    );
    details_object.insert(
        "enforcement_backend_capabilities".to_owned(),
        json!(entry.handle.enforcement_backend_capabilities),
    );
    details_object.insert(
        "sandbox_profile".to_owned(),
        json!(entry.handle.sandbox_profile),
    );
    details_object.insert("cleanup".to_owned(), json!(entry.cleanup));
    if let Some(shell_context) = shell_context {
        let shell_analysis = match shell_context {
            TerminalShellReceiptContext::Analysis(analysis) => json!({
                "program": analysis.shell_program.as_str(),
                "dialect": analysis.shell_dialect.as_str(),
                "command": analysis.command.as_str(),
                "normalized_command": analysis.normalized_command.as_str(),
                "command_family": analysis.command_family.as_str(),
                "classification_source": analysis.classification_source.as_str(),
                "grant_scope": analysis.grant_scope.as_ref().map(|scope| scope.as_str()),
                "grant_scope_detail": shell_grant_scope_detail(analysis.grant_scope.as_ref()),
                "approval_reason": analysis.explanation.as_str(),
                "exit_code": Value::Null,
                "verdict": "running",
                "output_truncated": entry.output_truncated,
                "tail_available": false,
                "rerun_not_needed": false,
            }),
            TerminalShellReceiptContext::Prepared(plan) => json!({
                "command_family": plan.semantic_scope.as_ref().map(|scope| scope.family.as_str()).unwrap_or("reviewed_shell"),
                "classification_source": "prepared_permission_plan_v2",
                "permission_plan_hash": plan.plan_hash.as_str(),
                "approval_reason": plan.operation.as_str(),
                "exit_code": Value::Null,
                "verdict": "running",
                "output_truncated": entry.output_truncated,
                "tail_available": false,
                "rerun_not_needed": false,
            }),
        };
        details_object.insert("shell_analysis".to_owned(), shell_analysis);
    }
    details
}

pub(crate) fn terminal_input_result(
    call_id: String,
    tool_name: String,
    result: TerminalInputResult,
) -> ToolResult {
    ToolResult::ok(
        call_id,
        tool_name,
        format!(
            "queued {} bytes for terminal task {}",
            result.input_bytes,
            result.task_id.as_str()
        ),
        ToolResultMeta {
            bytes: Some(result.input_bytes),
            details: json!({
                "execution_id": result.task_id.as_str(),
                "input_bytes": result.input_bytes,
                "backend": result.backend.as_str(),
                "supported": true
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(crate) fn terminal_resize_result(
    call_id: String,
    tool_name: String,
    result: TerminalResizeResult,
) -> ToolResult {
    ToolResult::ok(
        call_id,
        tool_name,
        format!(
            "resized terminal task {} to {}x{}",
            result.task_id.as_str(),
            result.size.cols,
            result.size.rows
        ),
        ToolResultMeta {
            details: json!({
                "execution_id": result.task_id.as_str(),
                "rows": result.size.rows,
                "cols": result.size.cols,
                "backend": result.backend.as_str(),
                "supported": true
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(crate) fn is_terminal_backend_unsupported(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("backend does not support input")
        || message.contains("backend does not support resize")
}

pub(crate) fn terminal_read_details(
    read: &TerminalReadResult,
    limit_bytes: usize,
    include_content: bool,
) -> Value {
    let mut details = json!({
        "execution_id": read.task_id.as_str(),
        "generation": read.generation,
        "readiness": &read.readiness,
        "offset": read.offset,
        "next_offset": read.next_offset,
        "returned_bytes": read.returned_bytes,
        "total_bytes": read.total_bytes,
        "limit_bytes": limit_bytes,
        "truncated": read.truncated,
        "content_returned": include_content,
        "content_omitted": !include_content,
        "no_change": read.no_change,
        "use_exec_wait": read.no_change
    });
    if let Some(entry) = &read.latest_entry
        && let Some(object) = details.as_object_mut()
    {
        object.insert(
            "terminal_task".to_owned(),
            terminal_entry_details(entry, None),
        );
    }
    if read.no_change
        && let Some(object) = details.as_object_mut()
    {
        object.insert("next_action".to_owned(), json!("exec_wait"));
        object.insert("after_generation".to_owned(), json!(read.generation));
    }
    details
}

pub(crate) fn terminal_read_content(read: &TerminalReadResult, include_content: bool) -> String {
    if include_content {
        return read.content.clone();
    }
    let mut lines = vec![format!(
        "terminal task {} read omitted from model context",
        read.task_id.as_str()
    )];
    lines.push(format!("offset: {}", read.offset));
    if let Some(next_offset) = read.next_offset {
        lines.push(format!("next_offset: {next_offset}"));
    }
    lines.push(format!("returned_bytes: {}", read.returned_bytes));
    lines.push(format!("total_bytes: {}", read.total_bytes));
    lines.push(format!("generation: {}", read.generation));
    if read.truncated {
        lines.push("truncated: true".to_owned());
    }
    if read.no_change {
        lines.push("no_change: true".to_owned());
        lines.push("use exec_wait instead of repeating exec_read".to_owned());
    }
    if let Some(entry) = &read.latest_entry {
        lines.push(format!("status: {}", entry.status.as_str()));
        lines.push(format!("log: {}", entry.handle.log_ref));
    }
    lines.push(
        "pass include_content=true to read a bounded raw output page for diagnosis".to_owned(),
    );
    lines.join("\n")
}
