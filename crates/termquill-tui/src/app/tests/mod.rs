mod approval_flow_tests;
mod common;
mod config_flow_tests;
mod input_flow_tests;
mod modal_flow_tests;
mod session_flow_tests;
mod slash_flow_tests;
mod timeline_flow_tests;
mod tool_focus_tests;
mod worker_bridge_tests;

use std::path::Path;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use tempfile::tempdir;
use termquill_kernel::{
    AgentConfig, ApprovalMode, CompactionConfig, CompactionRecord, ControlEntry, EventHandler,
    JsonlSessionStore, MemoryConfig, ModelMessage, PermissionConfig, ReasoningEffort, RootConfig,
    RunEvent, SessionConfig, SessionLogEntry, ToolCall, ToolPreview, ToolSpec, UsageStats,
    WorkspaceConfig,
};

use crate::runner::{CompactionTrigger, WorkerCommand, WorkerMessage};
use crate::slash::SLASH_COMMANDS;

use super::{
    AppAction, AppState, ApprovalDiffLineKind, ConfigField, ConfigSection, ModalState,
    ModelPickerRefresh, ModelPickerTarget, PaneFocus, RunPhase, SessionHistoryRow, SessionViewMode,
    SetupField, TimelineRole,
};

use common::*;
