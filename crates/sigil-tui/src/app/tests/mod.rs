mod app_tests;
mod approval_flow_tests;
mod command_dispatch_tests;
pub(crate) mod common;
mod config_flow_tests;
mod formatting_tests;
mod input_flow_tests;
mod modal_flow_tests;
mod mouse_flow_tests;
mod performance_tests;
mod session_flow_tests;
mod setup_flow_tests;
mod slash_flow_tests;
mod timeline_flow_tests;
mod tool_focus_tests;
mod worker_bridge_tests;

use std::path::Path;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, ApprovalMode, CompactionConfig, CompactionRecord, ControlEntry, EventHandler,
    JsonlSessionStore, McpServerStartup, McpTrustClass, MemoryConfig, ModelMessage,
    PermissionConfig, ReasoningEffort, RootConfig, RunEvent, SessionConfig, SessionLogEntry,
    ToolAccess, ToolCall, ToolCategory, ToolEgressEntry, ToolError, ToolErrorKind,
    ToolExecutionEntry, ToolExecutionStatus, ToolPreview, ToolPreviewCapability,
    ToolPreviewSnapshot, ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectAudit,
    ToolSubjectKind, ToolSubjectScope, UsageStats, WorkspaceConfig,
};
use sigil_runtime::{McpElicitationAction, McpElicitationRequest};
use tempfile::tempdir;

use crate::config_panel::{ConfigField, ConfigFooterAction, ConfigSection};
use crate::runner::{CompactionTrigger, McpActivationStatus, WorkerCommand, WorkerMessage};
use crate::slash::SLASH_COMMANDS;

use super::modal_flow::ModelPickerTarget;
use super::{
    AppAction, AppState, ApprovalAction, ApprovalDiagnosticSummary, ApprovalDiffLineKind,
    ModalState, ModelPickerRefresh, PaneFocus, RunPhase, SessionHistoryRow, SessionViewMode,
    SetupField, SidebarCard, TimelineRole,
};

use common::*;
