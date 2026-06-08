use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-schema-backed tool contract exposed to model providers and UI approvals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

/// Execution context shared with tools at runtime.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub timeout_secs: u64,
}

/// Normalized tool execution result returned to the agent loop and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
    pub metadata: ToolResultMeta,
}

/// Human-readable preview shown before a mutating tool is approved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreview {
    pub title: String,
    pub summary: String,
    pub body: String,
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub file_diffs: Vec<ToolPreviewFile>,
}

/// Per-file diff section within a tool preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewFile {
    pub path: String,
    pub diff: String,
}

/// Additional structured metadata emitted by a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub struct ToolResultMeta {
    pub exit_code: Option<i32>,
    pub changed_files: Vec<String>,
    pub truncated: bool,
    pub bytes: Option<u64>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's stable contract and JSON Schema surface.
    fn spec(&self) -> ToolSpec;

    /// Returns the stable permission subject for one tool call, when the tool naturally targets
    /// one file- or path-like resource.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable subject can be derived.
    fn permission_subject(&self, _args: &Value) -> Result<Option<String>> {
        Ok(None)
    }

    /// Produces an optional approval preview for the given tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when preview materialization fails and the caller should surface
    /// that failure instead of silently fabricating a preview.
    async fn preview(&self, _ctx: ToolContext, _args: Value) -> Result<Option<ToolPreview>> {
        Ok(None)
    }

    /// Executes the tool call within the provided workspace context.
    ///
    /// # Errors
    ///
    /// Returns an error when arguments are invalid or the underlying tool action fails.
    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult>;
}

/// Runtime registry for built-in and remote tools.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Creates an empty tool registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one tool by its stable spec name, replacing any prior entry with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name.clone(), tool);
    }

    /// Returns the full list of registered tool specifications.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    /// Returns one registered spec by name.
    pub fn spec_for(&self, name: &str) -> Option<ToolSpec> {
        self.tools.get(name).map(|tool| tool.spec())
    }

    /// Executes a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool fails.
    pub async fn execute(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| anyhow!("unknown tool {}", call.name))?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.execute(ctx, call.id, args).await
    }

    /// Builds a preview for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or preview
    /// generation itself fails.
    pub async fn preview(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreview>> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| anyhow!("unknown tool {}", call.name))?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.preview(ctx, args).await
    }

    /// Returns the stable permission subject for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the JSON arguments are invalid.
    pub fn permission_subject(&self, call: &crate::provider::ToolCall) -> Result<Option<String>> {
        let tool = self
            .tools
            .get(&call.name)
            .ok_or_else(|| anyhow!("unknown tool {}", call.name))?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.permission_subject(&args)
    }
}
