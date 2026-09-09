use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(any(test, feature = "test-support"))]
use crate::execution_backends::LocalExecutionBackend;

use crate::{
    changeset_tool::ApplyChangeSetTool,
    constants::{CHANGESET_ARTIFACT_ROOT, WORKSPACE_TEMP_ROOT},
    file_tools::{
        DeleteFileTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool, WriteFileTool,
    },
    managed_execution::ManagedCommandExecutionPortV1,
    scratch_namespace::ScratchNamespaceControl,
    shell::BashTool,
    shell_runtime::ResolvedShell,
    terminal_process::{self, TerminalExecutionConfig},
    terminal_tools::{
        TerminalCancelTool, TerminalInputTool, TerminalLifecycleRoute,
        TerminalManagerExecutionOwnerV1, TerminalProcessManagers, TerminalReadTool,
        TerminalResizeTool, TerminalStartTool, TerminalTaskControlHandle, TerminalWaitTool,
    },
    tool_artifact_tool::ReadToolArtifactTool,
    vcs_inspect::VcsInspectTool,
};
use sigil_kernel::ToolRegistry;
#[cfg(any(test, feature = "test-support"))]
use sigil_kernel::{ExecutionBackend, ExecutionConfig};
/// Handles returned by built-in tool registration: terminal task control and the shared
/// session-scoped scratch lease registry used by maintenance GC.
#[derive(Debug, Clone)]
pub struct BuiltinToolHandles {
    pub terminal: Option<TerminalTaskControlHandle>,
    pub scratch: ScratchNamespaceControl,
}

/// Optional built-in capabilities selected before their runtime owners are constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinToolSelection {
    /// Persistent terminal tasks and their lifecycle owner.
    pub terminal: bool,
    /// Multi-file changeset application.
    pub changesets: bool,
}

impl BuiltinToolSelection {
    /// File, artifact, VCS and one-shot command tools without optional runtime owners.
    #[must_use]
    pub const fn core() -> Self {
        Self {
            terminal: false,
            changesets: false,
        }
    }

    /// All built-in capabilities.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            terminal: true,
            changesets: true,
        }
    }
}

/// Authority-owned terminal inputs prepared only when terminal tasks are selected.
pub struct BuiltinTerminalOptions {
    /// Selected execution and confinement policy for persistent tasks.
    pub execution_config: TerminalExecutionConfig,
    /// Session-bound sink or factory that records terminal lifecycle events.
    pub lifecycle_route: Option<TerminalLifecycleRoute>,
    /// Runtime-owned authority route; built-ins never create a direct production backend.
    pub executor: Arc<dyn crate::ManagedTerminalExecutionPortV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinToolPaths {
    pub changesets_root: PathBuf,
    pub changesets_label_root: PathBuf,
    pub terminal_tasks_root: PathBuf,
    pub terminal_tasks_label_root: PathBuf,
    pub scratch_root: PathBuf,
    pub scratch_label: String,
    /// Capacity limits enforced before every scratch-using spawn; defaults to
    /// [`ScratchQuota::default`].
    pub scratch_quota: crate::scratch_namespace::ScratchQuota,
}

impl BuiltinToolPaths {
    #[must_use]
    pub fn workspace_defaults(workspace_root: &Path) -> Self {
        Self {
            changesets_root: workspace_root.join(CHANGESET_ARTIFACT_ROOT),
            changesets_label_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            terminal_tasks_root: workspace_root.join(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            terminal_tasks_label_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            scratch_root: workspace_root.join(WORKSPACE_TEMP_ROOT),
            scratch_label: WORKSPACE_TEMP_ROOT.to_owned(),
            scratch_quota: crate::scratch_namespace::ScratchQuota::default(),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    register_builtin_tools_with_paths(
        registry,
        BuiltinToolPaths {
            changesets_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            changesets_label_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            terminal_tasks_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            terminal_tasks_label_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            scratch_root: PathBuf::from(WORKSPACE_TEMP_ROOT),
            scratch_label: WORKSPACE_TEMP_ROOT.to_owned(),
            scratch_quota: crate::scratch_namespace::ScratchQuota::default(),
        },
    );
}

#[cfg(not(any(test, feature = "test-support")))]
pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    register_builtin_tools_with_unavailable_managed_execution(
        registry,
        BuiltinToolPaths {
            changesets_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            changesets_label_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            terminal_tasks_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            terminal_tasks_label_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            scratch_root: PathBuf::from(WORKSPACE_TEMP_ROOT),
            scratch_label: WORKSPACE_TEMP_ROOT.to_owned(),
            scratch_quota: crate::scratch_namespace::ScratchQuota::default(),
        },
    );
}

/// Registers built-ins without selecting a process backend. This is the safe default for
/// callers that only need tool contracts or diagnostics; command and terminal execution fail
/// closed until the runtime supplies an authority-owned managed execution route.
pub fn register_builtin_tools_with_unavailable_managed_execution(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
) -> BuiltinToolHandles {
    register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
        registry,
        paths,
        Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1),
        TerminalExecutionConfig::default(),
        None,
        None,
        Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1),
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn register_builtin_tools_with_paths(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_and_execution_backend(
        registry,
        paths,
        Arc::new(LocalExecutionBackend),
    )
}

#[cfg(not(any(test, feature = "test-support")))]
pub fn register_builtin_tools_with_paths(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
) -> BuiltinToolHandles {
    register_builtin_tools_with_unavailable_managed_execution(registry, paths)
}

#[cfg(any(test, feature = "test-support"))]
pub fn register_builtin_tools_with_paths_and_execution_backend(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::default(),
        None,
        None,
    )
}

#[cfg(any(test, feature = "test-support"))]
pub fn register_builtin_tools_with_paths_execution_backend_and_execution_config(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    execution_config: &ExecutionConfig,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle(
        registry,
        paths,
        execution_backend,
        execution_config,
        None,
        None,
    )
}

/// Registers built-ins with a session-bound terminal lifecycle route.
///
/// `external_scratch_control` shares the process-scoped scratch lease registry across repeated
/// surface assemblies (Desktop serve); `None` creates a fresh registry (TUI worker).
#[cfg(any(test, feature = "test-support"))]
pub fn register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    execution_config: &ExecutionConfig,
    terminal_lifecycle_sink: Option<Arc<dyn sigil_kernel::TerminalLifecycleSink>>,
    external_scratch_control: Option<ScratchNamespaceControl>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::from_execution_config(execution_config),
        terminal_lifecycle_sink.map(TerminalLifecycleRoute::Bound),
        external_scratch_control,
    )
}

