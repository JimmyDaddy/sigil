mod changeset_tool;
mod constants;
mod execution_backends;
mod file_tools;
mod path;
#[cfg(unix)]
mod process_group;
mod registry;
mod shell;
mod support;
mod terminal_process;
mod terminal_tools;
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
pub use registry::{
    BuiltinToolPaths, register_builtin_tools, register_builtin_tools_with_paths,
    register_builtin_tools_with_paths_and_execution_backend,
    register_builtin_tools_with_paths_execution_backend_and_execution_config,
};
pub use terminal_process::{
    MAX_TERMINAL_INPUT_BYTES, TerminalBackendKind, TerminalInputResult, TerminalProcessManager,
    TerminalPtySize, TerminalReadResult, TerminalResizeResult, TerminalStartRequest,
    TerminalTaskArtifacts, TerminalTaskPermissionContext,
};
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
#[path = "tests/lib_tests.rs"]
mod tests;
