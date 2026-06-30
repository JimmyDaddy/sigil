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
    shell::BashTool,
    terminal_process::{self, TerminalExecutionConfig},
    terminal_tools::{
        TerminalCancelTool, TerminalInputTool, TerminalProcessManagers, TerminalReadTool,
        TerminalResizeTool, TerminalStartTool,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinToolPaths {
    pub changesets_root: PathBuf,
    pub changesets_label_root: PathBuf,
    pub terminal_tasks_root: PathBuf,
    pub terminal_tasks_label_root: PathBuf,
    pub scratch_root: PathBuf,
    pub scratch_label: String,
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
        },
    );
}

pub fn register_builtin_tools_with_paths(registry: &mut ToolRegistry, paths: BuiltinToolPaths) {
    register_builtin_tools_with_paths_and_execution_backend(
        registry,
        paths,
        Arc::new(LocalExecutionBackend),
    );
}

pub fn register_builtin_tools_with_paths_and_execution_backend(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
) {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::default(),
    );
}

pub fn register_builtin_tools_with_paths_execution_backend_and_execution_config(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    execution_config: &ExecutionConfig,
) {
    register_builtin_tools_with_paths_execution_backend_and_terminal_config(
        registry,
        paths,
        execution_backend,
        TerminalExecutionConfig::from_execution_config(execution_config),
    );
}

fn register_builtin_tools_with_paths_execution_backend_and_terminal_config(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
    terminal_execution_config: TerminalExecutionConfig,
) {
    let terminal_managers = Arc::new(TerminalProcessManagers::new(terminal_execution_config));
    let terminal_tasks_root = paths.terminal_tasks_root;
    let terminal_tasks_label_root = paths.terminal_tasks_label_root;
    registry.register(Arc::new(ReadFileTool));
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
        backend: Arc::clone(&execution_backend),
    }));
    registry.register(Arc::new(TerminalStartTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
        scratch_root: paths.scratch_root,
        scratch_label: paths.scratch_label,
    }));
    registry.register(Arc::new(TerminalReadTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
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
    }));
}
