//! Bounded provider observations, excluded from model context and execution authority.

use serde::{Deserialize, Serialize};

/// Effective tool-schema enforcement for one materialized provider request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderToolSchemaModeV1 {
    Enabled,
    Disabled,
    FallbackUnsupportedSchema,
}

/// Closed finish classes; adapters must not copy arbitrary wire strings into diagnostics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStreamFinishV1 {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Other,
    Missing,
}

/// Count-only diagnostics attached to the physical provider attempt that produced them.
/// These observations do not certify generation, tool completion, or task evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderDiagnosticV1 {
    ToolsPrepared {
        schema_mode: ProviderToolSchemaModeV1,
        tool_count: u64,
    },
    ToolStreamFinished {
        finish: ProviderStreamFinishV1,
        tool_call_count: u64,
        argument_fragments: u64,
        argument_bytes: u64,
        completed_calls: u64,
    },
}
