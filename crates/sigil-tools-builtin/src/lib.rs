mod changeset_tool;
mod constants;
mod execution_backends;
mod file_tools;
mod managed_execution;
mod path;
#[cfg(unix)]
mod process_group;
mod process_owner;
mod registry;
mod scratch_namespace;
mod shell;
mod shell_runtime;
mod support;
mod terminal_process;
mod terminal_tools;
mod tool_artifact_tool;
mod vcs_inspect;
pub mod webfetch;

pub use changeset_tool::{
    ChangeSetArtifactRecord, ChangeSetArtifactStore, ChangeSetArtifactSummary,
    ChangeSetDiffArtifact,
};
pub use execution_backends::{
    DockerExecutionBackend, LinuxBubblewrapExecutionBackend, LocalExecutionBackend,
    LongLivedStdioProcessPlan, MacosSeatbeltExecutionBackend, build_execution_backend,
    long_lived_stdio_process_plan,
};
#[cfg(any(test, feature = "test-support"))]
pub use managed_execution::LegacyBackendCommandExecutionPortV1;
pub use managed_execution::{
    ManagedCommandExecutionPortV1, ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1,
    UnavailableManagedCommandExecutionPortV1,
};
pub use registry::{
    BuiltinToolHandles, BuiltinToolPaths, register_builtin_tools,
    register_builtin_tools_with_managed_execution_and_terminal_config,
    register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal,
    register_builtin_tools_with_paths, register_builtin_tools_with_unavailable_managed_execution,
};
#[cfg(any(test, feature = "test-support"))]
pub use registry::{
    register_builtin_tools_with_paths_and_execution_backend,
    register_builtin_tools_with_paths_execution_backend_and_execution_config,
    register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle,
    register_builtin_tools_with_paths_execution_backend_execution_config_and_terminal_lifecycle_factory,
};
pub use scratch_namespace::{
    ScratchDeleteOutcome, ScratchGcConfig, ScratchGcReport, ScratchMeasurementError,
    ScratchNamespaceControl, ScratchNamespaceLease, ScratchNamespaceLeaseRegistry,
    ScratchNamespaceProvider, ScratchNamespaceProviderLease, ScratchQuota,
    ScratchTaskLeaseRegistry, ScratchUsage, SessionScratchProvision, session_scratch_key,
};
pub use terminal_process::{
    MAX_TERMINAL_INPUT_BYTES, TerminalBackendKind, TerminalExecutionConfig, TerminalInputResult,
    TerminalProcessManager, TerminalPtySize, TerminalReadResult, TerminalResizeResult,
    TerminalStartRequest, TerminalTaskArtifacts, TerminalTaskPermissionContext,
};
pub use terminal_tools::TerminalLifecycleRoute;
pub use terminal_tools::TerminalTaskControlHandle;

/// Offline, secret-free summary of the built-in terminal runtime selected for this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinTerminalPlatformCapability {
    /// Executable selected once for this process's built-in shell and terminal tools.
    pub resolved_shell: String,
    /// Command syntax expected by the resolved executable.
    pub shell_dialect: &'static str,
    /// Platform lifecycle primitive used to own descendant processes.
    pub process_tree_owner: &'static str,
    /// Whether the local execution backend provides filesystem or network confinement.
    pub local_execution_sandboxed: bool,
}

/// Resolves the native shell and validates platform process-tree ownership without running a
/// command or accessing the network.
///
/// # Errors
///
/// Returns an error when the platform process-tree owner cannot be initialized.
pub fn inspect_builtin_terminal_platform_capability()
-> anyhow::Result<BuiltinTerminalPlatformCapability> {
    let shell = shell_runtime::ResolvedShell::detect_default();
    process_owner::validate_process_tree_owner()?;
    Ok(BuiltinTerminalPlatformCapability {
        resolved_shell: shell.program_string(),
        shell_dialect: shell.dialect().as_str(),
        process_tree_owner: if cfg!(windows) {
            "windows_job_object"
        } else if cfg!(unix) {
            "unix_process_group"
        } else {
            "direct_child_only"
        },
        local_execution_sandboxed: false,
    })
}
pub use webfetch::{
    WebFetchAuthorizedDialPlan, WebFetchError, WebFetchFetchedResponse, WebFetchFormat,
    WebFetchHopResult, WebFetchLimits, WebFetchNetworkGuard, WebFetchProxyEnvSource, WebFetchRoute,
    WebFetchTransport, WebFetchTransportSecurity,
};

#[cfg(test)]
pub(crate) use changeset_tool::*;
#[cfg(test)]
pub(crate) use constants::*;
#[cfg(test)]
pub(crate) use execution_backends::*;
#[cfg(test)]
pub(crate) use file_tools::*;
#[cfg(test)]
pub(crate) use path::*;
#[cfg(test)]
pub(crate) use shell::*;
#[cfg(test)]
pub(crate) use support::*;
#[cfg(test)]
pub(crate) use terminal_tools::*;
#[cfg(test)]
pub(crate) use tool_artifact_tool::*;

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
