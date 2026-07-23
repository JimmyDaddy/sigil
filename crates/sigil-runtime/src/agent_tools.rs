use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentApprovalRouteEntry, AgentBatchId, AgentInvocationMode, AgentInvocationSource,
    AgentMailboxMessageEntry, AgentMailboxStatus, AgentProfileId, AgentProfileSource,
    AgentResultContinuationEntry, AgentResultContinuationStatus, AgentRole, AgentRouteId,
    AgentRouteStatus, AgentRunInterruptedEntry, AgentRunOptions, AgentRunOutcome,
    AgentThreadClosedEntry, AgentThreadId, AgentThreadMessageRoutedEntry, AgentThreadProjection,
    AgentThreadResult, AgentThreadResultDeliveredEntry, AgentThreadStatus,
    AgentThreadStatusChangedEntry, AgentThreadTerminalStatus, AgentToolDelegate, AgentTrustState,
    AgentUsageSummary, ApprovalHandler, ApprovalMode, ChangeSet, ControlEntry,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DelegationAuthority, EventHandler, FileType,
    FinalAnswerContext, IsolatedChangeSetProduced, JsonlSessionStore, MergeReviewId,
    MergeReviewRequested, ModelMessage, MultiAgentMode, MutationSubject, PermissionConfig,
    PermissionMode, Provider, RootConfig, RunCancellationFinalizedEntry, RunCancellationOwner,
    RunCancellationRequestedEntry, RunCancellationTarget, RunCancellationTerminalOutcome, RunEvent,
    RunQuiescenceOutcome, Session, SessionLogEntry, SessionRef, TaskChildSessionStatus, TaskId,
    Tool, ToolAccess, ToolApproval, ToolApprovalAllowSource, ToolApprovalAuditAction,
    ToolApprovalUserDecision, ToolCall, ToolCategory, ToolContext, ToolErrorKind,
    ToolExecutionStatus, ToolPreview, ToolPreviewCapability, ToolRegistry, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, VerificationScope, WriteIsolationMode,
    build_workspace_snapshot_for_event, changeset_only_child_contract_prompt,
    changeset_only_child_tool_registry, decode_changeset_only_child_output, saturating_elapsed,
    stable_event_uuid, stable_workspace_id,
};

use crate::{
    AgentBudgetPolicy, AgentMailboxMessage, AgentProfileRegistry, AgentSupervisor,
    ResolvedAgentProfile,
    agent_supervisor::{AgentResultMaterialization, materialize_child_agent_final_answer},
    build_role_provider, build_role_run_options, build_role_tool_registry,
    chat_agent_thread_id_for_call,
};

pub const SPAWN_AGENT_TOOL_NAME: &str = "spawn_agent";
pub const SPAWN_AGENTS_TOOL_NAME: &str = "spawn_agents";
pub const WAIT_AGENT_TOOL_NAME: &str = "wait_agent";
pub const READ_AGENT_RESULT_TOOL_NAME: &str = "read_agent_result";
pub const LIST_AGENTS_TOOL_NAME: &str = "list_agents";
pub const CANCEL_AGENT_TOOL_NAME: &str = "cancel_agent";
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

fn tool_batch_allows_host_join(calls: &[ToolCall]) -> bool {
    !calls.is_empty()
        && calls.iter().all(|call| match call.name.as_str() {
            SPAWN_AGENT_TOOL_NAME => serde_json::from_str::<Value>(&call.args_json)
                .ok()
                .and_then(|args| SpawnAgentArgs::parse(&args).ok())
                .is_some_and(|args| matches!(args.mode, AgentInvocationMode::JoinBeforeFinal)),
            SPAWN_AGENTS_TOOL_NAME => serde_json::from_str::<Value>(&call.args_json)
                .ok()
                .and_then(|args| SpawnAgentsArgs::parse(&args).ok())
                .is_some(),
            _ => false,
        })
}

mod background;
mod batch_spawn;
mod chat;
mod completion;
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
    register_agent_tools_with_registry_and_mode, register_agent_tools_with_workspace,
    register_agent_tools_with_workspace_and_entries,
};

type JoinedChatAgentFuture =
    Pin<Box<dyn Future<Output = Result<background::BackgroundChatAgentResult>> + Send>>;

use background::{
    AgentBatchMemberContext, BackgroundChatAgentHandle, BackgroundChatAgentThreadRecord,
    JoinedChatAgentHandle, run_background_chat_agent,
};
use chat::close_agent_from_args;
#[cfg(test)]
use chat::wait_throttle_remaining_for_elapsed;
use completion::append_agent_result_continuation;
use handlers::{
    BackgroundApprovalHandler, ChatAgentApprovalRouteHandler, ChatChildEventHandler,
    ChatChildThreadGuard,
};
pub(crate) use permissions::tool_registry_is_safe_readonly_for_auto_spawn;
use permissions::{
    admit_model_agent_spawn, apply_child_permission_constraints, delegation_admission_entry,
    tool_contracts_are_safe_readonly_for_auto_spawn, tool_scope_summary,
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
    short_digest, simple_agent_preview, terminal_status_label, thread_id_arg, thread_status_label,
    unix_time_ms, usage_summary_from_stats,
};
use surface::{AgentToolKind, ChatAgentRunRequest, SpawnAgentArgs, SpawnAgentsArgs};

#[cfg(test)]
#[path = "tests/agent_tools_tests.rs"]
mod tests;
