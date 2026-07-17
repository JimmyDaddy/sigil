use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, EXTENSION_ENVIRONMENT_POLICY_VERSION,
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionNetworkReceipt,
    ExecutionSandboxProfile, ExtensionProcessLaunchError, ExtensionProcessLaunchPhase,
    ExtensionProcessLifecycleAudit, ExtensionProcessLifecycleStatus, McpServerConfig,
    McpServerPinnedIdentity, McpServerStartup, McpServerTrustPolicy, MutationEventRecorder,
    NetworkEffect, NetworkPolicy, ProcessEnvironmentPolicy, ProviderCapabilities,
    ResolvedProcessEnvironment, SecretRedactor, Tool, ToolAccess, ToolCategory, ToolContext,
    ToolEffect, ToolEgressAudit, ToolErrorKind, ToolLifecycleOwner, ToolOperation,
    ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
    VerificationScope, WorkspaceMutationScan, resolve_extension_process_environment,
    validate_extension_process_network_admission,
};
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    task::JoinHandle,
};
use tracing::warn;
use uuid::Uuid;

mod client; // JSON-RPC connection state and request/response flow.
mod elicitation; // client-mediated elicitation request/response mapping.
mod error; // typed connection, framing, and deadline failures.
mod events; // runtime progress and list-change event shapes.
mod framing; // bounded newline-delimited JSON stdio framing.
mod lifecycle; // server startup, activation, and registry reporting.
mod name; // provider-visible MCP tool name normalization.
mod output; // bounded MCP tool output and egress summaries.
mod process; // local process launch contracts and stderr handling.
mod process_group; // direct Unix process-group signalling and liveness checks.
mod prompts; // prompt-backed MCP tool adapter.
mod resources; // resource-backed MCP tool adapter.
mod roots; // workspace root URI/name helpers.
mod search_binding; // stable search eligibility over exact MCP identity and schema.
mod streamable_http; // internal-only MCP Streamable HTTP protocol core.
mod tools; // remote tool adapter and command fingerprinting.

const DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS: usize = 64;
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
// Keep MCP ToolResult content at or below the kernel's model-visible content ceiling. Truncation
// is carried only in structured metadata, so no post-redaction free-text marker is introduced.
const MCP_OUTPUT_LIMIT_BYTES: usize = 32 * 1024;
const MCP_OUTPUT_LIMIT_LINES: usize = 2_000;
const DEFAULT_MCP_OPERATION_TIMEOUT_SECS: u64 = 30;
const MAX_MCP_OPERATION_TIMEOUT_SECS: u64 = 24 * 60 * 60;
const MCP_OPERATION_MESSAGE_LIMIT: usize = 256;
const MCP_OPERATION_CUMULATIVE_BYTES_LIMIT: usize = 8 * 1024 * 1024;

use client::{McpClient, McpOperationDeadline, McpServerObservedIdentity};
use elicitation::mcp_elicitation_request;
use error::{McpCleanupEvidence, McpClientError, McpTerminalCause};
use events::{mcp_list_changed_kind, mcp_progress_notification};
#[cfg(test)]
use framing::read_ndjson_message;
use framing::{
    McpFrame, McpFramingError, read_ndjson_message_with_wire_limit, write_ndjson_message,
};
use output::{
    bounded_mcp_destination, bounded_mcp_identity_projection, bounded_mcp_json,
    bounded_mcp_metadata_text, bounded_mcp_protocol_error, bounded_mcp_text,
    bounded_mcp_text_segments, bounded_mcp_tool_result, secret_safe_mcp_metadata,
    summarize_egress_json,
};
use process::{
    McpProcessCleanupSummary, McpStderrFault, McpStderrSummary, drain_mcp_stderr,
    terminate_mcp_process,
};
use prompts::{McpPromptTool, McpPromptToolKind};
use resources::{McpResourceTool, McpResourceToolKind};
use roots::{canonical_root, file_uri, root_name};
use tools::{McpTool, McpToolDescriptor};

#[cfg(test)]
use tools::mcp_transport_fingerprint;

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
    MCP_TOOL_LIFECYCLE_NAMESPACE, McpToolRegistrationOptions, McpToolRegistrationReport,
    activate_lazy_mcp_tools, register_mcp_tools, register_mcp_tools_with_options,
    register_mcp_tools_with_report,
};
pub use name::{McpToolName, mcp_provider_tool_name_candidate, mcp_provider_tool_name_prefix};
pub use process::{
    LocalMcpProcessLauncher, McpDeclarationLaunchMetadata, McpProcessClass, McpProcessCoverage,
    McpProcessLaunch, McpProcessLaunchReceipt, McpProcessLaunchRequest, McpProcessLauncher,
};
pub use search_binding::{
    KnownMcpSearchAdapter, McpSearchAdapterKind, McpSearchIncompatibility,
    McpStableSearchEligibility, classify_mcp_search_binding, mcp_schema_fingerprint,
    mcp_tool_schema_fingerprint,
};
pub use sigil_kernel::ExtensionProcessNetworkAdmission;
pub use streamable_http::{
    CompiledMcpSchema, McpCallToolResult, McpOAuthAuthorizationCode, McpOAuthChallenge,
    McpOAuthClientIntent, McpOAuthClientRegistration, McpOAuthDiscovery, McpOAuthHttpExecutor,
    McpOAuthHttpMethod, McpOAuthHttpPurpose, McpOAuthHttpRequest, McpOAuthHttpResponse,
    McpOAuthLoopbackListener, McpOAuthPendingAuthorization, McpOAuthProtocolError,
    McpOAuthResource, McpOAuthTokenResponse, McpOAuthTransportError, McpRemoteClientCapabilities,
    McpRemoteFormField, McpRemoteFormFieldKind, McpRemoteFormHandler, McpRemoteFormResponse,
    McpRemoteProtocolVersion, McpRemoteRoot, McpRemoteServerIdentity, McpRemoteTool,
    McpRequestBodyObserver, McpStreamableHttpAuthState, McpStreamableHttpAuthorizedDialPlan,
    McpStreamableHttpClient, McpStreamableHttpDestinationAuthorizer,
    McpStreamableHttpDestinationError, McpStreamableHttpError, McpStreamableHttpHeaderConfig,
    McpStreamableHttpHeaderEnvironment, McpStreamableHttpLifecycle, McpStreamableHttpLimits,
    McpStreamableHttpRoute, McpStreamableHttpRouteEvidence, PreparedMcpStreamableHttpHeaders,
    ValidatedMcpFormRequest, discover_oauth_authorization_server,
    exchange_oauth_authorization_code, prepare_oauth_client,
};
pub use tools::{
    mcp_launch_static_fingerprint, mcp_launch_static_fingerprint_at,
    mcp_resolved_launch_static_fingerprint_at, mcp_transport_static_fingerprint,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
