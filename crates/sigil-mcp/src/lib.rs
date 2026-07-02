use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionSandboxProfile, McpServerConfig, McpServerPinnedIdentity,
    McpServerStartup, McpServerTrustPolicy, MutationEventRecorder, ProviderCapabilities,
    SecretRedactor, Tool, ToolAccess, ToolCategory, ToolContext, ToolEffect, ToolEgressAudit,
    ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, VerificationScope, WorkspaceMutationScan,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    task::JoinHandle,
};
use tracing::warn;

mod client; // JSON-RPC connection state and request/response flow.
mod elicitation; // client-mediated elicitation request/response mapping.
mod events; // runtime progress and list-change event shapes.
mod lifecycle; // server startup, activation, and registry reporting.
mod name; // provider-visible MCP tool name normalization.
mod output; // bounded MCP tool output and egress summaries.
mod process; // local process launch contracts and stderr handling.
mod prompts; // prompt-backed MCP tool adapter.
mod resources; // resource-backed MCP tool adapter.
mod roots; // workspace root URI/name helpers.
mod tools; // remote tool adapter and command fingerprinting.

const DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS: usize = 64;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const MCP_OUTPUT_LIMIT_LINES: usize = 2_000;

use client::{McpClient, McpServerObservedIdentity};
use elicitation::mcp_elicitation_request;
use events::{mcp_list_changed_kind, mcp_progress_notification};
use output::{bounded_mcp_tool_result, summarize_egress_json};
use process::drain_mcp_stderr;
use prompts::{McpPromptTool, McpPromptToolKind};
use resources::{McpResourceTool, McpResourceToolKind};
use roots::{canonical_root, file_uri, root_name};
use tools::{McpTool, McpToolDescriptor, mcp_command_fingerprint};

#[cfg(test)]
use client::{read_message, validate_mcp_pin};
#[cfg(test)]
use lifecycle::capture_mcp_server_lifecycle_scan;
#[cfg(test)]
use name::{
    fit_provider_name_with_hash, provider_name_with_hash, sanitize_provider_name_part, stable_hash,
};
#[cfg(test)]
use output::{append_utf8_prefix, json_type_label, to_u64, truncate_text_budget};

pub use elicitation::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest, McpElicitationResponse,
    unsupported_mcp_elicitation_handler,
};
pub use events::{
    McpListChangedKind, McpListChangedNotification, McpProgressNotification,
    McpRuntimeEventHandler, unsupported_mcp_runtime_event_handler,
};
pub use lifecycle::{
    McpToolRegistrationOptions, McpToolRegistrationReport, activate_lazy_mcp_tools,
    register_mcp_tools, register_mcp_tools_with_options, register_mcp_tools_with_report,
};
pub use name::{McpToolName, mcp_provider_tool_name_prefix};
pub use process::{
    LocalMcpProcessLauncher, McpProcessClass, McpProcessCoverage, McpProcessLaunch,
    McpProcessLaunchReceipt, McpProcessLaunchRequest, McpProcessLauncher,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
