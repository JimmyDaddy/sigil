pub mod agent;
pub mod approval;
pub mod config;
pub mod event;
pub mod memory;
pub mod permission;
pub mod provider;
pub mod secret;
pub mod session;
pub mod tool;

pub use agent::{Agent, AgentRunOptions, AgentRunResult};
pub use approval::{ApprovalHandler, AutoApproveHandler, ToolApproval};
pub use config::{
    AgentConfig, CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig,
    CompactionConfig, CompactionThresholdStatus, LanguageServerConfig, McpServerConfig,
    McpServerStartup, McpServerTrustPolicy, McpTrustClass, MemoryConfig, RootConfig, SessionConfig,
    WorkspaceConfig, default_user_config_dir, default_user_config_path, preferred_config_path,
    resolve_workspace_root,
};
pub use event::{EventHandler, NoopEventHandler, RunEvent};
pub use memory::{MemoryLoadReport, inspect_memory_documents};
pub use permission::{
    ApprovalMode, ExternalDirectoryConfig, ExternalDirectoryRule, InteractionMode,
    PermissionAccessConfig, PermissionConfig, PermissionDecision, PermissionPolicy, PermissionRule,
};
pub use provider::{
    BackgroundTaskHandle, BackgroundTaskStatus, CompletionRequest, MessageRole, ModelMessage,
    PrefixSnapshot, Provider, ProviderCapabilities, ProviderChunk, ProviderContinuationState,
    ReasoningArtifact, ReasoningEffort, ResponseHandle, SessionStats, ToolCall, UsageStats,
};
pub use secret::{REDACTED_SECRET, SecretRedactor};
pub use session::{
    CompactionPreview, CompactionRecord, ControlEntry, JsonlSessionStore, Session, SessionLogEntry,
    ToolApprovalAuditAction, ToolApprovalEntry, ToolApprovalUserDecision, ToolEgressEntry,
    ToolExecutionEntry, ToolExecutionStatus, ToolSubjectAudit, latest_compaction_record,
    session_stats_from_entries,
};
pub use tool::{
    Tool, ToolAccess, ToolCategory, ToolContext, ToolDiffBudget, ToolDiffStats, ToolEgressAudit,
    ToolError, ToolErrorKind, ToolPreview, ToolPreviewCapability, ToolPreviewFile,
    ToolPreviewFileSnapshot, ToolPreviewSnapshot, ToolRegistry, ToolResult, ToolResultMeta,
    ToolResultStatus, ToolResultSummary, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
};
