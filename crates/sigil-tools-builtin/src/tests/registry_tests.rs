use std::{cell::Cell, fs, sync::Arc};

use crate::tests::file_tool_fixture::execute_registered_file_call;
use anyhow::Result;
use serde_json::json;
use sigil_kernel::{ToolCall, ToolContext, ToolRegistry};

use crate::{
    BuiltinTerminalOptions, BuiltinToolPaths, BuiltinToolSelection, TerminalExecutionConfig,
    UnavailableManagedCommandExecutionPortV1, register_builtin_tools_with_selection,
    register_builtin_tools_with_unavailable_managed_execution,
};

#[tokio::test]
async fn core_reads_files_without_preparing_optional_terminal_runtime() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    fs::write(workspace.path().join("source.txt"), "core file content")?;
    let paths = BuiltinToolPaths::workspace_defaults(workspace.path());
    let mut registry = ToolRegistry::new();
    let handles = register_builtin_tools_with_selection(
        &mut registry,
        paths.clone(),
        Arc::new(UnavailableManagedCommandExecutionPortV1),
        BuiltinToolSelection::core(),
        None,
        || panic!("core must not prepare terminal configuration or its authority port"),
    );

    assert!(handles.terminal.is_none());
    for name in [
        "read_file",
        "read_tool_artifact",
        "write_file",
        "edit_file",
        "delete_file",
        "ls",
        "glob",
        "grep",
        "vcs_inspect",
        "bash",
    ] {
        assert!(
            registry.spec_for(name).is_some(),
            "missing core tool {name}"
        );
    }
    let result = execute_registered_file_call(
        &registry,
        ToolContext::new(workspace.path(), 5),
        ToolCall {
            id: "core-read".to_owned(),
            name: "read_file".to_owned(),
            args_json: json!({ "path": "source.txt" }).to_string(),
        },
    )
    .await?;
    assert!(!result.is_error(), "{result:?}");
    assert!(result.content.contains("core file content"));
    for name in ["terminal_start", "terminal_read", "apply_changeset"] {
        assert!(registry.spec_for(name).is_none());
        assert!(
            registry
                .execute(
                    ToolContext::new(workspace.path(), 5),
                    ToolCall {
                        id: format!("unselected-{name}"),
                        name: name.to_owned(),
                        args_json: "{}".to_owned(),
                    },
                )
                .await
                .is_err(),
            "unselected tool {name} must not remain invocable"
        );
    }
    assert!(!paths.terminal_tasks_root.exists());
    assert!(!paths.changesets_root.exists());
    assert!(!paths.scratch_root.exists());
    Ok(())
}

#[test]
fn changesets_can_be_selected_without_terminal_preparation() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let mut registry = ToolRegistry::new();
    let handles = register_builtin_tools_with_selection(
        &mut registry,
        BuiltinToolPaths::workspace_defaults(workspace.path()),
        Arc::new(UnavailableManagedCommandExecutionPortV1),
        BuiltinToolSelection {
            terminal: false,
            changesets: true,
        },
        None,
        || panic!("changeset registration must not prepare a terminal runtime"),
    );
    assert!(handles.terminal.is_none());
    assert!(registry.spec_for("apply_changeset").is_some());
    assert!(registry.spec_for("terminal_start").is_none());
    Ok(())
}

#[test]
fn terminal_selection_prepares_its_owner_once_without_changesets() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let mut registry = ToolRegistry::new();
    let preparations = Cell::new(0);
    let handles = register_builtin_tools_with_selection(
        &mut registry,
        BuiltinToolPaths::workspace_defaults(workspace.path()),
        Arc::new(UnavailableManagedCommandExecutionPortV1),
        BuiltinToolSelection {
            terminal: true,
            changesets: false,
        },
        None,
        || {
            preparations.set(preparations.get() + 1);
            BuiltinTerminalOptions {
                execution_config: TerminalExecutionConfig::default(),
                lifecycle_route: None,
                executor: Arc::new(UnavailableManagedCommandExecutionPortV1),
            }
        },
    );
    assert_eq!(preparations.get(), 1);
    assert!(handles.terminal.is_some());
    assert!(registry.spec_for("terminal_start").is_some());
    assert!(registry.spec_for("terminal_cancel").is_some());
    assert!(registry.spec_for("apply_changeset").is_none());
    Ok(())
}

#[test]
fn standard_entrypoint_uses_the_same_selected_tool_contracts() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let paths = BuiltinToolPaths::workspace_defaults(workspace.path());
    let mut default_registry = ToolRegistry::new();
    let default_handles = register_builtin_tools_with_unavailable_managed_execution(
        &mut default_registry,
        paths.clone(),
    );
    let mut selected_registry = ToolRegistry::new();
    let selected_handles = register_builtin_tools_with_selection(
        &mut selected_registry,
        paths,
        Arc::new(UnavailableManagedCommandExecutionPortV1),
        BuiltinToolSelection::standard(),
        None,
        || BuiltinTerminalOptions {
            execution_config: TerminalExecutionConfig::default(),
            lifecycle_route: None,
            executor: Arc::new(UnavailableManagedCommandExecutionPortV1),
        },
    );
    assert!(default_handles.terminal.is_some());
    assert!(selected_handles.terminal.is_some());
    assert_eq!(
        serde_json::to_value(default_registry.specs())?,
        serde_json::to_value(selected_registry.specs())?
    );
    Ok(())
}
