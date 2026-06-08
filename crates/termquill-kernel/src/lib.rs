pub mod agent;
pub mod approval;
pub mod config;
pub mod event;
pub mod memory;
pub mod permission;
pub mod provider;
pub mod session;
pub mod tool;

pub use agent::{Agent, AgentError, AgentRunOptions, AgentRunResult};
pub use approval::{ApprovalHandler, AutoApproveHandler, ToolApproval};
pub use config::{
    AgentConfig, CompactionConfig, CompactionThresholdStatus, McpServerConfig, MemoryConfig,
    RootConfig, SessionConfig, WorkspaceConfig, default_user_config_dir, default_user_config_path,
    preferred_config_path, resolve_workspace_root,
};
pub use event::{EventHandler, NoopEventHandler, RunEvent};
pub use memory::{MemoryLoadReport, inspect_memory_documents};
pub use permission::{
    ApprovalMode, InteractionMode, PermissionConfig, PermissionDecision, PermissionPolicy,
    PermissionRule,
};
pub use provider::{
    BackgroundTaskHandle, BackgroundTaskStatus, CompletionRequest, MessageRole, ModelMessage,
    PrefixSnapshot, Provider, ProviderCapabilities, ProviderChunk, ProviderContinuationState,
    ReasoningArtifact, ReasoningEffort, ResponseHandle, SessionStats, ToolCall, UsageStats,
};
pub use session::{
    CompactionPreview, CompactionRecord, ControlEntry, JsonlSessionStore, Session, SessionLogEntry,
    latest_compaction_record, session_stats_from_entries,
};
pub use tool::{
    Tool, ToolContext, ToolPreview, ToolPreviewFile, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec,
};
