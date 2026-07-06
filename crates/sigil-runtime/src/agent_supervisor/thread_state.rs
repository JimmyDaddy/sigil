use std::{collections::BTreeMap, path::PathBuf, sync::mpsc};

use sigil_kernel::{
    AgentInvocationMode, AgentInvocationSource, AgentProfileId, AgentRole, AgentRouteId,
    AgentRunAttemptId, AgentRunInput, AgentThreadId, ProviderCapabilities, SessionRef, TaskId,
    TaskStepSpec,
};

/// Result of cancelling only the foreground parent run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForegroundCancelImpact {
    pub foreground_children_interrupted: Vec<AgentInterruptedThread>,
    pub background_children_cancelled: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInterruptedThread {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
}

#[derive(Debug, Default)]
pub(super) struct AgentSupervisorState {
    pub(super) active_threads: BTreeMap<AgentThreadId, ActiveAgentThread>,
    pub(super) task_token_usage: BTreeMap<TaskId, u64>,
}

#[derive(Debug, Clone)]
pub(super) struct ActiveAgentThread {
    pub(super) profile_id: AgentProfileId,
    pub(super) attempt_id: AgentRunAttemptId,
    pub(super) background: bool,
    pub(super) mailbox_tx: Option<mpsc::Sender<AgentMailboxMessage>>,
}

#[derive(Debug, Clone)]
pub struct AgentMailboxMessage {
    pub route_id: AgentRouteId,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct AgentTaskChildStart {
    pub task_id: TaskId,
    pub parent_thread_id: AgentThreadId,
    pub parent_depth: usize,
    pub parent_session_ref: SessionRef,
    pub plan_version: u32,
    pub step: TaskStepSpec,
    pub child_task_id: TaskId,
    pub child_session_ref: SessionRef,
    pub child_input: AgentRunInput,
    pub objective: String,
    pub workspace_root: PathBuf,
    pub provider_capabilities: ProviderCapabilities,
    pub role: AgentRole,
    pub invocation_mode: AgentInvocationMode,
    pub invocation_source: AgentInvocationSource,
}

#[derive(Debug, Clone)]
pub struct AgentTaskChildThread {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub profile_id: AgentProfileId,
    pub parent_thread_id: AgentThreadId,
}

#[derive(Debug, Clone)]
pub struct AgentChatChildStart {
    pub call_id: String,
    pub budget_scope_id: TaskId,
    pub parent_thread_id: AgentThreadId,
    pub parent_depth: usize,
    pub parent_session_ref: SessionRef,
    pub profile_id: AgentProfileId,
    pub role: AgentRole,
    pub child_session_ref: SessionRef,
    pub objective: String,
    pub prompt: String,
    pub workspace_root: PathBuf,
    pub provider_capabilities: ProviderCapabilities,
    pub invocation_mode: AgentInvocationMode,
    pub invocation_source: AgentInvocationSource,
    pub display_name_hint: Option<String>,
}

#[derive(Debug)]
pub struct AgentChatChildThread {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub profile_id: AgentProfileId,
    pub parent_thread_id: AgentThreadId,
    pub child_session_ref: SessionRef,
    pub budget_scope_id: TaskId,
    pub mailbox_rx: Option<mpsc::Receiver<AgentMailboxMessage>>,
}