/// Registers built-ins with a factory that freezes a lifecycle sink from each exact tool context.
#[cfg(any(test, feature = "test-support"))]
pub fn register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle_factory(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    execution_config: &ExecutionConfig,
    terminal_lifecycle_factory: Arc<dyn sigil_kernel::TerminalLifecycleSinkFactory>,
    external_scratch_control: Option<ScratchNamespaceControl>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::from_execution_config(execution_config),
        Some(TerminalLifecycleRoute::Factory(terminal_lifecycle_factory)),
        external_scratch_control,
    )
}

#[cfg(any(test, feature = "test-support"))]
fn register_builtin_tools_with_paths_execution_backend_and_terminal_config(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    terminal_execution_config: TerminalExecutionConfig,
    terminal_lifecycle_route: Option<TerminalLifecycleRoute>,
    external_scratch_control: Option<ScratchNamespaceControl>,
) -> BuiltinToolHandles {
    let managed_executor: Arc<dyn ManagedCommandExecutionPortV1> = Arc::new(
        crate::managed_execution::LegacyBackendCommandExecutionPortV1 {
            backend: Arc::clone(&execution_backend),
        },
    );
    register_builtin_tools_with_legacy_terminal(
        registry,
        paths,
        managed_executor,
        terminal_execution_config,
        terminal_lifecycle_route,
        external_scratch_control,
    )
}

/// Registers built-ins with an authority-owned managed command port and a fail-closed terminal
/// port. Callers that have not composed the authority owner must use the unavailable helper.
pub fn register_builtin_tools_with_managed_execution_and_terminal_config(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    managed_executor: Arc<dyn ManagedCommandExecutionPortV1>,
    terminal_execution_config: TerminalExecutionConfig,
    terminal_lifecycle_route: Option<TerminalLifecycleRoute>,
    external_scratch_control: Option<ScratchNamespaceControl>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
        registry,
        paths,
        managed_executor,
        terminal_execution_config,
        terminal_lifecycle_route,
        external_scratch_control,
        Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1),
    )
}

/// Registers built-ins with managed one-shot and persistent terminal execution ports.
///
/// The terminal port is mandatory in normal builds, so terminal startup cannot fall back to a
/// direct `Command` or PTY spawn when authority composition is missing.
pub fn register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    managed_executor: Arc<dyn ManagedCommandExecutionPortV1>,
    terminal_execution_config: TerminalExecutionConfig,
    terminal_lifecycle_route: Option<TerminalLifecycleRoute>,
    external_scratch_control: Option<ScratchNamespaceControl>,
    managed_terminal: Arc<dyn crate::ManagedTerminalExecutionPortV1>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_selection(
        registry,
        paths,
        managed_executor,
        BuiltinToolSelection::standard(),
        external_scratch_control,
        || BuiltinTerminalOptions {
            execution_config: terminal_execution_config,
            lifecycle_route: terminal_lifecycle_route,
            executor: managed_terminal,
        },
    )
}

/// Registers selected built-ins through the shared file and command implementations.
///
/// `terminal` is called only when terminal tasks are selected. Callers can therefore defer
/// terminal configuration and authority-port preparation without constructing an unused owner.
pub fn register_builtin_tools_with_selection(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    managed_executor: Arc<dyn ManagedCommandExecutionPortV1>,
    selection: BuiltinToolSelection,
    external_scratch_control: Option<ScratchNamespaceControl>,
    terminal: impl FnOnce() -> BuiltinTerminalOptions,
) -> BuiltinToolHandles {
    register_builtin_tools_with_selection_impl(
        registry,
        paths,
        managed_executor,
        selection,
        external_scratch_control,
        || {
            let terminal = terminal();
            (
                terminal.execution_config,
                terminal.lifecycle_route,
                TerminalManagerExecutionOwnerV1::Managed(terminal.executor),
            )
        },
    )
}

