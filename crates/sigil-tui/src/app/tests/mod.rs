#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod app_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod approval_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod command_dispatch_tests;
pub(crate) mod common;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod config_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod formatting_tests;
mod input_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod modal_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod mouse_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod performance_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod product_smoke_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod session_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod session_review_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod setup_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod slash_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod timeline_flow_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod timeline_render_store_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod tool_card_interaction_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod worker_bridge_tests;
#[cfg(not(sigil_tui_test_slice_app_input_flow))]
mod workspace_trust_flow_tests;

use std::path::Path;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, ApprovalMode, AssistantMessageKind, CompactionConfig, ControlEntry, EventHandler,
    JsonlSessionStore, McpServerStartup, MemoryConfig, ModelMessage, PermissionConfig,
    ReasoningEffort, RootConfig, RunEvent, SessionConfig, SessionLogEntry, ToolAccess, ToolCall,
    ToolCategory, ToolEgressEntry, ToolError, ToolErrorKind, ToolExecutionEntry,
    ToolExecutionStatus, ToolPreview, ToolPreviewCapability, ToolPreviewSnapshot, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectAudit, ToolSubjectKind, ToolSubjectScope,
    UsageStats, WorkspaceConfig,
};
use sigil_runtime::{McpElicitationAction, McpElicitationRequest};
use tempfile::tempdir;

use crate::config_panel::{ConfigField, ConfigFooterAction, ConfigSection};
use crate::runner::{McpActivationStatus, WorkerCommand, WorkerMessage};
use crate::slash::SLASH_COMMANDS;

use super::modal_flow::ModelPickerTarget;
use super::{
    AppAction, AppState, ApprovalAction, ApprovalDiagnosticSummary, ApprovalDiffLineKind,
    ModalState, ModelPickerRefresh, PaneFocus, RunPhase, SessionHistoryRow, SessionViewMode,
    SetupField, SidebarCard, TimelineRole,
};

use common::*;
