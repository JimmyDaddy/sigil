use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentApprovalRouteEntry, AgentInvocationMode, AgentInvocationSource,
    AgentMailboxMessageEntry, AgentMailboxStatus, AgentProfileId, AgentRole, AgentRouteId,
    AgentRouteStatus, AgentRunOptions, AgentRunOutcome, AgentThreadClosedEntry, AgentThreadId,
    AgentThreadMessageRoutedEntry, AgentThreadProjection, AgentThreadResult,
    AgentThreadResultDeliveredEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentThreadTerminalStatus, AgentToolDelegate, AgentTrustState, AgentUsageSummary,
    ApprovalHandler, ApprovalMode, ControlEntry, EventHandler, FinalAnswerContext,
    JsonlSessionStore, ModelMessage, PermissionConfig, PermissionMode, Provider, RootConfig,
    RunEvent, Session, SessionLogEntry, SessionRef, TaskChildSessionStatus, TaskId, Tool,
    ToolAccess, ToolApproval, ToolApprovalAllowSource, ToolApprovalAuditAction,
    ToolApprovalUserDecision, ToolCall, ToolCategory, ToolContext, ToolErrorKind,
    ToolExecutionStatus, ToolPreview, ToolPreviewCapability, ToolRegistry, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, saturating_elapsed,
};

use crate::{
    AgentBudgetPolicy, AgentMailboxMessage, AgentProfileRegistry, AgentSupervisor,
    ResolvedAgentProfile, WORKER_PROFILE_ID,
    agent_supervisor::{AgentResultMaterialization, materialize_child_agent_final_answer},
    build_role_provider, build_role_run_options, build_role_tool_registry,
    chat_agent_thread_id_for_call,
};

pub const SPAWN_AGENT_TOOL_NAME: &str = "spawn_agent";
pub const WAIT_AGENT_TOOL_NAME: &str = "wait_agent";
pub const READ_AGENT_RESULT_TOOL_NAME: &str = "read_agent_result";
pub const MESSAGE_AGENT_TOOL_NAME: &str = "message_agent";
pub const CLOSE_AGENT_TOOL_NAME: &str = "close_agent";

const MAIN_THREAD_ID: &str = "main";
const DEFAULT_RESULT_SUMMARY_LIMIT: usize = 4_000;
const MIN_RESULT_SUMMARY_LIMIT: usize = 200;
const MAX_RESULT_PAGE_LIMIT: usize = 40_000;
const DEFAULT_RESULT_PAGE_LIMIT: usize = MAX_RESULT_PAGE_LIMIT;
const WAIT_AGENT_BACKGROUND_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WAIT_AGENT_FOREGROUND_WAIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const WAIT_AGENT_RUNNING_RETRY_AFTER_MS: u64 = 30 * 60 * 1_000;
const WAIT_AGENT_BACKGROUND_WAIT_TIMEOUT: Duration = WAIT_AGENT_FOREGROUND_WAIT_TIMEOUT;
const WAIT_AGENT_MIN_REPOLL_INTERVAL: Duration = WAIT_AGENT_FOREGROUND_WAIT_TIMEOUT;

mod background;
mod chat;
mod handlers;
mod permissions;
mod result_pages;
mod runtime;
mod shared;
mod spawn;
mod surface;

pub use background::{AgentToolBackgroundEventSink, AgentToolBackgroundRuns};
pub use runtime::{AgentToolProviderFactory, AgentToolRuntime, ManualAgentInvocationResult};
pub use surface::{
    close_agent_thread, register_agent_tools, register_agent_tools_with_registry,
    register_agent_tools_with_workspace, register_agent_tools_with_workspace_and_entries,
};

use background::{
    BackgroundChatAgentHandle, BackgroundChatAgentThreadRecord, run_background_chat_agent,
};
use chat::close_agent_from_args;
#[cfg(test)]
use chat::wait_throttle_remaining_since;
use handlers::{
    BackgroundApprovalHandler, ChatAgentApprovalRouteHandler, ChatChildEventHandler,
    ChatChildThreadGuard,
};
use permissions::{
    effective_child_permission_config, tool_scope_is_safe_readonly_for_auto_spawn,
    tool_scope_summary,
};
use result_pages::{
    agent_result_already_delivered_tool_result, agent_result_page_tool_result,
    agent_result_tool_result, agent_spawn_denied_tool_result, agent_status_tool_result,
    agent_wait_throttled_tool_result, read_agent_result_page, required_result_page_request_arg,
};
use shared::{
    agent_child_session_ref, agent_profile_system_prompt, agent_route_id_for_call, bounded_summary,
    build_agent_child_session, chat_budget_scope_id, child_status_from_outcome, hash_text,
    invocation_mode_label, manual_agent_call_id, optional_string, parent_session_ref,
    parse_invocation_mode, parse_tool_args, profile_index_description, required_string,
    role_for_profile_id, simple_agent_preview, terminal_status_label, thread_id_arg,
    thread_status_label, unix_time_ms, usage_summary_from_stats,
};
use surface::{AgentToolKind, ChatAgentRunRequest, SpawnAgentArgs};

#[cfg(test)]
#[path = "tests/agent_tools_tests.rs"]
mod tests;