#[cfg(any(test, feature = "test-support"))]
fn register_builtin_tools_with_legacy_terminal(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    managed_executor: Arc<dyn ManagedCommandExecutionPortV1>,
    terminal_execution_config: TerminalExecutionConfig,
    terminal_lifecycle_route: Option<TerminalLifecycleRoute>,
    external_scratch_control: Option<ScratchNamespaceControl>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_selection_impl(
        registry,
        paths,
        managed_executor,
        BuiltinToolSelection::standard(),
        external_scratch_control,
        || {
            (
                terminal_execution_config,
                terminal_lifecycle_route,
                TerminalManagerExecutionOwnerV1::LegacyDirect,
            )
        },
    )
}

fn register_builtin_tools_with_selection_impl(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    managed_executor: Arc<dyn ManagedCommandExecutionPortV1>,
    selection: BuiltinToolSelection,
    external_scratch_control: Option<ScratchNamespaceControl>,
    terminal: impl FnOnce() -> (
        TerminalExecutionConfig,
        Option<TerminalLifecycleRoute>,
        TerminalManagerExecutionOwnerV1,
    ),
) -> BuiltinToolHandles {
    let default_shell = ResolvedShell::detect_default();
    let scratch_control = external_scratch_control.unwrap_or_else(|| {
        #[cfg(test)]
        {
            ScratchNamespaceControl::for_local_root(paths.scratch_root.clone())
        }
        #[cfg(not(test))]
        {
            ScratchNamespaceControl::unavailable()
        }
    });
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(ReadToolArtifactTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(DeleteFileTool));
    if selection.changesets {
        registry.register(Arc::new(ApplyChangeSetTool {
            artifact_root: paths.changesets_root.clone(),
            artifact_label_root: paths.changesets_label_root.clone(),
        }));
    }
    registry.register(Arc::new(ListTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry.register(Arc::new(VcsInspectTool));
    registry.register(Arc::new(BashTool {
        scratch_label: paths.scratch_label.clone(),
        scratch_quota: paths.scratch_quota,
        scratch_control: scratch_control.clone(),
        scratch_namespaces: Arc::clone(&scratch_control.namespaces),
        executor: managed_executor,
        shell: default_shell.clone(),
    }));
    let terminal_control = selection.terminal.then(|| {
        let (execution_config, lifecycle_route, owner) = terminal();
        register_terminal_tools(
            registry,
            paths,
            &scratch_control,
            execution_config.with_default_shell(default_shell),
            lifecycle_route,
            owner,
        )
    });
    BuiltinToolHandles {
        terminal: terminal_control,
        scratch: scratch_control,
    }
}

fn register_terminal_tools(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    scratch_control: &ScratchNamespaceControl,
    execution_config: TerminalExecutionConfig,
    lifecycle_route: Option<TerminalLifecycleRoute>,
    owner: TerminalManagerExecutionOwnerV1,
) -> TerminalTaskControlHandle {
    let terminal_managers = match owner {
        TerminalManagerExecutionOwnerV1::Managed(managed_terminal) => {
            TerminalProcessManagers::new_managed(execution_config, managed_terminal)
        }
        #[cfg(any(test, feature = "test-support"))]
        TerminalManagerExecutionOwnerV1::LegacyDirect => {
            TerminalProcessManagers::new_legacy(execution_config)
        }
    };
    let terminal_managers = Arc::new(
        terminal_managers
            .with_lifecycle_route(lifecycle_route)
            .with_scratch_task_leases(Some(Arc::clone(&scratch_control.tasks))),
    );
    let terminal_tasks_root = paths.terminal_tasks_root;
    let terminal_tasks_label_root = paths.terminal_tasks_label_root;
    let terminal_control = TerminalTaskControlHandle::new(
        Arc::clone(&terminal_managers),
        terminal_tasks_root.clone(),
        terminal_tasks_label_root.clone(),
    );
    registry.register(Arc::new(TerminalStartTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
        scratch_label: paths.scratch_label,
        scratch_quota: paths.scratch_quota,
        scratch: scratch_control.clone(),
    }));
    registry.register(Arc::new(TerminalReadTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
        scratch: scratch_control.clone(),
    }));
    registry.register(Arc::new(TerminalWaitTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
        scratch: scratch_control.clone(),
    }));
    registry.register(Arc::new(TerminalInputTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
    }));
    registry.register(Arc::new(TerminalResizeTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
    }));
    registry.register(Arc::new(TerminalCancelTool {
        artifact_root: terminal_tasks_root,
        artifact_label_root: terminal_tasks_label_root,
        managers: terminal_managers,
        scratch: scratch_control.clone(),
    }));
    terminal_control
}

#[cfg(test)]
#[path = "tests/registry_tests.rs"]
mod tests;
