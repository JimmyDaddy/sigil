use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use sigil_kernel::{ExecutionBackend, ExecutionConfig, ToolRegistry};

use crate::{
    changeset_tool::ApplyChangeSetTool,
    constants::{CHANGESET_ARTIFACT_ROOT, WORKSPACE_TEMP_ROOT},
    execution_backends::LocalExecutionBackend,
    file_tools::{
        DeleteFileTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool, WriteFileTool,
    },
    scratch_namespace::ScratchNamespaceControl,
    shell::BashTool,
    shell_runtime::ResolvedShell,
    terminal_process::{self, TerminalExecutionConfig},
    terminal_tools::{
        TerminalCancelTool, TerminalInputTool, TerminalLifecycleRoute, TerminalProcessManagers,
        TerminalReadTool, TerminalResizeTool, TerminalStartTool, TerminalTaskControlHandle,
        TerminalWaitTool,
    },
    tool_artifact_tool::ReadToolArtifactTool,
};

/// Handles returned by built-in tool registration: terminal task control and the shared
/// session-scoped scratch lease registry used by maintenance GC.
#[derive(Debug, Clone)]
pub struct BuiltinToolHandles {
    pub terminal: TerminalTaskControlHandle,
    pub scratch: ScratchNamespaceControl,
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
    )
}

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
    )
}

/// Registers built-ins with a session-bound terminal lifecycle route.
pub fn register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    execution_config: &ExecutionConfig,
    terminal_lifecycle_sink: Option<Arc<dyn sigil_kernel::TerminalLifecycleSink>>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::from_execution_config(execution_config),
        terminal_lifecycle_sink.map(TerminalLifecycleRoute::Bound),
    )
}

/// Registers built-ins with a factory that freezes a lifecycle sink from each exact tool context.
pub fn register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle_factory(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    execution_config: &ExecutionConfig,
    terminal_lifecycle_factory: Arc<dyn sigil_kernel::TerminalLifecycleSinkFactory>,
) -> BuiltinToolHandles {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::from_execution_config(execution_config),
        Some(TerminalLifecycleRoute::Factory(terminal_lifecycle_factory)),
    )
}

fn register_builtin_tools_with_paths_execution_backend_and_terminal_config(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    terminal_execution_config: TerminalExecutionConfig,
    terminal_lifecycle_route: Option<TerminalLifecycleRoute>,
) -> BuiltinToolHandles {
    let default_shell = ResolvedShell::detect_default();
    let terminal_execution_config =
        terminal_execution_config.with_default_shell(default_shell.clone());
    let terminal_managers = Arc::new(
        TerminalProcessManagers::new(terminal_execution_config)
            .with_lifecycle_route(terminal_lifecycle_route),
    );
    let scratch_control = ScratchNamespaceControl::new();
    let terminal_tasks_root = paths.terminal_tasks_root;
    let terminal_tasks_label_root = paths.terminal_tasks_label_root;
    let terminal_control = TerminalTaskControlHandle::new(
        Arc::clone(&terminal_managers),
        terminal_tasks_root.clone(),
        terminal_tasks_label_root.clone(),
    );
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(ReadToolArtifactTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(DeleteFileTool));
    registry.register(Arc::new(ApplyChangeSetTool {
        artifact_root: paths.changesets_root,
        artifact_label_root: paths.changesets_label_root,
    }));
    registry.register(Arc::new(ListTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry.register(Arc::new(BashTool {
        scratch_root: paths.scratch_root.clone(),
        scratch_label: paths.scratch_label.clone(),
        scratch_quota: paths.scratch_quota,
        scratch_namespaces: Arc::clone(&scratch_control.namespaces),
        backend: Arc::clone(&execution_backend),
        shell: default_shell,
    }));
    registry.register(Arc::new(TerminalStartTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
        scratch_root: paths.scratch_root,
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
    BuiltinToolHandles {
        terminal: terminal_control,
        scratch: scratch_control,
    }
}
